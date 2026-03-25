#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, WorldPos};
use shared_world::{BlockId, ChunkData, TerrainGenerator};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_time::Instant;
use web_sys::{Document, HtmlCanvasElement, MessageEvent, Worker};
use wgpu_lite::{Mesh, Renderer, Vertex};
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, MouseButton, WindowEvent};
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
const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_HEIGHT: f32 = 1.8;
const PLAYER_EYE_HEIGHT: f32 = 1.62;
const PLAYER_WALK_SPEED: f32 = 7.5;
const PLAYER_SPRINT_SPEED: f32 = 11.0;
const PLAYER_JUMP_SPEED: f32 = 9.5;
const PLAYER_GRAVITY: f32 = 28.0;
const STEP_HEIGHT: f32 = 0.6;
const COLLISION_STEP: f32 = 0.2;
const WEB_PLACE_BLOCK: BlockId = BlockId::Stone;

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

    let canvas = window.canvas().expect("winit web canvas");
    attach_canvas(canvas.clone());

    let renderer = Renderer::new(window).await?;
    let (mesh_result_rx, workers, worker_onmessage) = start_mesh_worker_pool(MESH_WORKER_COUNT)?;
    let mut app = WebApp::new(renderer.size(), canvas, workers, worker_onmessage, mesh_result_rx);
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
                WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                    app.handle_mouse_button(button);
                }
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
                if app.mouse_captured {
                    app.handle_mouse_motion(delta.0 as f32, delta.1 as f32);
                }
            }
            Event::AboutToWait => window.request_redraw(),
            _ => {}
        }
    })?;

    Ok(())
}

struct WebApp {
    canvas: HtmlCanvasElement,
    camera: Camera,
    terrain: TerrainGenerator,
    collision_heightmaps: HashMap<ChunkPos, Vec<u16>>,
    pressed: HashSet<KeyCode>,
    last_tick: Instant,
    size: PhysicalSize<u32>,
    current_chunk: ChunkPos,
    desired_chunks: HashSet<ChunkPos>,
    pending_generation: VecDeque<ChunkPos>,
    inflight_generation: HashSet<ChunkPos>,
    dirty_generation: HashSet<ChunkPos>,
    movement_active: bool,
    mouse_captured: bool,
    chunk_edits: HashMap<ChunkPos, HashMap<(u8, u8, u8), BlockId>>,
    mesh_result_rx: Receiver<MeshBuildResult>,
    workers: Vec<Worker>,
    next_worker_index: usize,
    _worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
}

