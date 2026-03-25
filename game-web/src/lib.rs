#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_WIDTH, ChunkPos};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_time::Instant;
use web_sys::{HtmlCanvasElement, MessageEvent, Worker};
use wgpu_lite::{Mesh, Renderer, Vertex};
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::WindowExtWebSys;
use winit::window::WindowBuilder;

const WEB_RADIUS: i32 = 6;
const INITIAL_WEB_RADIUS: i32 = 1;
const CHUNK_WORLD_RADIUS: f32 = (CHUNK_WIDTH as f32) * 0.5;
const DRAW_DISTANCE_CHUNKS: f32 = 14.0;
const MESH_WORKER_COUNT: usize = 3;
const DEFAULT_GENERATION_BUDGET_PER_UPDATE: usize = 6;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    spawn_local(async {
        if let Err(error) = run().await {
            panic!("{error:?}");
        }
    });
}

async fn run() -> Result<()> {
    let event_loop = EventLoop::new()?;
    let window: &'static winit::window::Window = Box::leak(Box::new(
        WindowBuilder::new()
            .with_title("Augmego Voxel Sandbox Web")
            .build(&event_loop)?,
    ));

    attach_canvas(window.canvas().expect("winit web canvas"));

    let renderer = Renderer::new(window).await?;
    let (mesh_result_rx, workers, worker_onmessage) = start_mesh_worker_pool(MESH_WORKER_COUNT)?;
    let mut app = WebApp::new(renderer.size(), workers, worker_onmessage, mesh_result_rx);
    let mut renderer = renderer;
    let mut chunk_meshes = HashMap::new();

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    app.resize(size);
                }
                WindowEvent::KeyboardInput { event, .. } => app.handle_key(event),
                WindowEvent::RedrawRequested => {
                    app.tick();
                    app.process_generation_updates(&renderer, &mut chunk_meshes, DEFAULT_GENERATION_BUDGET_PER_UPDATE);
                    renderer.update_camera(app.camera_matrix());
                    let visible_meshes = chunk_meshes
                        .iter()
                        .filter_map(|(position, mesh)| app.chunk_is_visible(*position).then_some(mesh))
                        .collect::<Vec<_>>();

                    if let Err(error) = renderer.render(&visible_meshes, &[]) {
                        panic!("{error:?}");
                    }
                }
                _ => {}
            },
            Event::DeviceEvent { event: DeviceEvent::MouseMotion { delta }, .. } => {
                app.handle_mouse_motion(delta.0 as f32, delta.1 as f32);
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    })?;

    Ok(())
}

struct WebApp {
    camera: Camera,
    pressed: HashSet<KeyCode>,
    last_tick: Instant,
    size: PhysicalSize<u32>,
    current_chunk: ChunkPos,
    desired_chunks: HashSet<ChunkPos>,
    pending_generation: VecDeque<ChunkPos>,
    inflight_generation: HashSet<ChunkPos>,
    movement_active: bool,
    mesh_result_rx: Receiver<MeshBuildResult>,
    workers: Vec<Worker>,
    next_worker_index: usize,
    _worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
}

impl WebApp {
    fn new(
        size: PhysicalSize<u32>,
        workers: Vec<Worker>,
        worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
        mesh_result_rx: Receiver<MeshBuildResult>,
    ) -> Self {
        let mut camera = Camera::default();
        camera.position = Vec3::new(8.0, 98.0, 16.0);
        let current_chunk = chunk_from_world_position(camera.position);
        let desired_chunks = desired_chunk_set(current_chunk, WEB_RADIUS);
        let pending_generation =
            prioritize_chunks(desired_chunks.iter().copied().collect(), current_chunk, camera.position, camera.forward());

        Self {
            camera,
            pressed: HashSet::new(),
            last_tick: Instant::now(),
            size,
            current_chunk,
            desired_chunks,
            pending_generation,
            inflight_generation: HashSet::new(),
            movement_active: false,
            mesh_result_rx,
            workers,
            next_worker_index: 0,
            _worker_onmessages: worker_onmessages,
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
    }

    fn handle_key(&mut self, event: KeyEvent) {
        let code = match event.physical_key {
            PhysicalKey::Code(code) => code,
            _ => return,
        };

        match event.state {
            ElementState::Pressed => {
                self.pressed.insert(code);
            }
            ElementState::Released => {
                self.pressed.remove(&code);
            }
        }
    }

    fn handle_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.camera.yaw -= dx * 0.0025;
        self.camera.pitch = (self.camera.pitch - dy * 0.0025).clamp(-1.45, 1.45);
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick);
        self.last_tick = now;

