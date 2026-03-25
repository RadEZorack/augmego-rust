use anyhow::{Context, Result};
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, WorldPos};
use shared_protocol::{
    BreakBlockRequest, ChunkUnload, ClientHello, ClientMessage, LoginRequest, PROTOCOL_VERSION,
    InventorySnapshot, InventoryStack, PlaceBlockRequest, ServerMessage, SubscribeChunks, decode,
    frame,
};
use shared_world::{BlockId, ChunkData};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use wgpu_lite::{Mesh, Renderer, Vertex};
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowBuilder};

const CHUNK_RADIUS: u8 = 12;
const CHUNK_WORLD_RADIUS: f32 = (CHUNK_WIDTH as f32) * 0.5;
const DRAW_DISTANCE_CHUNKS: f32 = 18.0;
const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_HEIGHT: f32 = 1.8;
const PLAYER_EYE_HEIGHT: f32 = 1.62;
const PLAYER_WALK_SPEED: f32 = 7.5;
const PLAYER_SPRINT_SPEED: f32 = 11.0;
const PLAYER_JUMP_SPEED: f32 = 9.5;
const PLAYER_GRAVITY: f32 = 28.0;
const COLLISION_STEP: f32 = 0.2;
const STEP_HEIGHT: f32 = 0.6;
const CROSSHAIR_DISTANCE: f32 = 0.6;
const CROSSHAIR_LENGTH: f32 = 0.035;
const CROSSHAIR_THICKNESS: f32 = 0.004;
const TARGET_OUTLINE_THICKNESS: f32 = 0.035;

fn main() -> Result<()> {
    let event_loop = EventLoop::new()?;
    let window: &'static winit::window::Window = Box::leak(Box::new(
        WindowBuilder::new()
            .with_title("Augmego Voxel Sandbox")
            .build(&event_loop)?,
    ));
    set_mouse_capture(window, true);

    let renderer = pollster::block_on(Renderer::new(window))?;
    let (network_tx, network_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    let (mesh_job_tx, mesh_job_rx) = mpsc::channel();
    let (mesh_result_tx, mesh_result_rx) = mpsc::channel();
    start_network_thread(network_tx, command_rx);
    start_mesh_workers(mesh_job_rx, mesh_result_tx);

    let mut app = GameApp::new(renderer.size(), network_rx, command_tx, mesh_job_tx, mesh_result_rx);
    let mut chunk_meshes: HashMap<ChunkPos, Mesh> = HashMap::new();
    let mut renderer = renderer;

    event_loop.run(move |event, target| {
        target.set_control_flow(ControlFlow::Poll);

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    renderer.resize(size);
                    app.resize(size.width, size.height);
                }
                WindowEvent::KeyboardInput { event, .. } => {
                    if app.handle_key(event) {
                        set_mouse_capture(window, false);
                    }
                }
                WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                    if !app.mouse_captured {
                        set_mouse_capture(window, true);
                        app.mouse_captured = true;
                        return;
                    }
                    app.handle_mouse_button(button);
                }
                WindowEvent::RedrawRequested => {
                    app.drain_network();
                    app.process_mesh_updates(&renderer, &mut chunk_meshes, 6);

                    app.tick();
                    renderer.update_camera(app.camera_matrix());
                    window.set_title(&app.hud_title());
                    let visible_meshes = chunk_meshes
                        .iter()
                        .filter_map(|(position, mesh)| app.chunk_is_visible(*position).then_some(mesh))
                        .collect::<Vec<_>>();
                    let mut overlay_meshes = Vec::new();
                    if let Some(mesh) = app.build_crosshair_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    if let Some(mesh) = app.build_target_highlight_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    let overlay_refs = overlay_meshes.iter().collect::<Vec<_>>();
                    if let Err(error) = renderer.render(&visible_meshes, &overlay_refs) {
                        eprintln!("render error: {error:?}");
                        target.exit();
                    }
                }
                _ => {}
            },
            Event::DeviceEvent { event: DeviceEvent::MouseMotion { delta }, .. } => {
                if app.mouse_captured {
                    app.handle_mouse_motion(delta.0 as f32, delta.1 as f32);
                }
            }
            Event::AboutToWait => {
                window.request_redraw();
            }
            _ => {}
        }
    })?;

    Ok(())
}

struct GameApp {
    chunk_cache: HashMap<ChunkPos, ChunkData>,
    network_rx: Receiver<NetworkEvent>,
    command_tx: Sender<ClientCommand>,
    pressed: HashSet<KeyCode>,
    camera: Camera,
    spawned: bool,
    last_tick: Instant,
    width: u32,
    height: u32,
    mesh_job_tx: Sender<MeshJob>,
    mesh_result_rx: Receiver<MeshBuildResult>,
    inflight_meshes: HashSet<ChunkPos>,
    dirty_meshes: HashSet<ChunkPos>,
    current_subscription_center: Option<ChunkPos>,
    physics_ready: bool,
    mouse_captured: bool,
    inventory: Vec<InventoryStack>,
    selected_hotbar: usize,
}