impl WebApp {
    fn new(
        size: PhysicalSize<u32>,
        canvas: HtmlCanvasElement,
        workers: Vec<Worker>,
        worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
        mesh_result_rx: Receiver<MeshBuildResult>,
    ) -> Self {
        let terrain = TerrainGenerator::new(0xA66D_E601);
        let mut camera = Camera::default();
        camera.position = find_safe_spawn_position(&terrain);
        camera.on_ground = true;
        let current_chunk = chunk_from_world_position(camera.position);
        let desired_chunks = desired_chunk_set(current_chunk, WEB_RADIUS);
        let pending_generation =
            prioritize_chunks(desired_chunks.iter().copied().collect(), current_chunk, camera.position, camera.forward());

        Self {
            canvas,
            camera,
            terrain,
            collision_heightmaps: HashMap::new(),
            pressed: HashSet::new(),
            last_tick: Instant::now(),
            size,
            current_chunk,
            desired_chunks,
            pending_generation,
            inflight_generation: HashSet::new(),
            dirty_generation: HashSet::new(),
            movement_active: false,
            mouse_captured: false,
            chunk_edits: HashMap::new(),
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

        if code == KeyCode::Escape && event.state == ElementState::Pressed {
            if let Some(document) = document() {
                document.exit_pointer_lock();
            }
            self.mouse_captured = false;
        }

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

    fn handle_mouse_button(&mut self, button: MouseButton) {
        if !self.mouse_captured {
            self.canvas.request_pointer_lock();
            self.mouse_captured = pointer_is_locked(&self.canvas);
        }

        let Some(hit) = self.raycast_world(6.0) else {
            return;
        };

        match button {
            MouseButton::Left => {
                self.apply_local_block_edit(hit.block, BlockId::Air);
            }
            MouseButton::Right => {
                let Some(place_at) = hit.previous_empty else {
                    return;
                };

                if self.player_collides_with_world_pos(self.camera.position, place_at, WEB_PLACE_BLOCK) {
                    return;
                }

                self.apply_local_block_edit(place_at, WEB_PLACE_BLOCK);
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.mouse_captured = pointer_is_locked(&self.canvas);
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
        let jump = self.pressed.contains(&KeyCode::Space);
        let sprint = self.pressed.contains(&KeyCode::ShiftLeft);

        self.movement_active = movement != Vec3::ZERO;
        self.update_camera_physics(dt, movement, jump, sprint);
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
                self.collision_heightmaps
                    .insert(result.position, result.heights.clone());
                chunk_meshes.insert(result.position, renderer.create_mesh(&result.vertices, &result.indices));
            }

            if self.dirty_generation.remove(&result.position) {
                self.pending_generation.push_front(result.position);
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
            let edits = self.chunk_edits.get(&position);
            dispatch_mesh_job(&self.workers[worker_index], position, edits);
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
        self.collision_heightmaps
            .retain(|position, _| self.desired_chunks.contains(position));
        self.chunk_edits
            .retain(|position, _| self.desired_chunks.contains(position));
        self.pending_generation
            .retain(|position| self.desired_chunks.contains(position));
        self.inflight_generation
            .retain(|position| self.desired_chunks.contains(position));
        self.dirty_generation
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

    fn update_camera_physics(&mut self, dt: Duration, local_movement: Vec3, jump: bool, sprint: bool) {
        let dt_secs = dt.as_secs_f32();
        if dt_secs <= 0.0 {
            return;
        }

        let mut horizontal = Vec3::new(local_movement.x, 0.0, local_movement.z);
        if horizontal.length_squared() > 1.0 {
            horizontal = horizontal.normalize();
        }

        let forward = Vec3::new(self.camera.yaw.sin(), 0.0, self.camera.yaw.cos()).normalize_or_zero();
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let speed = if sprint {
            PLAYER_SPRINT_SPEED
        } else {
            PLAYER_WALK_SPEED
        };
        let horizontal_delta = (forward * -horizontal.z + right * horizontal.x) * speed * dt_secs;

        if jump && self.camera.on_ground {
            self.camera.vertical_velocity = PLAYER_JUMP_SPEED;
            self.camera.on_ground = false;
        }

        self.camera.vertical_velocity -= PLAYER_GRAVITY * dt_secs;
        let vertical_delta = self.camera.vertical_velocity * dt_secs;

        let mut position = self.camera.position;
        self.sweep_axis(&mut position, horizontal_delta.x, Axis::X, self.camera.on_ground);
        self.sweep_axis(&mut position, horizontal_delta.z, Axis::Z, self.camera.on_ground);

        let moved_vertically = self.sweep_axis(&mut position, vertical_delta, Axis::Y, false);
        if moved_vertically {
            self.camera.on_ground = false;
        } else {
            if self.camera.vertical_velocity < 0.0 {
                self.camera.on_ground = true;
            }
            self.camera.vertical_velocity = 0.0;
        }

        self.camera.position = position;
    }

    fn sweep_axis(&mut self, position: &mut Vec3, delta: f32, axis: Axis, allow_step: bool) -> bool {
        if delta.abs() <= f32::EPSILON {
            return false;
        }

        let steps = (delta.abs() / COLLISION_STEP).ceil().max(1.0) as usize;
        let step = delta / steps as f32;
        let mut moved = false;

        for _ in 0..steps {
            let mut candidate = *position;
            match axis {
                Axis::X => candidate.x += step,
                Axis::Y => candidate.y += step,
                Axis::Z => candidate.z += step,
            }

            if self.player_collides(candidate) {
                if allow_step && matches!(axis, Axis::X | Axis::Z) {
                    let mut stepped = candidate;
                    stepped.y += STEP_HEIGHT;
                    if !self.player_collides(stepped) {
                        *position = stepped;
                        moved = true;
                        continue;
                    }
                }
                return moved;
            }

            *position = candidate;
            moved = true;
        }

        moved
    }

    fn player_collides(&mut self, eye_position: Vec3) -> bool {
        let min = Vec3::new(
            eye_position.x - PLAYER_RADIUS,
            eye_position.y - PLAYER_EYE_HEIGHT,
            eye_position.z - PLAYER_RADIUS,
        );
        let max = Vec3::new(
            eye_position.x + PLAYER_RADIUS,
            eye_position.y + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT),
            eye_position.z + PLAYER_RADIUS,
        );

        let min_x = min.x.floor() as i32;
        let max_x = (max.x - 0.001).floor() as i32;
        let min_y = min.y.floor() as i32;
        let max_y = (max.y - 0.001).floor() as i32;
        let min_z = min.z.floor() as i32;
        let max_z = (max.z - 0.001).floor() as i32;

        for y in min_y..=max_y {
            for z in min_z..=max_z {
                for x in min_x..=max_x {
                    if self.world_block_is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn world_block_is_solid(&mut self, x: i32, y: i32, z: i32) -> bool {
        if y < 0 {
            return true;
        }
        if y >= CHUNK_HEIGHT {
            return false;
        }

        let world = WorldPos {
            x: i64::from(x),
            y,
            z: i64::from(z),
        };
        let chunk_pos = ChunkPos::from_world(world);
        let local = LocalVoxelPos {
            x: x.rem_euclid(CHUNK_WIDTH) as u8,
            y: y as u8,
            z: z.rem_euclid(CHUNK_DEPTH) as u8,
        };

        if let Some(block) = self
            .chunk_edits
            .get(&chunk_pos)
            .and_then(|edits| edits.get(&(local.x, local.y, local.z)))
            .copied()
        {
            return block_is_solid(block);
        }

        if let Some(heights) = self.collision_heightmaps.get(&chunk_pos) {
            let surface = heights[usize::from(local.z) * CHUNK_WIDTH as usize + usize::from(local.x)] as i32;
            return y <= surface;
        }

        let chunk = self.terrain.generate_chunk(chunk_pos);
        matches!(
            chunk.voxel(local).block,
            BlockId::Grass
                | BlockId::Dirt
                | BlockId::Stone
                | BlockId::Sand
                | BlockId::Log
                | BlockId::Planks
                | BlockId::Glass
                | BlockId::Lantern
                | BlockId::Storage
        )
    }

    fn player_collides_with_world_pos(&mut self, eye_position: Vec3, position: WorldPos, block: BlockId) -> bool {
        let min = Vec3::new(
            eye_position.x - PLAYER_RADIUS,
            eye_position.y - PLAYER_EYE_HEIGHT,
            eye_position.z - PLAYER_RADIUS,
        );
        let max = Vec3::new(
            eye_position.x + PLAYER_RADIUS,
            eye_position.y + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT),
            eye_position.z + PLAYER_RADIUS,
        );

        let min_x = min.x.floor() as i32;
        let max_x = (max.x - 0.001).floor() as i32;
        let min_y = min.y.floor() as i32;
        let max_y = (max.y - 0.001).floor() as i32;
        let min_z = min.z.floor() as i32;
        let max_z = (max.z - 0.001).floor() as i32;

        for y in min_y..=max_y {
            for z in min_z..=max_z {
                for x in min_x..=max_x {
                    if i64::from(x) == position.x && y == position.y && i64::from(z) == position.z {
                        if block_is_solid(block) {
                            return true;
                        }
                        continue;
                    }

                    if self.world_block_is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn raycast_world(&mut self, max_distance: f32) -> Option<RaycastHit> {
        let direction = self.camera.forward().normalize_or_zero();
        if direction == Vec3::ZERO {
            return None;
        }

        let step = 0.1;
        let steps = (max_distance / step).ceil() as usize;
        let mut previous_empty = None;

        for index in 1..=steps {
            let sample = self.camera.position + direction * (index as f32 * step);
            let world = WorldPos {
                x: sample.x.floor() as i64,
                y: sample.y.floor() as i32,
                z: sample.z.floor() as i64,
            };

            if self.world_block_is_solid(world.x as i32, world.y, world.z as i32) {
                return Some(RaycastHit {
                    block: world,
                    previous_empty,
                });
            }

            previous_empty = Some(world);
        }

        None
    }

    fn apply_local_block_edit(&mut self, position: WorldPos, block: BlockId) {
        let Ok((chunk_pos, local)) = position.to_chunk_local() else {
            return;
        };

        self.chunk_edits
            .entry(chunk_pos)
            .or_default()
            .insert((local.x, local.y, local.z), block);
        self.schedule_chunk_rebuild(chunk_pos);
    }

    fn schedule_chunk_rebuild(&mut self, position: ChunkPos) {
        if self.inflight_generation.contains(&position) {
            self.dirty_generation.insert(position);
            return;
        }

        if !self.pending_generation.contains(&position) {
            self.pending_generation.push_front(position);
        }
    }
}

#[derive(Default)]
struct Camera {
    position: Vec3,
    yaw: f32,
    pitch: f32,
    vertical_velocity: f32,
    on_ground: bool,
}

impl Camera {

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

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
    Z,
}

fn attach_canvas(canvas: HtmlCanvasElement) {
    let window = web_sys::window().expect("window");
    let document = window.document().expect("document");
    let body = document.body().expect("body");
    let _ = body.append_child(&canvas);
}

fn document() -> Option<Document> {
    web_sys::window()?.document()
}

fn pointer_is_locked(canvas: &HtmlCanvasElement) -> bool {
    document()
        .and_then(|document| document.pointer_lock_element())
        .and_then(|element| element.dyn_into::<HtmlCanvasElement>().ok())
        .map(|locked| locked == *canvas)
        .unwrap_or(false)
}

fn find_safe_spawn_position(terrain: &TerrainGenerator) -> Vec3 {
    let mut chunks = HashMap::<ChunkPos, ChunkData>::new();

    for radius in 0_i32..=8 {
        for z in -radius..=radius {
            for x in -radius..=radius {
                if radius > 0 && x.abs().max(z.abs()) != radius {
                    continue;
                }

                let world_x = i64::from(x * 2);
                let world_z = i64::from(z * 2);
                let surface = terrain.surface_height(world_x, world_z);
                let candidate = Vec3::new(
                    world_x as f32 + 0.5,
                    surface as f32 + 1.0 + PLAYER_EYE_HEIGHT,
                    world_z as f32 + 0.5,
                );

                if !generated_player_collides(terrain, &mut chunks, candidate) {
                    return candidate;
                }
            }
        }
    }

    Vec3::new(0.5, terrain.surface_height(0, 0) as f32 + 3.0 + PLAYER_EYE_HEIGHT, 0.5)
}

fn generated_player_collides(
    terrain: &TerrainGenerator,
    chunks: &mut HashMap<ChunkPos, ChunkData>,
    eye_position: Vec3,
) -> bool {
    let min = Vec3::new(
        eye_position.x - PLAYER_RADIUS,
        eye_position.y - PLAYER_EYE_HEIGHT,
        eye_position.z - PLAYER_RADIUS,
    );
    let max = Vec3::new(
        eye_position.x + PLAYER_RADIUS,
        eye_position.y + (PLAYER_HEIGHT - PLAYER_EYE_HEIGHT),
        eye_position.z + PLAYER_RADIUS,
    );

    let min_x = min.x.floor() as i32;
    let max_x = (max.x - 0.001).floor() as i32;
    let min_y = min.y.floor() as i32;
    let max_y = (max.y - 0.001).floor() as i32;
    let min_z = min.z.floor() as i32;
    let max_z = (max.z - 0.001).floor() as i32;

    for y in min_y..=max_y {
        for z in min_z..=max_z {
            for x in min_x..=max_x {
                if generated_world_block_is_solid(terrain, chunks, x, y, z) {
                    return true;
                }
            }
        }
    }

    false
}

fn generated_world_block_is_solid(
    terrain: &TerrainGenerator,
    chunks: &mut HashMap<ChunkPos, ChunkData>,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    if y < 0 {
        return true;
    }
    if y >= CHUNK_HEIGHT {
        return false;
    }

    let world = WorldPos {
        x: i64::from(x),
        y,
        z: i64::from(z),
    };
    let chunk_pos = ChunkPos::from_world(world);
    let local = LocalVoxelPos {
        x: x.rem_euclid(CHUNK_WIDTH) as u8,
        y: y as u8,
        z: z.rem_euclid(CHUNK_DEPTH) as u8,
    };

    if !chunks.contains_key(&chunk_pos) {
        chunks.insert(chunk_pos, terrain.generate_chunk(chunk_pos));
    }

    let Some(chunk) = chunks.get(&chunk_pos) else {
        return false;
    };

    block_is_solid(chunk.voxel(local).block)
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
                    heights: Vec::new(),
                    failed: true,
                });
                return;
            }

            let vertices_value = js_sys::Reflect::get(&object, &JsValue::from_str("vertices")).unwrap();
            let indices_value = js_sys::Reflect::get(&object, &JsValue::from_str("indices")).unwrap();
            let heights_value = js_sys::Reflect::get(&object, &JsValue::from_str("heights")).unwrap();
            let vertex_floats = js_sys::Float32Array::new(&vertices_value).to_vec();
            let indices = js_sys::Uint32Array::new(&indices_value).to_vec();
            let heights = js_sys::Uint16Array::new(&heights_value).to_vec();
            let vertices = vertex_floats
                .chunks_exact(11)
                .map(|values| Vertex {
                    position: [values[0], values[1], values[2]],
                    color: [values[3], values[4], values[5]],
                    normal: [values[6], values[7], values[8]],
                    uv: [values[9], values[10]],
                })
                .collect::<Vec<_>>();

            let _ = tx.send(MeshBuildResult {
                position: ChunkPos { x, z },
                vertices,
                indices,
                heights,
                failed: false,
            });
        }) as Box<dyn FnMut(MessageEvent)>);

        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        workers.push(worker);
        onmessages.push(onmessage);
    }

    Ok((rx, workers, onmessages))
}

fn dispatch_mesh_job(
    worker: &Worker,
    position: ChunkPos,
    edits: Option<&HashMap<(u8, u8, u8), BlockId>>,
) {
    let job = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("kind"), &JsValue::from_str("build"));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("x"), &JsValue::from_f64(f64::from(position.x)));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("z"), &JsValue::from_f64(f64::from(position.z)));
    let edits_array = js_sys::Array::new();
    if let Some(edits) = edits {
        for (&(x, y, z), &block) in edits {
            let edit = js_sys::Array::new();
            edit.push(&JsValue::from_f64(f64::from(x)));
            edit.push(&JsValue::from_f64(f64::from(y)));
            edit.push(&JsValue::from_f64(f64::from(z)));
            edit.push(&JsValue::from_f64(block as u16 as f64));
            edits_array.push(&edit);
        }
    }
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("edits"), &edits_array);
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
    heights: Vec<u16>,
    failed: bool,
}

struct RaycastHit {
    block: WorldPos,
    previous_empty: Option<WorldPos>,
}

fn block_is_solid(block: BlockId) -> bool {
    matches!(
        block,
        BlockId::Grass
            | BlockId::Dirt
            | BlockId::Stone
            | BlockId::Sand
            | BlockId::Log
            | BlockId::Planks
            | BlockId::Glass
            | BlockId::Lantern
            | BlockId::Storage
    )
}