        let mut movement = Vec3::ZERO;
        if self.pressed.contains(&KeyCode::KeyW) {
            movement.z -= 1.0;
        }
        if self.pressed.contains(&KeyCode::KeyS) {
            movement.z += 1.0;
        }
        if self.pressed.contains(&KeyCode::KeyA) {
            movement.x -= 1.0;
        }
        if self.pressed.contains(&KeyCode::KeyD) {
            movement.x += 1.0;
        }
        if self.pressed.contains(&KeyCode::Space) {
            movement.y += 1.0;
        }
        if self.pressed.contains(&KeyCode::ShiftLeft) {
            movement.y -= 1.0;
        }

        self.movement_active = movement != Vec3::ZERO;
        self.camera.update(dt, movement);
    }

    fn camera_matrix(&self) -> Mat4 {
        let aspect = self.size.width as f32 / self.size.height.max(1) as f32;
        self.camera.matrix(aspect)
    }

    fn chunk_is_visible(&self, position: ChunkPos) -> bool {
        let center = Vec3::new(
            position.x as f32 * CHUNK_WIDTH as f32 + CHUNK_WORLD_RADIUS,
            64.0,
            position.z as f32 * CHUNK_DEPTH as f32 + CHUNK_WORLD_RADIUS,
        );
        let to_chunk = center - self.camera.position;
        let distance = to_chunk.length();
        let max_distance = DRAW_DISTANCE_CHUNKS * CHUNK_WIDTH as f32;

        if distance > max_distance {
            return false;
        }

        if distance <= CHUNK_WIDTH as f32 * 2.0 {
            return true;
        }

        let direction = to_chunk / distance.max(0.001);
        let threshold = 0.1 - (24.0 / distance).min(0.25);
        self.camera.forward().dot(direction) >= threshold
    }

    fn process_generation_updates(
        &mut self,
        renderer: &Renderer<'_>,
        chunk_meshes: &mut HashMap<ChunkPos, Mesh>,
        default_budget: usize,
    ) {
        self.update_streaming_window(chunk_meshes);

        while let Ok(result) = self.mesh_result_rx.try_recv() {
            self.inflight_generation.remove(&result.position);
            if result.failed {
                if self.desired_chunks.contains(&result.position) {
                    self.pending_generation.push_back(result.position);
                }
                continue;
            }

            if self.desired_chunks.contains(&result.position) {
                chunk_meshes.insert(result.position, renderer.create_mesh(&result.vertices, &result.indices));
            }
        }

        self.reprioritize_pending_generation(chunk_meshes);

        let budget = self.generation_budget(default_budget);
        for _ in 0..budget {
            let Some(position) = self.pending_generation.pop_front() else {
                break;
            };

            if self.inflight_generation.contains(&position) {
                continue;
            }

            let worker_index = self.next_worker_index % self.workers.len();
            dispatch_mesh_job(&self.workers[worker_index], position);
            self.next_worker_index = (self.next_worker_index + 1) % self.workers.len();
            self.inflight_generation.insert(position);
        }
    }

    fn generation_budget(&self, default_budget: usize) -> usize {
        if self.pending_generation.is_empty() {
            return 0;
        }

        if self.movement_active {
            return default_budget.saturating_div(2).max(1);
        }

        default_budget
    }

    fn update_streaming_window(&mut self, chunk_meshes: &mut HashMap<ChunkPos, Mesh>) {
        let next_chunk = chunk_from_world_position(self.camera.position);
        if next_chunk == self.current_chunk {
            return;
        }

        self.current_chunk = next_chunk;
        self.desired_chunks = desired_chunk_set(self.current_chunk, WEB_RADIUS);
        chunk_meshes.retain(|position, _| self.desired_chunks.contains(position));
        self.pending_generation
            .retain(|position| self.desired_chunks.contains(position));
        self.inflight_generation
            .retain(|position| self.desired_chunks.contains(position));
    }

    fn reprioritize_pending_generation(&mut self, chunk_meshes: &HashMap<ChunkPos, Mesh>) {
        for position in &self.desired_chunks {
            if chunk_meshes.contains_key(position)
                || self.inflight_generation.contains(position)
                || self.pending_generation.contains(position)
            {
                continue;
            }

            self.pending_generation.push_back(*position);
        }

        if self.pending_generation.len() <= 1 {
            return;
        }

        let forward = self.camera.forward();
        let mut pending = self.pending_generation.drain(..).collect::<Vec<_>>();
        pending.sort_by(|a, b| {
            chunk_priority(*a, self.current_chunk, self.camera.position, forward)
                .total_cmp(&chunk_priority(*b, self.current_chunk, self.camera.position, forward))
        });
        self.pending_generation = pending.into();
    }
}