impl GameApp {
    fn new(
        size: winit::dpi::PhysicalSize<u32>,
        network_rx: Receiver<NetworkEvent>,
        command_tx: Sender<ClientCommand>,
        mesh_job_tx: Sender<MeshJob>,
        mesh_result_rx: Receiver<MeshBuildResult>,
    ) -> Self {
        Self {
            chunk_cache: HashMap::new(),
            network_rx,
            command_tx,
            pressed: HashSet::new(),
            camera: Camera::default(),
            spawned: false,
            last_tick: Instant::now(),
            width: size.width.max(1),
            height: size.height.max(1),
            mesh_job_tx,
            mesh_result_rx,
            inflight_meshes: HashSet::new(),
            dirty_meshes: HashSet::new(),
            current_subscription_center: None,
            physics_ready: false,
            mouse_captured: true,
            inventory: vec![
                InventoryStack { block: BlockId::Grass, count: 64 },
                InventoryStack { block: BlockId::Stone, count: 64 },
                InventoryStack { block: BlockId::Planks, count: 32 },
            ],
            selected_hotbar: 0,
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    fn handle_key(&mut self, event: KeyEvent) -> bool {
        let code = match event.physical_key {
            PhysicalKey::Code(code) => code,
            _ => return false,
        };

        if code == KeyCode::Escape && event.state == ElementState::Pressed {
            self.mouse_captured = false;
            return true;
        }

        if event.state == ElementState::Pressed {
            match code {
                KeyCode::Digit1 => self.selected_hotbar = 0,
                KeyCode::Digit2 => self.selected_hotbar = 1,
                KeyCode::Digit3 => self.selected_hotbar = 2,
                KeyCode::Digit4 => self.selected_hotbar = 3,
                KeyCode::Digit5 => self.selected_hotbar = 4,
                KeyCode::Digit6 => self.selected_hotbar = 5,
                KeyCode::Digit7 => self.selected_hotbar = 6,
                KeyCode::Digit8 => self.selected_hotbar = 7,
                KeyCode::Digit9 => self.selected_hotbar = 8,
                _ => {}
            }
        }

        match event.state {
            ElementState::Pressed => {
                self.pressed.insert(code);
            }
            ElementState::Released => {
                self.pressed.remove(&code);
            }
        }

        false
    }

    fn handle_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.camera.yaw -= dx * 0.0025;
        self.camera.pitch = (self.camera.pitch - dy * 0.0025).clamp(-1.45, 1.45);
    }

    fn handle_mouse_button(&mut self, button: MouseButton) {
        if !self.spawned || !self.physics_ready {
            return;
        }

        let Some(hit) = self.current_target() else {
            return;
        };

        match button {
            MouseButton::Left => {
                self.apply_local_block_edit(hit.block, BlockId::Air);
                let _ = self.command_tx.send(ClientCommand::BreakBlock(hit.block));
            }
            MouseButton::Right => {
                let Some(place_at) = hit.previous_empty else {
                    return;
                };

                let selected_block = self.selected_hotbar_block();
                if player_collides_with_world_pos(&self.chunk_cache, self.camera.position, place_at, selected_block) {
                    return;
                }

                self.apply_local_block_edit(place_at, selected_block);
                let _ = self.command_tx.send(ClientCommand::PlaceBlock {
                    position: place_at,
                    block: selected_block,
                });
            }
            _ => {}
        }
    }

    fn drain_network(&mut self) {
        while let Ok(event) = self.network_rx.try_recv() {
            match event {
                NetworkEvent::Chunk(chunk) => {
                    let position = chunk.position;
                    self.chunk_cache.insert(position, chunk);
                    self.schedule_mesh(position);
                }
                NetworkEvent::ChunkUnload(unload) => {
                    for position in unload.positions {
                        self.chunk_cache.remove(&position);
                        self.inflight_meshes.remove(&position);
                        self.dirty_meshes.remove(&position);
                    }
                }
                NetworkEvent::Welcome { message, spawn_position } => {
                    println!("{message}");
                    if !self.spawned {
                        self.camera.position = Vec3::new(
                            spawn_position.x as f32 + 0.5,
                            spawn_position.y as f32 + PLAYER_EYE_HEIGHT,
                            spawn_position.z as f32 + 0.5,
                        );
                        self.camera.vertical_velocity = 0.0;
                        self.camera.on_ground = false;
                        self.spawned = true;
                        self.physics_ready = false;
                    }
                }
                NetworkEvent::Inventory(snapshot) => {
                    self.inventory = snapshot.slots;
                    if self.selected_hotbar >= self.inventory.len() {
                        self.selected_hotbar = self.inventory.len().saturating_sub(1);
                    }
                }
                NetworkEvent::PlayerState { position } => {
                    let server_camera = Vec3::new(position[0], position[1] + PLAYER_EYE_HEIGHT, position[2]);
                    if !self.spawned {
                        self.camera.position = server_camera;
                        self.camera.vertical_velocity = 0.0;
                        self.camera.on_ground = false;
                        self.spawned = true;
                        self.physics_ready = false;
                    }
                }
                NetworkEvent::BlockAction(message) => {
                    println!("block action: {message}");
                }
                NetworkEvent::Disconnected(reason) => {
                    eprintln!("network disconnected: {reason}");
                }
            }
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick);
        self.last_tick = now;

        if !self.spawned {
            return;
        }

        if !self.physics_ready {
            self.update_chunk_subscription();
            if self.chunk_cache.contains_key(&world_to_chunk(self.camera.position)) {
                self.snap_to_ground();
                self.physics_ready = self.ensure_clear_spawn_space();
            }
        }

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

        if self.physics_ready {
            update_camera_physics(&self.chunk_cache, &mut self.camera, dt, movement, jump, sprint);
        } else {
            update_camera_preview(&mut self.camera, dt, movement, sprint);
        }
        self.update_chunk_subscription();

        let _ = self.command_tx.send(ClientCommand::Input {
            movement: [movement.x, 0.0, movement.z],
            jump,
        });
    }

    fn camera_matrix(&self) -> Mat4 {
        let aspect = self.width as f32 / self.height.max(1) as f32;
        self.camera.matrix(aspect)
    }

    fn current_target(&self) -> Option<RaycastHit> {
        if !self.spawned || !self.physics_ready {
            return None;
        }

        raycast_world(&self.chunk_cache, self.camera.position, self.camera.forward(), 6.0)
    }

    fn selected_hotbar_block(&self) -> BlockId {
        self.inventory
            .get(self.selected_hotbar)
            .map(|slot| slot.block)
            .unwrap_or(BlockId::Stone)
    }

    fn hud_title(&self) -> String {
        let selected = self.selected_hotbar_block();
        let target = self
            .current_target()
            .map(|hit| {
                let block = self
                    .block_at_world(hit.block)
                    .map(|voxel| voxel.block)
                    .unwrap_or(BlockId::Air);
                format!(" | Target: {:?}", block)
            })
            .unwrap_or_default();
        format!(
            "Augmego Voxel Sandbox | Slot {}: {:?} x{}{}",
            self.selected_hotbar + 1,
            selected,
            self.inventory
                .get(self.selected_hotbar)
                .map(|slot| slot.count)
                .unwrap_or(0),
            target
        )
    }

    fn build_crosshair_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        if !self.mouse_captured {
            return None;
        }

        let forward = self.camera.forward();
        let right = Vec3::new(-forward.z, 0.0, forward.x).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let center = self.camera.position + forward * CROSSHAIR_DISTANCE;

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_box_oriented(
            &mut vertices,
            &mut indices,
            center,
            right * CROSSHAIR_LENGTH,
            up * CROSSHAIR_THICKNESS,
            forward * CROSSHAIR_THICKNESS,
            [1.0, 1.0, 1.0],
            (3, 1),
        );
        add_box_oriented(
            &mut vertices,
            &mut indices,
            center,
            right * CROSSHAIR_THICKNESS,
            up * CROSSHAIR_LENGTH,
            forward * CROSSHAIR_THICKNESS,
            [1.0, 1.0, 1.0],
            (3, 1),
        );

        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn build_target_highlight_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let target = self.current_target()?;
        let min = Vec3::new(target.block.x as f32, target.block.y as f32, target.block.z as f32)
            - Vec3::splat(TARGET_OUTLINE_THICKNESS * 0.5);
        let max = min + Vec3::splat(1.0 + TARGET_OUTLINE_THICKNESS);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_wire_box(
            &mut vertices,
            &mut indices,
            min,
            max,
            TARGET_OUTLINE_THICKNESS,
            [1.0, 0.95, 0.45],
            (3, 1),
        );
        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn block_at_world(&self, position: WorldPos) -> Option<shared_world::Voxel> {
        let (chunk_pos, local) = position.to_chunk_local().ok()?;
        self.chunk_cache.get(&chunk_pos).map(|chunk| chunk.voxel(local))
    }

    fn process_mesh_updates(
        &mut self,
        renderer: &Renderer<'_>,
        chunk_meshes: &mut HashMap<ChunkPos, Mesh>,
        budget: usize,
    ) {
        for _ in 0..budget {
            let Ok(result) = self.mesh_result_rx.try_recv() else {
                break;
            };

            self.inflight_meshes.remove(&result.position);
            if self.chunk_cache.contains_key(&result.position) {
                chunk_meshes.insert(result.position, renderer.create_mesh(&result.vertices, &result.indices));
            } else {
                chunk_meshes.remove(&result.position);
            }

            if self.dirty_meshes.remove(&result.position) {
                self.schedule_mesh(result.position);
            }
        }

        chunk_meshes.retain(|position, _| self.chunk_cache.contains_key(position));
    }

    fn schedule_mesh(&mut self, position: ChunkPos) {
        if self.inflight_meshes.contains(&position) {
            self.dirty_meshes.insert(position);
            return;
        }

        let Some(chunk) = self.chunk_cache.get(&position).cloned() else {
            return;
        };

        if self.mesh_job_tx.send(MeshJob { position, chunk }).is_ok() {
            self.inflight_meshes.insert(position);
        }
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
        let chunk_radius = 24.0;
        let threshold = 0.15 - (chunk_radius / distance).min(0.25);
        self.camera.forward().dot(direction) >= threshold
    }

    fn update_chunk_subscription(&mut self) {
        let center = world_to_chunk(self.camera.position);
        if self.current_subscription_center == Some(center) {
            return;
        }

        self.current_subscription_center = Some(center);
        let _ = self
            .command_tx
            .send(ClientCommand::SubscribeChunks(SubscribeChunks { center, radius: CHUNK_RADIUS }));
    }

    fn snap_to_ground(&mut self) {
        let foot_x = self.camera.position.x.floor() as i32;
        let foot_z = self.camera.position.z.floor() as i32;
        let start_y = (self.camera.position.y - PLAYER_EYE_HEIGHT).floor() as i32;

        for y in (0..=start_y.max(0)).rev() {
            if world_block_is_solid(&self.chunk_cache, foot_x, y, foot_z) {
                self.camera.position.y = y as f32 + 1.0 + PLAYER_EYE_HEIGHT;
                self.camera.vertical_velocity = 0.0;
                self.camera.on_ground = true;
                return;
            }
        }
    }

    fn ensure_clear_spawn_space(&mut self) -> bool {
        if !player_collides(&self.chunk_cache, self.camera.position) {
            return true;
        }

        let base_feet = self.camera.position.y - PLAYER_EYE_HEIGHT;
        for offset in 1..=12 {
            let mut candidate = self.camera.position;
            candidate.y = base_feet + offset as f32 + PLAYER_EYE_HEIGHT;
            if !player_collides(&self.chunk_cache, candidate) {
                self.camera.position = candidate;
                self.camera.vertical_velocity = 0.0;
                self.camera.on_ground = false;
                return true;
            }
        }

        false
    }

    fn apply_local_block_edit(&mut self, position: WorldPos, block: BlockId) {
        let Ok((chunk_pos, local)) = position.to_chunk_local() else {
            return;
        };

        let Some(chunk) = self.chunk_cache.get_mut(&chunk_pos) else {
            return;
        };

        chunk.set_voxel(local, shared_world::Voxel { block });
        self.schedule_mesh(chunk_pos);
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

fn update_camera_physics(
    chunk_cache: &HashMap<ChunkPos, ChunkData>,
    camera: &mut Camera,
    dt: Duration,
    local_movement: Vec3,
    jump: bool,
    sprint: bool,
) {
    let dt_secs = dt.as_secs_f32();
    if dt_secs <= 0.0 {
        return;
    }

    let mut horizontal = Vec3::new(local_movement.x, 0.0, local_movement.z);
    if horizontal.length_squared() > 1.0 {
        horizontal = horizontal.normalize();
    }

    let forward = Vec3::new(camera.yaw.sin(), 0.0, camera.yaw.cos()).normalize_or_zero();
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let speed = if sprint {
        PLAYER_SPRINT_SPEED
    } else {
        PLAYER_WALK_SPEED
    };
    let horizontal_delta = (forward * -horizontal.z + right * horizontal.x) * speed * dt_secs;

    if jump && camera.on_ground {
        camera.vertical_velocity = PLAYER_JUMP_SPEED;
        camera.on_ground = false;
    }

    camera.vertical_velocity -= PLAYER_GRAVITY * dt_secs;
    let vertical_delta = camera.vertical_velocity * dt_secs;

    let mut position = camera.position;
    sweep_axis(chunk_cache, &mut position, horizontal_delta.x, Axis::X, camera.on_ground);
    sweep_axis(chunk_cache, &mut position, horizontal_delta.z, Axis::Z, camera.on_ground);

    let moved_vertically = sweep_axis(chunk_cache, &mut position, vertical_delta, Axis::Y, false);
    if moved_vertically {
        camera.on_ground = false;
    } else {
        if camera.vertical_velocity < 0.0 {
            camera.on_ground = true;
        }
        camera.vertical_velocity = 0.0;
    }

    camera.position = position;
}

fn update_camera_preview(camera: &mut Camera, dt: Duration, local_movement: Vec3, sprint: bool) {
    let dt_secs = dt.as_secs_f32();
    if dt_secs <= 0.0 {
        return;
    }

    let mut horizontal = Vec3::new(local_movement.x, 0.0, local_movement.z);
    if horizontal.length_squared() > 1.0 {
        horizontal = horizontal.normalize();
    }

    let forward = Vec3::new(camera.yaw.sin(), 0.0, camera.yaw.cos()).normalize_or_zero();
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    let speed = if sprint {
        PLAYER_SPRINT_SPEED
    } else {
        PLAYER_WALK_SPEED
    };

    camera.position += (forward * -horizontal.z + right * horizontal.x) * speed * dt_secs;
}

#[derive(Clone, Copy)]
enum Axis {
    X,
    Y,
    Z,
}

fn sweep_axis(
    chunk_cache: &HashMap<ChunkPos, ChunkData>,
    position: &mut Vec3,
    delta: f32,
    axis: Axis,
    allow_step: bool,
) -> bool {
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

        if player_collides(chunk_cache, candidate) {
            if allow_step && matches!(axis, Axis::X | Axis::Z) {
                let mut stepped = candidate;
                stepped.y += STEP_HEIGHT;
                if !player_collides(chunk_cache, stepped) {
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

fn player_collides(chunk_cache: &HashMap<ChunkPos, ChunkData>, eye_position: Vec3) -> bool {
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
                if world_block_is_solid(chunk_cache, x, y, z) {
                    return true;
                }
            }
        }
    }

    false
}

fn player_collides_with_world_pos(
    chunk_cache: &HashMap<ChunkPos, ChunkData>,
    eye_position: Vec3,
    position: WorldPos,
    block: BlockId,
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
                if i64::from(x) == position.x && y == position.y && i64::from(z) == position.z {
                    if block_is_solid(block) {
                        return true;
                    }
                    continue;
                }

                if world_block_is_solid(chunk_cache, x, y, z) {
                    return true;
                }
            }
        }
    }

    false
}

fn world_block_is_solid(chunk_cache: &HashMap<ChunkPos, ChunkData>, x: i32, y: i32, z: i32) -> bool {
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

    let Some(chunk) = chunk_cache.get(&chunk_pos) else {
        return false;
    };

    block_is_solid(chunk.voxel(local).block)
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

struct RaycastHit {
    block: WorldPos,
    previous_empty: Option<WorldPos>,
}

fn raycast_world(
    chunk_cache: &HashMap<ChunkPos, ChunkData>,
    origin: Vec3,
    direction: Vec3,
    max_distance: f32,
) -> Option<RaycastHit> {
    let direction = direction.normalize_or_zero();
    if direction == Vec3::ZERO {
        return None;
    }

    let step = 0.1;
    let steps = (max_distance / step).ceil() as usize;
    let mut previous_empty = None;

    for index in 1..=steps {
        let sample = origin + direction * (index as f32 * step);
        let world = WorldPos {
            x: sample.x.floor() as i64,
            y: sample.y.floor() as i32,
            z: sample.z.floor() as i64,
        };

        if world_block_is_solid(chunk_cache, world.x as i32, world.y, world.z as i32) {
            return Some(RaycastHit {
                block: world,
                previous_empty,
            });
        }

        previous_empty = Some(world);
    }

    None
}

fn add_wire_box(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: Vec3,
    max: Vec3,
    thickness: f32,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, max.z),
        Vec3::new(min.x, max.y, max.z),
    ];
    let edges = [
        (0, 1),
        (1, 2),
        (2, 3),
        (3, 0),
        (4, 5),
        (5, 6),
        (6, 7),
        (7, 4),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];

