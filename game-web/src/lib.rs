#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_WIDTH, ChunkPos};
use shared_world::{BlockId, ChunkData, TerrainGenerator};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_time::Instant;
use web_sys::HtmlCanvasElement;
use wgpu_lite::{Mesh, Renderer, Vertex};
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::WindowExtWebSys;
use winit::window::WindowBuilder;

const WEB_RADIUS: i32 = 6;
const CHUNK_WORLD_RADIUS: f32 = (CHUNK_WIDTH as f32) * 0.5;
const DRAW_DISTANCE_CHUNKS: f32 = 14.0;

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
    let mut app = WebApp::new(renderer.size());
    let chunk_meshes = build_world_meshes(&renderer);
    let mut renderer = renderer;

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
                    renderer.update_camera(app.camera_matrix());
                    let visible_meshes = chunk_meshes
                        .iter()
                        .filter_map(|(position, mesh)| app.chunk_is_visible(*position).then_some(mesh))
                        .collect::<Vec<_>>();

                    if let Err(error) = renderer.render(&visible_meshes) {
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
}

impl WebApp {
    fn new(size: PhysicalSize<u32>) -> Self {
        let mut camera = Camera::default();
        camera.position = Vec3::new(8.0, 98.0, 16.0);

        Self {
            camera,
            pressed: HashSet::new(),
            last_tick: Instant::now(),
            size,
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

fn build_world_meshes(renderer: &Renderer<'_>) -> HashMap<ChunkPos, Mesh> {
    let generator = TerrainGenerator::new(0xA66D_E601);
    let mut meshes = HashMap::new();

    for z in -WEB_RADIUS..=WEB_RADIUS {
        for x in -WEB_RADIUS..=WEB_RADIUS {
            let position = ChunkPos { x, z };
            let chunk = generator.generate_chunk(position);
            let (vertices, indices) = mesh_chunk(&chunk);
            meshes.insert(position, renderer.create_mesh(&vertices, &indices));
        }
    }

    meshes
}

fn mesh_chunk(chunk: &ChunkData) -> (Vec<Vertex>, Vec<u32>) {
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let origin_x = chunk.position.x as f32 * CHUNK_WIDTH as f32;
    let origin_z = chunk.position.z as f32 * CHUNK_DEPTH as f32;

    for y in 0..shared_math::CHUNK_HEIGHT {
        for z in 0..CHUNK_DEPTH {
            for x in 0..CHUNK_WIDTH {
                let block = chunk.voxel(shared_math::LocalVoxelPos {
                    x: x as u8,
                    y: y as u8,
                    z: z as u8,
                });
                if block.block.is_empty() || matches!(block.block, BlockId::Water) {
                    continue;
                }

                let world = [origin_x + x as f32, y as f32, origin_z + z as f32];
                emit_block_faces(chunk, &mut vertices, &mut indices, world, x, y, z, block.block);
            }
        }
    }

    (vertices, indices)
}

fn emit_block_faces(
    chunk: &ChunkData,
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    world: [f32; 3],
    x: i32,
    y: i32,
    z: i32,
    block: BlockId,
) {
    let base_color = [1.0, 1.0, 1.0];
    let neighbors = [
        ((0, 0, -1), face_vertices(world, Face::North, base_color, tile_uvs(block, Face::North))),
        ((0, 0, 1), face_vertices(world, Face::South, base_color, tile_uvs(block, Face::South))),
        ((-1, 0, 0), face_vertices(world, Face::West, base_color, tile_uvs(block, Face::West))),
        ((1, 0, 0), face_vertices(world, Face::East, base_color, tile_uvs(block, Face::East))),
        ((0, 1, 0), face_vertices(world, Face::Up, brighten(base_color, 0.08), tile_uvs(block, Face::Up))),
        ((0, -1, 0), face_vertices(world, Face::Down, darken(base_color, 0.16), tile_uvs(block, Face::Down))),
    ];

    for (offset, face) in neighbors {
        let neighbor = sample_voxel(chunk, x + offset.0, y + offset.1, z + offset.2);
        if neighbor.map(|voxel| voxel.block.is_transparent()).unwrap_or(true) {
            let base = vertices.len() as u32;
            vertices.extend(face);
            indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
}

fn sample_voxel(chunk: &ChunkData, x: i32, y: i32, z: i32) -> Option<shared_world::Voxel> {
    if !(0..CHUNK_WIDTH).contains(&x) || !(0..shared_math::CHUNK_HEIGHT).contains(&y) || !(0..CHUNK_DEPTH).contains(&z) {
        return None;
    }

    Some(chunk.voxel(shared_math::LocalVoxelPos {
        x: x as u8,
        y: y as u8,
        z: z as u8,
    }))
}

#[derive(Clone, Copy)]
enum Face {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

fn face_vertices(origin: [f32; 3], face: Face, color: [f32; 3], uvs: [[f32; 2]; 4]) -> [Vertex; 4] {
    let [x, y, z] = origin;
    match face {
        Face::North => [v(x, y + 1.0, z, color, uvs[0]), v(x + 1.0, y + 1.0, z, color, uvs[1]), v(x + 1.0, y, z, color, uvs[2]), v(x, y, z, color, uvs[3])],
        Face::South => [v(x + 1.0, y + 1.0, z + 1.0, color, uvs[0]), v(x, y + 1.0, z + 1.0, color, uvs[1]), v(x, y, z + 1.0, color, uvs[2]), v(x + 1.0, y, z + 1.0, color, uvs[3])],
        Face::East => [v(x + 1.0, y + 1.0, z, color, uvs[0]), v(x + 1.0, y + 1.0, z + 1.0, color, uvs[1]), v(x + 1.0, y, z + 1.0, color, uvs[2]), v(x + 1.0, y, z, color, uvs[3])],
        Face::West => [v(x, y + 1.0, z + 1.0, color, uvs[0]), v(x, y + 1.0, z, color, uvs[1]), v(x, y, z, color, uvs[2]), v(x, y, z + 1.0, color, uvs[3])],
        Face::Up => [v(x, y + 1.0, z, color, uvs[0]), v(x, y + 1.0, z + 1.0, color, uvs[1]), v(x + 1.0, y + 1.0, z + 1.0, color, uvs[2]), v(x + 1.0, y + 1.0, z, color, uvs[3])],
        Face::Down => [v(x, y, z, color, uvs[0]), v(x + 1.0, y, z, color, uvs[1]), v(x + 1.0, y, z + 1.0, color, uvs[2]), v(x, y, z + 1.0, color, uvs[3])],
    }
}

fn v(x: f32, y: f32, z: f32, color: [f32; 3], uv: [f32; 2]) -> Vertex {
    Vertex {
        position: [x, y, z],
        color,
        uv,
    }
}

fn tile_uvs(block: BlockId, face: Face) -> [[f32; 2]; 4] {
    atlas_quad(tile_for(block, face))
}

fn tile_for(block: BlockId, face: Face) -> (u32, u32) {
    match block {
        BlockId::Grass => match face {
            Face::Up => (1, 0),
            Face::Down => (0, 0),
            _ => (1, 1),
        },
        BlockId::Dirt => (0, 0),
        BlockId::Stone => (2, 0),
        BlockId::Sand => (3, 0),
        BlockId::Water => (2, 1),
        BlockId::Log => match face {
            Face::Up | Face::Down => (3, 1),
            _ => (0, 1),
        },
        BlockId::Leaves => (1, 1),
        BlockId::Planks => (3, 1),
        BlockId::Glass => (2, 1),
        BlockId::Lantern => (3, 1),
        BlockId::Storage => (0, 1),
        BlockId::Air => (0, 0),
    }
}

fn atlas_quad(tile: (u32, u32)) -> [[f32; 2]; 4] {
    const TILE_COUNT: f32 = 4.0;
    const EPS: f32 = 0.001;

    let min_u = tile.0 as f32 / TILE_COUNT + EPS;
    let max_u = (tile.0 + 1) as f32 / TILE_COUNT - EPS;
    let min_v = tile.1 as f32 / TILE_COUNT + EPS;
    let max_v = (tile.1 + 1) as f32 / TILE_COUNT - EPS;

    [[min_u, min_v], [max_u, min_v], [max_u, max_v], [min_u, max_v]]
}

fn darken(color: [f32; 3], amount: f32) -> [f32; 3] {
    [color[0] * (1.0 - amount), color[1] * (1.0 - amount), color[2] * (1.0 - amount)]
}

fn brighten(color: [f32; 3], amount: f32) -> [f32; 3] {
    [
        (color[0] + amount).min(1.0),
        (color[1] + amount).min(1.0),
        (color[2] + amount).min(1.0),
    ]
}