#[derive(Default)]
struct Camera {
    position: Vec3,
    yaw: f32,
    pitch: f32,
}

impl Camera {
    fn update(&mut self, dt: Duration, local_movement: Vec3) {
        let forward = Vec3::new(self.yaw.sin(), 0.0, self.yaw.cos()).normalize_or_zero();
        let right = Vec3::new(forward.z, 0.0, -forward.x);
        let speed = 18.0 * dt.as_secs_f32();
        self.position += (forward * -local_movement.z + right * local_movement.x + Vec3::Y * local_movement.y) * speed;
    }

    fn matrix(&self, aspect: f32) -> Mat4 {
        let look = self.forward();
        let view = Mat4::look_at_rh(self.position, self.position + look, Vec3::Y);
        let proj = Mat4::perspective_rh_gl(60.0_f32.to_radians(), aspect, 0.1, 1_500.0);
        proj * view
    }

    fn forward(&self) -> Vec3 {
        Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.cos() * self.pitch.cos(),
        )
        .normalize_or_zero()
    }
}

fn attach_canvas(canvas: HtmlCanvasElement) {
    let window = web_sys::window().expect("window");
    let document = window.document().expect("document");
    let body = document.body().expect("body");
    let _ = body.append_child(&canvas);
}

fn start_mesh_worker_pool(
    worker_count: usize,
) -> Result<(Receiver<MeshBuildResult>, Vec<Worker>, Vec<Closure<dyn FnMut(MessageEvent)>>)> {
    let (tx, rx) = mpsc::channel::<MeshBuildResult>();
    let mut workers = Vec::with_capacity(worker_count);
    let mut onmessages = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let worker = Worker::new("mesh-worker.js")
            .map_err(|error| anyhow::anyhow!("create mesh worker: {error:?}"))?;
        let tx = tx.clone();
        let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
            let data = event.data();
            let object = js_sys::Object::from(data);
            let kind = js_sys::Reflect::get(&object, &JsValue::from_str("kind"))
                .ok()
                .and_then(|value| value.as_string())
                .unwrap_or_default();
            let x = js_sys::Reflect::get(&object, &JsValue::from_str("x"))
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or_default() as i32;
            let z = js_sys::Reflect::get(&object, &JsValue::from_str("z"))
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or_default() as i32;

            if kind == "error" {
                let message = js_sys::Reflect::get(&object, &JsValue::from_str("message"))
                    .ok()
                    .and_then(|value| value.as_string())
                    .unwrap_or_else(|| "unknown worker error".to_string());
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "mesh worker failed for chunk ({x}, {z}): {message}"
                )));
                let _ = tx.send(MeshBuildResult {
                    position: ChunkPos { x, z },
                    vertices: Vec::new(),
                    indices: Vec::new(),
                    failed: true,
                });
                return;
            }

            let vertices_value = js_sys::Reflect::get(&object, &JsValue::from_str("vertices")).unwrap();
            let indices_value = js_sys::Reflect::get(&object, &JsValue::from_str("indices")).unwrap();
            let vertex_floats = js_sys::Float32Array::new(&vertices_value).to_vec();
            let indices = js_sys::Uint32Array::new(&indices_value).to_vec();
            let vertices = vertex_floats
                .chunks_exact(8)
                .map(|values| Vertex {
                    position: [values[0], values[1], values[2]],
                    color: [values[3], values[4], values[5]],
                    uv: [values[6], values[7]],
                })
                .collect::<Vec<_>>();

            let _ = tx.send(MeshBuildResult {
                position: ChunkPos { x, z },
                vertices,
                indices,
                failed: false,
            });
        }) as Box<dyn FnMut(MessageEvent)>);

        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        workers.push(worker);
        onmessages.push(onmessage);
    }

    Ok((rx, workers, onmessages))
}