    for (start, end) in edges {
        add_edge_prism(vertices, indices, corners[start], corners[end], thickness, color, tile);
    }
}

fn add_edge_prism(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    start: Vec3,
    end: Vec3,
    thickness: f32,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let delta = end - start;
    let midpoint = (start + end) * 0.5;
    let half = delta * 0.5;
    let axis_x = if delta.x.abs() > 0.0 {
        Vec3::new(half.x, 0.0, 0.0)
    } else {
        Vec3::new(thickness * 0.5, 0.0, 0.0)
    };
    let axis_y = if delta.y.abs() > 0.0 {
        Vec3::new(0.0, half.y, 0.0)
    } else {
        Vec3::new(0.0, thickness * 0.5, 0.0)
    };
    let axis_z = if delta.z.abs() > 0.0 {
        Vec3::new(0.0, 0.0, half.z)
    } else {
        Vec3::new(0.0, 0.0, thickness * 0.5)
    };
    add_box_oriented(vertices, indices, midpoint, axis_x, axis_y, axis_z, color, tile);
}

fn add_box_oriented(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    center: Vec3,
    axis_x: Vec3,
    axis_y: Vec3,
    axis_z: Vec3,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let corners = [
        center - axis_x - axis_y - axis_z,
        center + axis_x - axis_y - axis_z,
        center + axis_x + axis_y - axis_z,
        center - axis_x + axis_y - axis_z,
        center - axis_x - axis_y + axis_z,
        center + axis_x - axis_y + axis_z,
        center + axis_x + axis_y + axis_z,
        center - axis_x + axis_y + axis_z,
    ];
    let uvs = atlas_quad(tile);
    add_face_indices(vertices, indices, [corners[3], corners[2], corners[1], corners[0]], color, uvs);
    add_face_indices(vertices, indices, [corners[6], corners[7], corners[4], corners[5]], color, uvs);
    add_face_indices(vertices, indices, [corners[2], corners[6], corners[5], corners[1]], color, uvs);
    add_face_indices(vertices, indices, [corners[7], corners[3], corners[0], corners[4]], color, uvs);
    add_face_indices(vertices, indices, [corners[7], corners[6], corners[2], corners[3]], color, uvs);
    add_face_indices(vertices, indices, [corners[0], corners[1], corners[5], corners[4]], color, uvs);
}

fn add_face_indices(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    positions: [Vec3; 4],
    color: [f32; 3],
    uvs: [[f32; 2]; 4],
) {
    let edge_a = positions[1] - positions[0];
    let edge_b = positions[2] - positions[0];
    let normal = edge_a.cross(edge_b).normalize_or_zero().to_array();
    let base = vertices.len() as u32;
    for (position, uv) in positions.into_iter().zip(uvs) {
        vertices.push(Vertex {
            position: position.to_array(),
            color,
            normal,
            uv,
        });
    }
    indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

#[derive(Debug)]
enum ClientCommand {
    Input { movement: [f32; 3], jump: bool },
    SubscribeChunks(SubscribeChunks),
    PlaceBlock { position: WorldPos, block: BlockId },
    BreakBlock(WorldPos),
}

#[derive(Debug)]
enum NetworkEvent {
    Welcome { message: String, spawn_position: WorldPos },
    Inventory(InventorySnapshot),
    Chunk(ChunkData),
    ChunkUnload(ChunkUnload),
    PlayerState { position: [f32; 3] },
    BlockAction(String),
    Disconnected(String),
}

struct MeshJob {
    position: ChunkPos,
    chunk: ChunkData,
}

struct MeshBuildResult {
    position: ChunkPos,
    vertices: Vec<Vertex>,
    indices: Vec<u32>,
}

fn start_network_thread(events: Sender<NetworkEvent>, commands: Receiver<ClientCommand>) {
    thread::spawn(move || {
        if let Err(error) = network_main(events.clone(), commands) {
            let _ = events.send(NetworkEvent::Disconnected(error.to_string()));
        }
    });
}

fn start_mesh_workers(jobs: Receiver<MeshJob>, results: Sender<MeshBuildResult>) {
    let worker_count = thread::available_parallelism()
        .map(|parallelism| parallelism.get().clamp(2, 4))
        .unwrap_or(2);
    let shared_jobs = Arc::new(Mutex::new(jobs));

    for index in 0..worker_count {
        let jobs = Arc::clone(&shared_jobs);
        let results = results.clone();
        thread::Builder::new()
            .name(format!("mesh-worker-{index}"))
            .spawn(move || {
                loop {
                    let job = {
                        let receiver = jobs.lock().expect("mesh job receiver poisoned");
                        receiver.recv()
                    };

                    let Ok(job) = job else {
                        break;
                    };

                    let (vertices, indices) = mesh_chunk(&job.chunk);
                    if results
                        .send(MeshBuildResult {
                            position: job.position,
                            vertices,
                            indices,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .expect("spawn mesh worker");
    }
}

fn network_main(events: Sender<NetworkEvent>, commands: Receiver<ClientCommand>) -> Result<()> {
    let mut stream = TcpStream::connect("127.0.0.1:4000").context("connect to backend")?;
    stream
        .set_read_timeout(Some(Duration::from_millis(10)))
        .context("set read timeout")?;

    write_client(
        &mut stream,
        &ClientMessage::ClientHello(ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_name: "augmego-desktop".to_string(),
        }),
    )?;
    expect_server_hello(&mut stream)?;
    write_client(
        &mut stream,
        &ClientMessage::LoginRequest(LoginRequest {
            name: "builder".to_string(),
        }),
    )?;

    let login = read_server_blocking(&mut stream)?;
    if let ServerMessage::LoginResponse(response) = login {
        let _ = events.send(NetworkEvent::Welcome {
            message: response.message,
            spawn_position: response.spawn_position,
        });
    }

    let _inventory = read_server_blocking(&mut stream)?;
    write_client(
        &mut stream,
        &ClientMessage::SubscribeChunks(SubscribeChunks {
            center: ChunkPos { x: 0, z: 0 },
            radius: CHUNK_RADIUS,
        }),
    )?;

    let mut tick = 0_u64;
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                ClientCommand::Input { movement, jump } => {
                    tick += 1;
                    write_client(
                        &mut stream,
                        &ClientMessage::PlayerInputTick(shared_protocol::PlayerInputTick {
                            tick,
                            movement,
                            jump,
                        }),
                    )?;
                }
                ClientCommand::SubscribeChunks(request) => {
                    write_client(&mut stream, &ClientMessage::SubscribeChunks(request))?;
                }
                ClientCommand::PlaceBlock { position, block } => {
                    write_client(
                        &mut stream,
                        &ClientMessage::PlaceBlockRequest(PlaceBlockRequest { position, block }),
                    )?;
                }
                ClientCommand::BreakBlock(position) => {
                    write_client(
                        &mut stream,
                        &ClientMessage::BreakBlockRequest(BreakBlockRequest { position }),
                    )?;
                }
            }
        }

        match try_read_server(&mut stream) {
            Ok(Some(message)) => match message {
                ServerMessage::ChunkData(chunk) => {
                    let _ = events.send(NetworkEvent::Chunk(chunk));
                }
                ServerMessage::ChunkUnload(unload) => {
                    let _ = events.send(NetworkEvent::ChunkUnload(unload));
                }
                ServerMessage::InventorySnapshot(snapshot) => {
                    let _ = events.send(NetworkEvent::Inventory(snapshot));
                }
                ServerMessage::PlayerStateSnapshot(state) => {
                    let _ = events.send(NetworkEvent::PlayerState { position: state.position });
                }
                ServerMessage::BlockActionResult(result) => {
                    let _ = events.send(NetworkEvent::BlockAction(result.reason));
                }
                _ => {}
            },
            Ok(None) => thread::sleep(Duration::from_millis(4)),
            Err(error) => return Err(error),
        }
    }
}

fn write_client(stream: &mut TcpStream, message: &ClientMessage) -> Result<()> {
    let bytes = frame(message)?;
    stream.write_all(&bytes).context("write client packet")
}

fn expect_server_hello(stream: &mut TcpStream) -> Result<()> {
    let message = read_server_blocking(stream)?;
    match message {
        ServerMessage::ServerHello(hello) if hello.protocol_version == PROTOCOL_VERSION => Ok(()),
        _ => anyhow::bail!("unexpected handshake response"),
    }
}

fn read_server_blocking(stream: &mut TcpStream) -> Result<ServerMessage> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length).context("read packet length")?;
    let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
    stream.read_exact(&mut payload).context("read packet payload")?;
    Ok(decode(&payload)?)
}