fn dispatch_mesh_job(worker: &Worker, position: ChunkPos) {
    let job = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("kind"), &JsValue::from_str("build"));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("x"), &JsValue::from_f64(f64::from(position.x)));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("z"), &JsValue::from_f64(f64::from(position.z)));
    let _ = worker.post_message(&job);
}

fn ordered_chunk_positions(radius: i32) -> Vec<ChunkPos> {
    let mut positions = Vec::new();
    positions.push(ChunkPos { x: 0, z: 0 });

    for ring in 1..=radius {
        for z in -ring..=ring {
            for x in -ring..=ring {
                if x.abs().max(z.abs()) != ring {
                    continue;
                }

                positions.push(ChunkPos { x, z });
            }
        }
    }

    positions
}

fn chunk_from_world_position(position: Vec3) -> ChunkPos {
    ChunkPos {
        x: (position.x / CHUNK_WIDTH as f32).floor() as i32,
        z: (position.z / CHUNK_DEPTH as f32).floor() as i32,
    }
}

fn desired_chunk_set(center: ChunkPos, radius: i32) -> HashSet<ChunkPos> {
    ordered_chunk_positions(radius)
        .into_iter()
        .map(|offset| ChunkPos {
            x: center.x + offset.x,
            z: center.z + offset.z,
        })
        .collect()
}

fn prioritize_chunks(
    positions: Vec<ChunkPos>,
    current_chunk: ChunkPos,
    camera_position: Vec3,
    forward: Vec3,
) -> VecDeque<ChunkPos> {
    let mut positions = positions;
    positions.sort_by(|a, b| {
        chunk_priority(*a, current_chunk, camera_position, forward)
            .total_cmp(&chunk_priority(*b, current_chunk, camera_position, forward))
    });

    let mut pending = VecDeque::new();
    for position in positions {
        if (position.x - current_chunk.x).abs() <= INITIAL_WEB_RADIUS
            && (position.z - current_chunk.z).abs() <= INITIAL_WEB_RADIUS
        {
            pending.push_front(position);
        } else {
            pending.push_back(position);
        }
    }

    pending
}

fn chunk_priority(position: ChunkPos, camera_chunk: ChunkPos, camera_position: Vec3, forward: Vec3) -> f32 {
    let dx = (position.x - camera_chunk.x) as f32;
    let dz = (position.z - camera_chunk.z) as f32;
    let distance_sq = dx * dx + dz * dz;
    let center = Vec3::new(
        position.x as f32 * CHUNK_WIDTH as f32 + CHUNK_WORLD_RADIUS,
        camera_position.y,
        position.z as f32 * CHUNK_DEPTH as f32 + CHUNK_WORLD_RADIUS,
    );
    let to_chunk = (center - camera_position).normalize_or_zero();
    let forward_bias = 1.0 - forward.dot(to_chunk);

    distance_sq + forward_bias * 2.5
}

struct MeshBuildResult {
    position: ChunkPos,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
    failed: bool,
}