fn try_read_server(stream: &mut TcpStream) -> Result<Option<ServerMessage>> {
    let mut length = [0_u8; 4];
    match stream.read_exact(&mut length) {
        Ok(()) => {
            let mut payload = vec![0_u8; u32::from_le_bytes(length) as usize];
            stream.read_exact(&mut payload).context("read packet payload")?;
            Ok(Some(decode(&payload)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock || error.kind() == std::io::ErrorKind::TimedOut => Ok(None),
        Err(error) => Err(error).context("read server packet"),
    }
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
        ((0, 0, -1), Face::North),
        ((0, 0, 1), Face::South),
        ((-1, 0, 0), Face::West),
        ((1, 0, 0), Face::East),
        ((0, 1, 0), Face::Up),
        ((0, -1, 0), Face::Down),
    ];

    for (offset, face) in neighbors {
        let neighbor = sample_voxel(chunk, x + offset.0, y + offset.1, z + offset.2);
        if neighbor.map(|voxel| voxel.block.is_transparent()).unwrap_or(true) {
            let shadow = skylight_shadow(chunk, x + offset.0, y.max(0), z + offset.2);
            let color = shaded_face_color(base_color, face, shadow);
            let face = face_vertices(world, face, color, tile_uvs(block, face));
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

fn skylight_shadow(chunk: &ChunkData, x: i32, y: i32, z: i32) -> f32 {
    if !(0..CHUNK_WIDTH).contains(&x) || !(0..CHUNK_DEPTH).contains(&z) {
        return 1.0;
    }

    let mut light = 1.0_f32;
    for yy in (y + 1).max(0)..CHUNK_HEIGHT {
        let Some(voxel) = sample_voxel(chunk, x, yy, z) else {
            break;
        };

        light *= match voxel.block {
            BlockId::Air => 1.0,
            BlockId::Glass | BlockId::Water => 0.96,
            BlockId::Leaves => 0.72,
            _ => 0.52,
        };

        if light <= 0.35 || !matches!(voxel.block, BlockId::Air | BlockId::Glass | BlockId::Water | BlockId::Leaves) {
            break;
        }
    }

    light.clamp(0.35, 1.0)
}

fn shaded_face_color(base: [f32; 3], face: Face, shadow: f32) -> [f32; 3] {
    let directional = match face {
        Face::Up => brighten(base, 0.08),
        Face::Down => darken(base, 0.22),
        Face::North | Face::South => darken(base, 0.08),
        Face::East | Face::West => darken(base, 0.02),
    };

    [
        directional[0] * shadow,
        directional[1] * shadow,
        directional[2] * shadow,
    ]
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
    let normal = face_normal(face);
    match face {
        Face::North => [
            v(x, y + 1.0, z, color, normal, uvs[0]),
            v(x + 1.0, y + 1.0, z, color, normal, uvs[1]),
            v(x + 1.0, y, z, color, normal, uvs[2]),
            v(x, y, z, color, normal, uvs[3]),
        ],
        Face::South => [
            v(x + 1.0, y + 1.0, z + 1.0, color, normal, uvs[0]),
            v(x, y + 1.0, z + 1.0, color, normal, uvs[1]),
            v(x, y, z + 1.0, color, normal, uvs[2]),
            v(x + 1.0, y, z + 1.0, color, normal, uvs[3]),
        ],
        Face::East => [
            v(x + 1.0, y + 1.0, z, color, normal, uvs[0]),
            v(x + 1.0, y + 1.0, z + 1.0, color, normal, uvs[1]),
            v(x + 1.0, y, z + 1.0, color, normal, uvs[2]),
            v(x + 1.0, y, z, color, normal, uvs[3]),
        ],
        Face::West => [
            v(x, y + 1.0, z + 1.0, color, normal, uvs[0]),
            v(x, y + 1.0, z, color, normal, uvs[1]),
            v(x, y, z, color, normal, uvs[2]),
            v(x, y, z + 1.0, color, normal, uvs[3]),
        ],
        Face::Up => [
            v(x, y + 1.0, z, color, normal, uvs[0]),
            v(x, y + 1.0, z + 1.0, color, normal, uvs[1]),
            v(x + 1.0, y + 1.0, z + 1.0, color, normal, uvs[2]),
            v(x + 1.0, y + 1.0, z, color, normal, uvs[3]),
        ],
        Face::Down => [
            v(x, y, z, color, normal, uvs[0]),
            v(x + 1.0, y, z, color, normal, uvs[1]),
            v(x + 1.0, y, z + 1.0, color, normal, uvs[2]),
            v(x, y, z + 1.0, color, normal, uvs[3]),
        ],
    }
}

fn v(x: f32, y: f32, z: f32, color: [f32; 3], normal: [f32; 3], uv: [f32; 2]) -> Vertex {
    Vertex {
        position: [x, y, z],
        color,
        normal,
        uv,
    }
}

fn face_normal(face: Face) -> [f32; 3] {
    match face {
        Face::North => [0.0, 0.0, -1.0],
        Face::South => [0.0, 0.0, 1.0],
        Face::East => [1.0, 0.0, 0.0],
        Face::West => [-1.0, 0.0, 0.0],
        Face::Up => [0.0, 1.0, 0.0],
        Face::Down => [0.0, -1.0, 0.0],
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

    [
        [min_u, min_v],
        [max_u, min_v],
        [max_u, max_v],
        [min_u, max_v],
    ]
}

fn world_to_chunk(position: Vec3) -> ChunkPos {
    ChunkPos::from_world(WorldPos {
        x: position.x.floor() as i64,
        y: position.y.floor() as i32,
        z: position.z.floor() as i64,
    })
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

fn set_mouse_capture(window: &Window, captured: bool) {
    if captured {
        let _ = window
            .set_cursor_grab(CursorGrabMode::Locked)
            .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
        window.set_cursor_visible(false);
    } else {
        let _ = window.set_cursor_grab(CursorGrabMode::None);
        window.set_cursor_visible(true);
    }
}
