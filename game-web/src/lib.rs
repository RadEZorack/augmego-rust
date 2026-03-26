#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, WorldPos};
use shared_protocol::{
    BreakBlockRequest, ClientHello, ClientMessage, ClientWebRtcSignal, InventorySnapshot, LoginRequest,
    PROTOCOL_VERSION, PlaceBlockRequest, PlayerInputTick, ServerMessage, ServerWebRtcSignal,
    SubscribeChunks, WebRtcSignalPayload, decode, encode,
};
use shared_world::{BlockId, ChunkData, TerrainGenerator};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_time::Instant;
use web_sys::{
    BinaryType, CanvasRenderingContext2d, CloseEvent, Document, Element, ErrorEvent,
    Event as WebEvent, HtmlCanvasElement, HtmlVideoElement, MediaStream, MediaStreamConstraints,
    MessageEvent, RtcIceCandidateInit, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
    RtcSessionDescriptionInit, RtcTrackEvent, WebSocket, Worker,
};
use wgpu_lite::{DynamicTexture, Mesh, Renderer, TexturedMesh, Vertex};
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::WindowExtWebSys;
use winit::window::WindowBuilder;

const WEB_RADIUS: i32 = 6;
#[allow(dead_code)]
const INITIAL_WEB_RADIUS: i32 = 1;
const SPAWN_READY_RADIUS: i32 = 1;
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
const CROSSHAIR_DISTANCE: f32 = 0.6;
const CROSSHAIR_LENGTH: f32 = 0.035;
const CROSSHAIR_THICKNESS: f32 = 0.004;
const TARGET_OUTLINE_THICKNESS: f32 = 0.035;
const LINK_PANEL_OPEN_URL: &str = "https://www.google.com";
const LINK_PANEL_LABEL_URL: &str = "google.com";
const LINK_PANEL_HALF_WIDTH: f32 = 1.2;
const LINK_PANEL_HALF_HEIGHT: f32 = 0.75;
const LINK_PANEL_HALF_DEPTH: f32 = 0.03;
const LINK_PANEL_TILE: (u32, u32) = (8, 4);
const REMOTE_MEDIA_PLACEHOLDER_TILE: (u32, u32) = (0, 0);
const WEBCAM_SOURCE_SIZE: usize = 64;
const REMOTE_PLAYER_HALF_WIDTH: f32 = 0.35;
const REMOTE_PLAYER_HALF_HEIGHT: f32 = 0.9;
const WEBCAM_PANEL_HALF_WIDTH: f32 = 0.55;
const WEBCAM_PANEL_HALF_HEIGHT: f32 = 0.40;

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
    let (network_rx, websocket, websocket_handlers) = start_websocket_client()?;
    let (webcam_tx, webcam_rx) = mpsc::channel();
    let mut app = WebApp::new(
        renderer.size(),
        canvas,
        workers,
        worker_onmessage,
        mesh_result_rx,
        network_rx,
        websocket,
        websocket_handlers,
        webcam_tx,
        webcam_rx,
    );
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
                    app.process_webcam_events();
                    app.process_generation_updates(&renderer, &mut chunk_meshes, DEFAULT_GENERATION_BUDGET_PER_UPDATE);
                    app.tick();
                    renderer.update_camera(app.camera_matrix());
                    let visible_meshes = chunk_meshes
                        .iter()
                        .filter_map(|(position, mesh)| app.chunk_is_visible(*position).then_some(mesh))
                        .collect::<Vec<_>>();
                    let link_panel_mesh = app.build_link_panel_mesh(&renderer);
                    let mut visible_mesh_refs = visible_meshes;
                    if let Some(mesh) = &link_panel_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    let remote_players_mesh = app.build_remote_players_mesh(&renderer);
                    if let Some(mesh) = &remote_players_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    let remote_media_placeholder_mesh = app.build_remote_media_placeholder_mesh(&renderer);
                    if let Some(mesh) = &remote_media_placeholder_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    app.update_remote_media_textures(&renderer);
                    let textured_meshes = app.build_remote_media_meshes(&renderer);
                    let textured_mesh_refs = textured_meshes.iter().collect::<Vec<_>>();
                    let mut overlay_meshes = Vec::new();
                    if let Some(mesh) = app.build_crosshair_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    if let Some(mesh) = app.build_target_highlight_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    let overlay_refs = overlay_meshes.iter().collect::<Vec<_>>();

                    if let Err(error) = renderer.render(&visible_mesh_refs, &textured_mesh_refs, &overlay_refs) {
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
    authoritative_chunks: HashMap<ChunkPos, ChunkData>,
    collision_voxels: HashMap<ChunkPos, Vec<u16>>,
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
    spawn_settled: bool,
    chunk_edits: HashMap<ChunkPos, HashMap<(u8, u8, u8), BlockId>>,
    link_panel: LinkPanel,
    hotbar_slots: Vec<Element>,
    hotbar_blocks: Vec<BlockId>,
    selected_hotbar: usize,
    player_id: Option<u64>,
    remote_players: HashMap<u64, [f32; 3]>,
    remote_media: HashMap<u64, RemotePeerMedia>,
    webcam_requested: bool,
    webcam_tx: Sender<WebcamEvent>,
    webcam_rx: Receiver<WebcamEvent>,
    webcam: Option<WebcamCapture>,
    last_sent_position: Option<[f32; 3]>,
    last_sent_velocity: Option<[f32; 3]>,
    tick_counter: u64,
    transport_open: bool,
    logged_in: bool,
    network_rx: Receiver<NetworkEvent>,
    websocket: WebSocket,
    _websocket_bindings: WebSocketBindings,
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
        network_rx: Receiver<NetworkEvent>,
        websocket: WebSocket,
        websocket_bindings: WebSocketBindings,
        webcam_tx: Sender<WebcamEvent>,
        webcam_rx: Receiver<WebcamEvent>,
    ) -> Self {
        let mut camera = Camera::default();
        camera.position = Vec3::new(0.5, PLAYER_EYE_HEIGHT + 96.0, 0.5);
        let link_panel = LinkPanel::near_spawn(camera.position);
        let hotbar_blocks = vec![
            BlockId::Grass,
            BlockId::Stone,
            BlockId::Planks,
            BlockId::Glass,
            BlockId::Lantern,
        ];
        let hotbar_slots = create_hotbar(&hotbar_blocks);
        update_hotbar_ui(&hotbar_slots, &hotbar_blocks, 0);
        let current_chunk = chunk_from_world_position(camera.position);
        let desired_chunks = HashSet::new();
        let pending_generation = VecDeque::new();

        Self {
            canvas,
            camera,
            authoritative_chunks: HashMap::new(),
            collision_voxels: HashMap::new(),
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
            spawn_settled: false,
            chunk_edits: HashMap::new(),
            link_panel,
            hotbar_slots,
            hotbar_blocks,
            selected_hotbar: 0,
            player_id: None,
            remote_players: HashMap::new(),
            remote_media: HashMap::new(),
            webcam_requested: false,
            webcam_tx,
            webcam_rx,
            webcam: None,
            last_sent_position: None,
            last_sent_velocity: None,
            tick_counter: 0,
            transport_open: false,
            logged_in: false,
            network_rx,
            websocket,
            _websocket_bindings: websocket_bindings,
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

        if event.state == ElementState::Pressed {
            match code {
                KeyCode::Digit1 => self.set_selected_hotbar(0),
                KeyCode::Digit2 => self.set_selected_hotbar(1),
                KeyCode::Digit3 => self.set_selected_hotbar(2),
                KeyCode::Digit4 => self.set_selected_hotbar(3),
                KeyCode::Digit5 => self.set_selected_hotbar(4),
                KeyCode::Digit6 => self.set_selected_hotbar(5),
                KeyCode::Digit7 => self.set_selected_hotbar(6),
                KeyCode::Digit8 => self.set_selected_hotbar(7),
                KeyCode::Digit9 => self.set_selected_hotbar(8),
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
    }

    fn set_selected_hotbar(&mut self, index: usize) {
        if index < self.hotbar_blocks.len() {
            self.selected_hotbar = index;
            update_hotbar_ui(&self.hotbar_slots, &self.hotbar_blocks, self.selected_hotbar);
        }
    }

    fn selected_hotbar_block(&self) -> BlockId {
        self.hotbar_blocks
            .get(self.selected_hotbar)
            .copied()
            .unwrap_or(BlockId::Stone)
    }

    fn handle_mouse_motion(&mut self, dx: f32, dy: f32) {
        self.camera.yaw -= dx * 0.0025;
        self.camera.pitch = (self.camera.pitch - dy * 0.0025).clamp(-1.45, 1.45);
    }

    fn handle_mouse_button(&mut self, button: MouseButton) {
        if !self.mouse_captured {
            self.canvas.request_pointer_lock();
            self.mouse_captured = pointer_is_locked(&self.canvas);
            self.ensure_webcam_requested();
            return;
        }

        if !self.logged_in {
            return;
        }

        let Some(target) = self.current_interaction_target() else {
            return;
        };

        match target {
            InteractionTarget::Link if button == MouseButton::Left => {
                if confirm_open_url(LINK_PANEL_LABEL_URL) {
                    open_url(LINK_PANEL_OPEN_URL);
                }
            }
            InteractionTarget::Block(hit) => match button {
                MouseButton::Left => {
                    self.apply_local_block_edit(hit.block, BlockId::Air);
                }
                MouseButton::Right => {
                    let Some(place_at) = hit.previous_empty else {
                        return;
                    };

                    let selected_block = self.selected_hotbar_block();
                    if self.player_collides_with_world_pos(self.camera.position, place_at, selected_block) {
                        return;
                    }

                    self.apply_local_block_edit(place_at, selected_block);
                }
                _ => {}
            },
            InteractionTarget::Link => {}
        }
    }

    fn ensure_webcam_requested(&mut self) {
        if self.webcam_requested {
            return;
        }

        self.webcam_requested = true;
        request_webcam_capture(self.webcam_tx.clone());
    }

    fn process_webcam_events(&mut self) {
        while let Ok(event) = self.webcam_rx.try_recv() {
            match event {
                WebcamEvent::Ready(capture) => {
                    attach_local_webcam_overlay(&capture.video);
                    self.webcam = Some(capture);
                    let remote_ids = self.remote_players.keys().copied().collect::<Vec<_>>();
                    for remote_id in remote_ids {
                        self.ensure_peer_connection(remote_id);
                        self.maybe_enable_peer_media(remote_id);
                    }
                }
                WebcamEvent::Failed(_message) => {}
            }
        }

        REMOTE_MEDIA_REGISTRY.with(|registry| {
            let mut registry = registry.borrow_mut();
            for (player_id, registration) in registry.drain() {
                if let Some(remote) = self.remote_media.get_mut(&player_id) {
                    remote.video = Some(registration.video);
                    remote.canvas = Some(registration.canvas);
                    remote.context = Some(registration.context);
                }
            }
        });
    }

    fn drain_network(&mut self) {
        while let Ok(event) = self.network_rx.try_recv() {
            match event {
                NetworkEvent::Opened => {
                    self.transport_open = true;
                    self.send_client_message(&ClientMessage::ClientHello(ClientHello {
                        protocol_version: PROTOCOL_VERSION,
                        client_name: "game-web".to_string(),
                    }));
                }
                NetworkEvent::Server(message) => match message {
                    ServerMessage::ServerHello(_) => {
                        self.send_client_message(&ClientMessage::LoginRequest(LoginRequest {
                            name: "Web Player".to_string(),
                        }));
                    }
                    ServerMessage::LoginResponse(response) => {
                        if response.accepted {
                            self.logged_in = true;
                            self.player_id = Some(response.player_id);
                            self.remote_players.clear();
                            self.remote_media.clear();
                            self.last_sent_position = None;
                            self.last_sent_velocity = None;
                            self.camera.position = Vec3::new(
                                response.spawn_position.x as f32 + 0.5,
                                response.spawn_position.y as f32 + PLAYER_EYE_HEIGHT,
                                response.spawn_position.z as f32 + 0.5,
                            );
                            self.camera.vertical_velocity = 0.0;
                            self.camera.on_ground = false;
                            self.spawn_settled = false;
                            self.current_chunk = chunk_from_world_position(self.camera.position);
                            self.desired_chunks = desired_chunk_set(self.current_chunk, WEB_RADIUS);
                            self.send_chunk_subscription(self.current_chunk);
                            self.link_panel = LinkPanel::near_spawn(self.camera.position);
                        }
                    }
                    ServerMessage::ChunkData(chunk) => {
                        let position = chunk.position;
                        self.authoritative_chunks.insert(position, chunk);
                        if self.desired_chunks.contains(&position) {
                            self.schedule_chunk_rebuild(position);
                        }
                    }
                    ServerMessage::ChunkUnload(unload) => {
                        for position in unload.positions {
                            self.authoritative_chunks.remove(&position);
                            self.collision_voxels.remove(&position);
                            self.chunk_edits.remove(&position);
                            self.pending_generation.retain(|pending| *pending != position);
                            self.inflight_generation.remove(&position);
                            self.dirty_generation.remove(&position);
                        }
                    }
                    ServerMessage::InventorySnapshot(InventorySnapshot { slots }) => {
                        self.hotbar_blocks = slots.into_iter().map(|slot| slot.block).collect();
                        if self.hotbar_blocks.is_empty() {
                            self.hotbar_blocks = vec![BlockId::Grass, BlockId::Stone, BlockId::Planks];
                        }
                        if self.selected_hotbar >= self.hotbar_blocks.len() {
                            self.selected_hotbar = self.hotbar_blocks.len().saturating_sub(1);
                        }
                        update_hotbar_ui(&self.hotbar_slots, &self.hotbar_blocks, self.selected_hotbar);
                    }
                    ServerMessage::PlayerStateSnapshot(snapshot) => {
                        if Some(snapshot.player_id) != self.player_id {
                            self.remote_players.insert(snapshot.player_id, snapshot.position);
                            self.ensure_peer_connection(snapshot.player_id);
                        }
                    }
                    ServerMessage::WebRtcSignal(signal) => self.handle_webrtc_signal(signal),
                    ServerMessage::BlockActionResult(result) => {
                        if !result.accepted {
                            web_sys::console::warn_1(&JsValue::from_str(&result.reason));
                        }
                    }
                    ServerMessage::ChunkDelta(_) | ServerMessage::ChatMessage(_) => {}
                },
                NetworkEvent::Disconnected(reason) => {
                    self.transport_open = false;
                    self.logged_in = false;
                    self.player_id = None;
                    self.remote_players.clear();
                    self.remote_media.clear();
                    web_sys::console::error_1(&JsValue::from_str(&format!("multiplayer disconnected: {reason}")));
                }
            }
        }
    }

    fn send_client_message(&self, message: &ClientMessage) {
        if !self.transport_open {
            return;
        }
        match encode(message) {
            Ok(bytes) => {
                let _ = self.websocket.send_with_u8_array(&bytes);
            }
            Err(error) => {
                web_sys::console::error_1(&JsValue::from_str(&format!("encode client message: {error}")));
            }
        }
    }

    fn send_chunk_subscription(&self, center: ChunkPos) {
        if !self.logged_in {
            return;
        }
        self.send_client_message(&ClientMessage::SubscribeChunks(SubscribeChunks {
            center,
            radius: WEB_RADIUS as u8,
        }));
    }

    fn tick(&mut self) {
        self.mouse_captured = pointer_is_locked(&self.canvas);
        self.drain_network();
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

        if !self.spawn_settled {
            if self.ensure_clear_spawn_space() {
                self.spawn_settled = true;
            } else {
                return;
            }
        }

        let mut movement_for_server = Vec3::new(movement.x, 0.0, movement.z);
        if movement_for_server.length_squared() > 1.0 {
            movement_for_server = movement_for_server.normalize();
        }
        let forward = Vec3::new(self.camera.yaw.sin(), 0.0, self.camera.yaw.cos()).normalize_or_zero();
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let world_movement = forward * -movement_for_server.z + right * movement_for_server.x;

        let previous_position = self.camera.position;
        self.update_camera_physics(dt, movement, jump, sprint);
        if !self.logged_in {
            return;
        }
        let dt_secs = dt.as_secs_f32();
        let actual_velocity = if dt_secs > 0.0 {
            (self.camera.position - previous_position) / dt_secs
        } else {
            Vec3::ZERO
        };
        let position = self.camera.position.to_array();
        let velocity = [actual_velocity.x, self.camera.vertical_velocity, actual_velocity.z];
        let input_active = movement != Vec3::ZERO || jump || sprint;
        let should_broadcast_motion = input_active || !self.camera.on_ground;
        let position_changed = self
            .last_sent_position
            .map(|last| {
                let dx = position[0] - last[0];
                let dy = position[1] - last[1];
                let dz = position[2] - last[2];
                dx * dx + dy * dy + dz * dz > 0.0001
            })
            .unwrap_or(true);
        let velocity_changed = self
            .last_sent_velocity
            .map(|last| {
                let dx = velocity[0] - last[0];
                let dy = velocity[1] - last[1];
                let dz = velocity[2] - last[2];
                dx * dx + dy * dy + dz * dz > 0.0025
            })
            .unwrap_or(true);

        if should_broadcast_motion && (position_changed || velocity_changed) {
            self.tick_counter = self.tick_counter.wrapping_add(1);
            self.send_client_message(&ClientMessage::PlayerInputTick(PlayerInputTick {
                tick: self.tick_counter,
                movement: [world_movement.x, 0.0, world_movement.z],
                position: Some(position),
                velocity: Some(velocity),
                jump,
            }));
            self.last_sent_position = Some(position);
            self.last_sent_velocity = Some(velocity);
        }
    }

    fn camera_matrix(&self) -> Mat4 {
        let aspect = self.size.width as f32 / self.size.height.max(1) as f32;
        self.camera.matrix(aspect)
    }

    fn current_target(&mut self) -> Option<RaycastHit> {
        self.raycast_world(6.0)
    }

    fn current_interaction_target(&mut self) -> Option<InteractionTarget> {
        let block_hit = self.current_target();
        let link_hit = self.current_link_target();

        match (block_hit, link_hit) {
            (Some(block), Some(link)) => {
                if link.distance < block.distance {
                    Some(InteractionTarget::Link)
                } else {
                    Some(InteractionTarget::Block(block))
                }
            }
            (Some(block), None) => Some(InteractionTarget::Block(block)),
            (None, Some(_)) => Some(InteractionTarget::Link),
            (None, None) => None,
        }
    }

    fn current_link_target(&self) -> Option<LinkHit> {
        raycast_link_panel(self.camera.position, self.camera.forward(), self.link_panel)
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

    fn build_target_highlight_mesh(&mut self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let InteractionTarget::Block(target) = self.current_interaction_target()? else {
            return None;
        };
        let face = target.face?;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_face_highlight(
            &mut vertices,
            &mut indices,
            target.block,
            face,
            TARGET_OUTLINE_THICKNESS,
            [1.0, 0.95, 0.45],
            (3, 1),
        );
        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn build_link_panel_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_link_panel_mesh(&mut vertices, &mut indices, self.link_panel, [1.0, 1.0, 1.0], LINK_PANEL_TILE);
        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn build_remote_players_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        if self.remote_players.is_empty() {
            return None;
        }

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (&player_id, position) in &self.remote_players {
            let tint = remote_player_color(player_id);
            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            let center = (anchor.body + anchor.head) * 0.5;
            add_box_oriented(
                &mut vertices,
                &mut indices,
                center,
                Vec3::new(REMOTE_PLAYER_HALF_WIDTH, 0.0, 0.0),
                Vec3::new(0.0, REMOTE_PLAYER_HALF_HEIGHT, 0.0),
                Vec3::new(0.0, 0.0, REMOTE_PLAYER_HALF_WIDTH),
                tint,
                (2, 0),
            );
        }

        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn build_remote_media_placeholder_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        if self.remote_players.is_empty() {
            return None;
        }

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for (&player_id, position) in &self.remote_players {
            if self
                .remote_media
                .get(&player_id)
                .and_then(|media| media.texture.as_ref())
                .is_some()
            {
                continue;
            }

            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            add_media_panel_billboard(
                &mut vertices,
                &mut indices,
                anchor.media,
                self.camera.position,
                [0.14, 0.14, 0.16],
                atlas_quad_raw(REMOTE_MEDIA_PLACEHOLDER_TILE),
            );
        }

        (!vertices.is_empty()).then(|| renderer.create_mesh(&vertices, &indices))
    }

    fn build_remote_media_meshes(&self, renderer: &Renderer<'_>) -> Vec<TexturedMesh> {
        let mut meshes = Vec::new();
        for (&player_id, position) in &self.remote_players {
            let Some(texture) = self.remote_media.get(&player_id).and_then(|media| media.texture.as_ref()) else {
                continue;
            };
            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            add_media_panel_billboard(
                &mut vertices,
                &mut indices,
                anchor.media,
                self.camera.position,
                [1.0, 1.0, 1.0],
                full_uv_quad(),
            );
            meshes.push(renderer.create_textured_mesh(&vertices, &indices, texture));
        }
        meshes
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
                self.collision_voxels
                    .insert(result.position, result.voxels.clone());
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

            let Some(chunk) = self.authoritative_chunks.get(&position).cloned() else {
                continue;
            };
            let worker_index = self.next_worker_index % self.workers.len();
            dispatch_chunk_mesh_job(&self.workers[worker_index], &chunk);
            self.next_worker_index = (self.next_worker_index + 1) % self.workers.len();
            self.inflight_generation.insert(position);
        }
    }

    fn update_remote_media_textures(&mut self, renderer: &Renderer<'_>) {
        for media in self.remote_media.values_mut() {
            let (Some(video), Some(canvas), Some(context)) =
                (media.video.as_ref(), media.canvas.as_ref(), media.context.as_ref())
            else {
                continue;
            };
            if video.video_width() == 0 || video.video_height() == 0 {
                continue;
            }
            if media.texture.is_none() {
                media.texture = Some(
                    renderer.create_dynamic_texture(WEBCAM_SOURCE_SIZE as u32, WEBCAM_SOURCE_SIZE as u32),
                );
            }

            let width = canvas.width() as f64;
            let height = canvas.height() as f64;
            let _ = context.draw_image_with_html_video_element_and_dw_and_dh(video, 0.0, 0.0, width, height);
            let Ok(image_data) = context.get_image_data(0.0, 0.0, width, height) else {
                continue;
            };
            if let Some(texture) = &media.texture {
                renderer.update_dynamic_texture_rgba(texture, &image_data.data().0);
            }
        }
    }

    fn maybe_enable_peer_media(&mut self, remote_player_id: u64) {
        let (Some(local_player_id), Some(webcam)) = (self.player_id, self.webcam.as_ref()) else {
            return;
        };
        let Some(remote) = self.remote_media.get_mut(&remote_player_id) else {
            return;
        };

        if !remote.local_tracks_attached {
            let tracks = webcam.stream.get_tracks();
            for index in 0..tracks.length() {
                if let Ok(track) = tracks.get(index).dyn_into::<web_sys::MediaStreamTrack>() {
                    let args = js_sys::Array::new();
                    args.push(&track);
                    args.push(&webcam.stream);
                    if let Ok(add_track) =
                        js_sys::Reflect::get(remote.connection.as_ref(), &JsValue::from_str("addTrack"))
                    {
                        if let Ok(add_track) = add_track.dyn_into::<js_sys::Function>() {
                            let _ = add_track.apply(remote.connection.as_ref(), &args);
                        }
                    }
                }
            }
            remote.local_tracks_attached = true;
        }

        if local_player_id < remote_player_id && !remote.offer_started {
            remote.offer_started = true;
            let connection = remote.connection.clone();
            let ws = self.websocket.clone();
            spawn_local(async move {
                let Ok(offer) = JsFuture::from(connection.create_offer()).await else {
                    return;
                };
                let Some(sdp) = js_sys::Reflect::get(&offer, &JsValue::from_str("sdp"))
                    .ok()
                    .and_then(|value| value.as_string())
                else {
                    return;
                };
                let description = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                description.set_sdp(&sdp);
                if JsFuture::from(connection.set_local_description(&description)).await.is_ok() {
                    send_client_message_over_websocket(
                        &ws,
                        &ClientMessage::WebRtcSignal(ClientWebRtcSignal {
                            target_player_id: remote_player_id,
                            payload: WebRtcSignalPayload::Offer { sdp },
                        }),
                    );
                }
            });
        }
    }

    fn ensure_peer_connection(&mut self, remote_player_id: u64) {
        let Some(local_player_id) = self.player_id else {
            return;
        };
        if local_player_id == remote_player_id {
            return;
        }
        if self.remote_media.contains_key(&remote_player_id) {
            self.maybe_enable_peer_media(remote_player_id);
            return;
        }

        let Ok(connection) = RtcPeerConnection::new() else {
            return;
        };

        let ws_for_ice = self.websocket.clone();
        let onicecandidate = Closure::wrap(Box::new(move |event: RtcPeerConnectionIceEvent| {
            let Some(candidate) = event.candidate() else {
                return;
            };
            let signal = ClientMessage::WebRtcSignal(ClientWebRtcSignal {
                target_player_id: remote_player_id,
                payload: WebRtcSignalPayload::IceCandidate {
                    candidate: candidate.candidate(),
                    sdp_mid: candidate.sdp_mid(),
                    sdp_mline_index: candidate.sdp_m_line_index(),
                },
            });
            send_client_message_over_websocket(&ws_for_ice, &signal);
        }) as Box<dyn FnMut(RtcPeerConnectionIceEvent)>);
        connection.set_onicecandidate(Some(onicecandidate.as_ref().unchecked_ref()));

        let ontrack = Closure::wrap(Box::new(move |event: RtcTrackEvent| {
            let streams = event.streams();
            if streams.length() == 0 {
                return;
            }
            let Some(window) = web_sys::window() else {
                return;
            };
            let Some(document) = window.document() else {
                return;
            };
            let Ok(video_element) = document.create_element("video") else {
                return;
            };
            let Ok(video) = video_element.dyn_into::<HtmlVideoElement>() else {
                return;
            };
            video.set_autoplay(true);
            let _ = video.set_attribute("playsinline", "true");
            let _ = video.set_attribute("style", "display:none;");
            let stream = streams.get(0);
            if let Ok(media_stream) = stream.dyn_into::<MediaStream>() {
                video.set_src_object(Some(&media_stream));
                let _ = video.play();
            }
            let Ok(canvas_element) = document.create_element("canvas") else {
                return;
            };
            let Ok(canvas) = canvas_element.dyn_into::<HtmlCanvasElement>() else {
                return;
            };
            canvas.set_width(WEBCAM_SOURCE_SIZE as u32);
            canvas.set_height(WEBCAM_SOURCE_SIZE as u32);
            let options = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&options, &JsValue::from_str("willReadFrequently"), &JsValue::TRUE);
            let Ok(Some(context_value)) = canvas.get_context_with_context_options("2d", &options) else {
                return;
            };
            let Ok(context) = context_value.dyn_into::<CanvasRenderingContext2d>() else {
                return;
            };
            REMOTE_MEDIA_REGISTRY.with(|registry| {
                registry.borrow_mut().insert(
                    remote_player_id,
                    RemoteMediaRegistration {
                        video,
                        canvas,
                        context,
                    },
                );
            });
        }) as Box<dyn FnMut(RtcTrackEvent)>);
        connection.set_ontrack(Some(ontrack.as_ref().unchecked_ref()));

        let remote = RemotePeerMedia {
            connection: connection.clone(),
            video: None,
            canvas: None,
            context: None,
            texture: None,
            local_tracks_attached: false,
            offer_started: false,
            _onicecandidate: onicecandidate,
            _ontrack: ontrack,
        };
        self.remote_media.insert(remote_player_id, remote);
        self.maybe_enable_peer_media(remote_player_id);
    }

    fn handle_webrtc_signal(&mut self, signal: ServerWebRtcSignal) {
        self.ensure_peer_connection(signal.source_player_id);
        let Some(remote) = self.remote_media.get(&signal.source_player_id) else {
            return;
        };
        let connection = remote.connection.clone();
        let websocket = self.websocket.clone();
        let target_player_id = signal.source_player_id;
        match signal.payload {
            WebRtcSignalPayload::Offer { sdp } => {
                let source_player_id = signal.source_player_id;
                spawn_local(async move {
                    let description = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
                    description.set_sdp(&sdp);
                    if JsFuture::from(connection.set_remote_description(&description)).await.is_err() {
                        return;
                    }
                    flush_pending_ice_candidates(source_player_id, &connection);
                    let Ok(answer) = JsFuture::from(connection.create_answer()).await else {
                        return;
                    };
                    let Some(answer_sdp) = js_sys::Reflect::get(&answer, &JsValue::from_str("sdp"))
                        .ok()
                        .and_then(|value| value.as_string())
                    else {
                        return;
                    };
                    let answer_description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                    answer_description.set_sdp(&answer_sdp);
                    if JsFuture::from(connection.set_local_description(&answer_description)).await.is_ok() {
                        send_client_message_over_websocket(
                            &websocket,
                            &ClientMessage::WebRtcSignal(ClientWebRtcSignal {
                                target_player_id,
                                payload: WebRtcSignalPayload::Answer { sdp: answer_sdp },
                            }),
                        );
                    }
                });
            }
            WebRtcSignalPayload::Answer { sdp } => {
                let source_player_id = signal.source_player_id;
                spawn_local(async move {
                    let description = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
                    description.set_sdp(&sdp);
                    if JsFuture::from(connection.set_remote_description(&description)).await.is_ok() {
                        flush_pending_ice_candidates(source_player_id, &connection);
                    }
                });
            }
            WebRtcSignalPayload::IceCandidate {
                candidate,
                sdp_mid,
                sdp_mline_index,
            } => {
                let remote_description_ready = js_sys::Reflect::get(
                    connection.as_ref(),
                    &JsValue::from_str("remoteDescription"),
                )
                .ok()
                .is_some_and(|value| !value.is_null() && !value.is_undefined());
                if !remote_description_ready {
                    PENDING_ICE_REGISTRY.with(|registry| {
                        registry
                            .borrow_mut()
                            .entry(signal.source_player_id)
                            .or_default()
                            .push(PendingIceCandidate {
                                candidate,
                                sdp_mid,
                                sdp_mline_index,
                            });
                    });
                    return;
                }
                let init = RtcIceCandidateInit::new(&candidate);
                if let Some(mid) = sdp_mid.as_deref() {
                    init.set_sdp_mid(Some(mid));
                }
                if let Some(index) = sdp_mline_index {
                    init.set_sdp_m_line_index(Some(index));
                }
                let _ = connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init));
            }
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
        self.send_chunk_subscription(self.current_chunk);
        chunk_meshes.retain(|position, _| self.desired_chunks.contains(position));
        self.authoritative_chunks
            .retain(|position, _| self.desired_chunks.contains(position));
        self.collision_voxels
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

    fn reprioritize_pending_generation(&mut self, _chunk_meshes: &HashMap<ChunkPos, Mesh>) {
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

        if let Some(voxels) = self.collision_voxels.get(&chunk_pos) {
            let index = usize::from(local.y) * CHUNK_WIDTH as usize * CHUNK_DEPTH as usize
                + usize::from(local.z) * CHUNK_WIDTH as usize
                + usize::from(local.x);
            return voxels
                .get(index)
                .copied()
                .map(|block| block_is_solid(block_from_id(block)))
                .unwrap_or(false);
        }

        false
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
                    face: previous_empty.and_then(|empty| face_from_empty_neighbor(world, empty)),
                    distance: index as f32 * step,
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

        if let Some(chunk) = self.authoritative_chunks.get_mut(&chunk_pos) {
            chunk.set_voxel(local, shared_world::Voxel { block });
        }
        self.schedule_chunk_rebuild(chunk_pos);

        match block {
            BlockId::Air => self.send_client_message(&ClientMessage::BreakBlockRequest(BreakBlockRequest {
                position,
            })),
            _ => self.send_client_message(&ClientMessage::PlaceBlockRequest(PlaceBlockRequest {
                position,
                block,
            })),
        }
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

    fn ensure_clear_spawn_space(&mut self) -> bool {
        if !self.spawn_area_ready() {
            return false;
        }

        if !self.player_collides(self.camera.position) {
            return true;
        }

        for lift in 1..=12 {
            let mut candidate = self.camera.position;
            candidate.y += lift as f32;
            if !self.player_collides(candidate) {
                self.camera.position = candidate;
                self.camera.vertical_velocity = 0.0;
                self.camera.on_ground = false;
                return true;
            }
        }

        false
    }

    fn spawn_area_ready(&self) -> bool {
        for dz in -SPAWN_READY_RADIUS..=SPAWN_READY_RADIUS {
            for dx in -SPAWN_READY_RADIUS..=SPAWN_READY_RADIUS {
                let chunk = ChunkPos {
                    x: self.current_chunk.x + dx,
                    z: self.current_chunk.z + dz,
                };
                if !self.collision_voxels.contains_key(&chunk) {
                    return false;
                }
            }
        }
        true
    }
}

struct WebcamCapture {
    stream: MediaStream,
    video: HtmlVideoElement,
}

#[derive(Clone, Copy)]
struct PlayerAnchor {
    body: Vec3,
    head: Vec3,
    media: Vec3,
}

struct RemotePeerMedia {
    connection: RtcPeerConnection,
    video: Option<HtmlVideoElement>,
    canvas: Option<HtmlCanvasElement>,
    context: Option<CanvasRenderingContext2d>,
    texture: Option<DynamicTexture>,
    local_tracks_attached: bool,
    offer_started: bool,
    _onicecandidate: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
    _ontrack: Closure<dyn FnMut(RtcTrackEvent)>,
}

struct RemoteMediaRegistration {
    video: HtmlVideoElement,
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
}

#[derive(Clone)]
struct PendingIceCandidate {
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
}

thread_local! {
    static REMOTE_MEDIA_REGISTRY: RefCell<HashMap<u64, RemoteMediaRegistration>> = RefCell::new(HashMap::new());
    static PENDING_ICE_REGISTRY: RefCell<HashMap<u64, Vec<PendingIceCandidate>>> = RefCell::new(HashMap::new());
}

enum WebcamEvent {
    Ready(WebcamCapture),
    Failed(String),
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

fn create_hotbar(blocks: &[BlockId]) -> Vec<Element> {
    let Some(document) = document() else {
        return Vec::new();
    };
    let Some(body) = document.body() else {
        return Vec::new();
    };

    let root = document.create_element("div").expect("hotbar root");
    let _ = root.set_attribute(
        "style",
        "position:fixed;left:50%;bottom:24px;transform:translateX(-50%);display:flex;gap:10px;padding:10px 14px;border-radius:18px;background:rgba(18,24,32,0.64);backdrop-filter:blur(8px);box-shadow:0 12px 34px rgba(0,0,0,0.28);pointer-events:none;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;z-index:20;",
    );

    let mut slots = Vec::new();
    for (index, block) in blocks.iter().enumerate() {
        let slot = document.create_element("div").expect("hotbar slot");
        let _ = slot.set_attribute(
            "style",
            "width:78px;height:62px;border-radius:14px;padding:8px 10px;box-sizing:border-box;display:flex;flex-direction:column;justify-content:space-between;color:#e6edf3;border:1px solid rgba(255,255,255,0.14);background:rgba(255,255,255,0.08);",
        );
        slot.set_inner_html(&format!(
            "<div style=\"font-size:11px;opacity:0.72;\">{}</div><div style=\"font-size:13px;font-weight:700;\">{}</div>",
            index + 1,
            block_label(*block)
        ));
        let _ = root.append_child(&slot);
        slots.push(slot);
    }

    let _ = body.append_child(&root);
    slots
}

fn update_hotbar_ui(slots: &[Element], blocks: &[BlockId], selected: usize) {
    for (index, slot) in slots.iter().enumerate() {
        let active = index == selected;
        let block = blocks.get(index).copied().unwrap_or(BlockId::Air);
        let style = if active {
            "width:78px;height:62px;border-radius:14px;padding:8px 10px;box-sizing:border-box;display:flex;flex-direction:column;justify-content:space-between;color:#081018;border:1px solid rgba(255,255,255,0.36);background:linear-gradient(180deg,rgba(255,244,196,0.96),rgba(245,208,105,0.96));box-shadow:0 0 0 2px rgba(255,240,180,0.42);"
        } else {
            "width:78px;height:62px;border-radius:14px;padding:8px 10px;box-sizing:border-box;display:flex;flex-direction:column;justify-content:space-between;color:#e6edf3;border:1px solid rgba(255,255,255,0.14);background:rgba(255,255,255,0.08);"
        };
        let _ = slot.set_attribute("style", style);
        slot.set_inner_html(&format!(
            "<div style=\"font-size:11px;opacity:0.72;\">{}</div><div style=\"font-size:13px;font-weight:700;\">{}</div>",
            index + 1,
            block_label(block)
        ));
    }
}

fn block_label(block: BlockId) -> &'static str {
    match block {
        BlockId::Grass => "Grass",
        BlockId::Dirt => "Dirt",
        BlockId::Stone => "Stone",
        BlockId::Sand => "Sand",
        BlockId::Water => "Water",
        BlockId::Log => "Log",
        BlockId::Leaves => "Leaves",
        BlockId::Planks => "Planks",
        BlockId::Glass => "Glass",
        BlockId::Lantern => "Lantern",
        BlockId::Storage => "Storage",
        BlockId::Air => "Empty",
    }
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

fn confirm_open_url(label: &str) -> bool {
    web_sys::window()
        .and_then(|window| {
            window
                .confirm_with_message(&format!("Do you want to go to {label}?"))
                .ok()
        })
        .unwrap_or(false)
}

fn open_url(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.open_with_url_and_target(url, "_blank");
    }
}

fn attach_local_webcam_overlay(video: &HtmlVideoElement) {
    let _ = video.set_attribute(
        "style",
        "position:fixed;top:16px;right:16px;width:192px;height:144px;object-fit:cover;border:2px solid rgba(255,255,255,0.85);border-radius:12px;box-shadow:0 12px 28px rgba(0,0,0,0.35);z-index:20;pointer-events:none;background:#111;",
    );
    if let Some(document) = document() {
        if let Some(body) = document.body() {
            let _ = body.append_child(video);
        }
    }
}

fn send_client_message_over_websocket(websocket: &WebSocket, message: &ClientMessage) {
    if let Ok(bytes) = encode(message) {
        let _ = websocket.send_with_u8_array(&bytes);
    }
}

fn flush_pending_ice_candidates(player_id: u64, connection: &RtcPeerConnection) {
    let pending = PENDING_ICE_REGISTRY.with(|registry| registry.borrow_mut().remove(&player_id));
    let Some(pending) = pending else {
        return;
    };

    for candidate in pending {
        let init = RtcIceCandidateInit::new(&candidate.candidate);
        if let Some(mid) = candidate.sdp_mid.as_deref() {
            init.set_sdp_mid(Some(mid));
        }
        if let Some(index) = candidate.sdp_mline_index {
            init.set_sdp_m_line_index(Some(index));
        }
        let _ = connection.add_ice_candidate_with_opt_rtc_ice_candidate_init(Some(&init));
    }
}

fn request_webcam_capture(sender: Sender<WebcamEvent>) {
    spawn_local(async move {
        let result = async {
            let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
            let media_devices = window
                .navigator()
                .media_devices()
                .map_err(|error| anyhow::anyhow!("media devices unavailable: {error:?}"))?;

            let constraints = MediaStreamConstraints::new();
            constraints.set_video(&JsValue::TRUE);
            constraints.set_audio(&JsValue::FALSE);

            let stream_value = JsFuture::from(
                media_devices
                    .get_user_media_with_constraints(&constraints)
                    .map_err(|error| anyhow::anyhow!("getUserMedia failed: {error:?}"))?,
            )
            .await
            .map_err(|error| anyhow::anyhow!("getUserMedia rejected: {error:?}"))?;
            let stream: MediaStream = stream_value
                .dyn_into()
                .map_err(|_| anyhow::anyhow!("media stream cast failed"))?;

            let document = window.document().ok_or_else(|| anyhow::anyhow!("document unavailable"))?;
            let video: HtmlVideoElement = document
                .create_element("video")
                .map_err(|error| anyhow::anyhow!("video element create failed: {error:?}"))?
                .dyn_into()
                .map_err(|_| anyhow::anyhow!("video element cast failed"))?;
            video.set_autoplay(true);
            video.set_muted(true);
            video.set_attribute("playsinline", "true")
                .map_err(|error| anyhow::anyhow!("playsinline failed: {error:?}"))?;
            video.set_src_object(Some(&stream));
            let _ = video.play();

            Ok::<WebcamCapture, anyhow::Error>(WebcamCapture { stream, video })
        }
        .await;

        let _ = match result {
            Ok(capture) => sender.send(WebcamEvent::Ready(capture)),
            Err(error) => sender.send(WebcamEvent::Failed(error.to_string())),
        };
    });
}

#[allow(dead_code)]
fn find_safe_spawn_position(terrain: &TerrainGenerator) -> Vec3 {
    let mut chunks = HashMap::<ChunkPos, ChunkData>::new();
    let spawn_offsets = [
        (0.5_f32, 0.5_f32),
        (0.25_f32, 0.25_f32),
        (0.75_f32, 0.25_f32),
        (0.25_f32, 0.75_f32),
        (0.75_f32, 0.75_f32),
    ];

    for radius in 0_i32..=12 {
        for z in -radius..=radius {
            for x in -radius..=radius {
                if radius > 0 && x.abs().max(z.abs()) != radius {
                    continue;
                }

                for (offset_x, offset_z) in spawn_offsets {
                    let sample_x = x as f32 + offset_x;
                    let sample_z = z as f32 + offset_z;
                    let surface = terrain.surface_height(sample_x.floor() as i64, sample_z.floor() as i64);

                    for lift in 0..=12 {
                        let candidate = Vec3::new(
                            sample_x,
                            surface as f32 + 1.0 + lift as f32 + PLAYER_EYE_HEIGHT,
                            sample_z,
                        );

                        if !generated_player_collides(terrain, &mut chunks, candidate) {
                            return candidate;
                        }
                    }
                }
            }
        }
    }

    Vec3::new(0.5, terrain.surface_height(0, 0) as f32 + 3.0 + PLAYER_EYE_HEIGHT, 0.5)
}

#[allow(dead_code)]
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

#[allow(dead_code)]
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
                    voxels: Vec::new(),
                    failed: true,
                });
                return;
            }

            let vertices_value = js_sys::Reflect::get(&object, &JsValue::from_str("vertices")).unwrap();
            let indices_value = js_sys::Reflect::get(&object, &JsValue::from_str("indices")).unwrap();
            let voxels_value = js_sys::Reflect::get(&object, &JsValue::from_str("voxels")).unwrap();
            let vertex_floats = js_sys::Float32Array::new(&vertices_value).to_vec();
            let indices = js_sys::Uint32Array::new(&indices_value).to_vec();
            let voxels = js_sys::Uint16Array::new(&voxels_value).to_vec();
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
                voxels,
                failed: false,
            });
        }) as Box<dyn FnMut(MessageEvent)>);

        worker.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        workers.push(worker);
        onmessages.push(onmessage);
    }

    Ok((rx, workers, onmessages))
}

fn start_websocket_client() -> Result<(Receiver<NetworkEvent>, WebSocket, WebSocketBindings)> {
    let url = websocket_url()?;
    let websocket = WebSocket::new(&url).map_err(|error| anyhow::anyhow!("create websocket: {error:?}"))?;
    websocket.set_binary_type(BinaryType::Arraybuffer);

    let (tx, rx) = mpsc::channel::<NetworkEvent>();

    let open_tx = tx.clone();
    let onopen = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = open_tx.send(NetworkEvent::Opened);
    }) as Box<dyn FnMut(WebEvent)>);
    websocket.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let message_tx = tx.clone();
    let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
        let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
        match decode::<ServerMessage>(&bytes) {
            Ok(message) => {
                let _ = message_tx.send(NetworkEvent::Server(message));
            }
            Err(error) => {
                let _ = message_tx.send(NetworkEvent::Disconnected(format!("decode websocket message: {error}")));
            }
        }
    }) as Box<dyn FnMut(MessageEvent)>);
    websocket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    let error_tx = tx.clone();
    let onerror = Closure::wrap(Box::new(move |_event: ErrorEvent| {
        let _ = error_tx.send(NetworkEvent::Disconnected("websocket error".to_string()));
    }) as Box<dyn FnMut(ErrorEvent)>);
    websocket.set_onerror(Some(onerror.as_ref().unchecked_ref()));

    let close_tx = tx;
    let onclose = Closure::wrap(Box::new(move |event: CloseEvent| {
        let _ = close_tx.send(NetworkEvent::Disconnected(format!(
            "websocket closed ({})",
            event.code()
        )));
    }) as Box<dyn FnMut(CloseEvent)>);
    websocket.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    Ok((
        rx,
        websocket,
        WebSocketBindings {
            _onopen: onopen,
            _onmessage: onmessage,
            _onerror: onerror,
            _onclose: onclose,
        },
    ))
}

#[allow(dead_code)]
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

fn dispatch_chunk_mesh_job(worker: &Worker, chunk: &ChunkData) {
    let job = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("kind"), &JsValue::from_str("mesh_chunk"));
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("x"),
        &JsValue::from_f64(f64::from(chunk.position.x)),
    );
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("z"),
        &JsValue::from_f64(f64::from(chunk.position.z)),
    );
    let voxels = expand_chunk_voxels(chunk);
    let voxels_array = js_sys::Uint16Array::from(voxels.as_slice());
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("voxels"), &voxels_array);
    let _ = worker.post_message(&job);
}

fn expand_chunk_voxels(chunk: &ChunkData) -> Vec<u16> {
    let mut voxels = Vec::with_capacity(CHUNK_WIDTH as usize * CHUNK_HEIGHT as usize * CHUNK_DEPTH as usize);
    for y in 0..CHUNK_HEIGHT {
        for z in 0..CHUNK_DEPTH {
            for x in 0..CHUNK_WIDTH {
                let voxel = chunk.voxel(LocalVoxelPos {
                    x: x as u8,
                    y: y as u8,
                    z: z as u8,
                });
                voxels.push(voxel.block as u16);
            }
        }
    }
    voxels
}

fn websocket_url() -> Result<String> {
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window"))?;
    let location = window.location();
    let host = location.hostname().unwrap_or_else(|_| "127.0.0.1".to_string());
    Ok(format!("ws://{host}:4001"))
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

#[allow(dead_code)]
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
    voxels: Vec<u16>,
    failed: bool,
}

#[allow(dead_code)]
struct WebSocketBindings {
    _onopen: Closure<dyn FnMut(WebEvent)>,
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
    _onerror: Closure<dyn FnMut(ErrorEvent)>,
    _onclose: Closure<dyn FnMut(CloseEvent)>,
}

enum NetworkEvent {
    Opened,
    Server(ServerMessage),
    Disconnected(String),
}

#[derive(Clone, Copy, Debug)]
enum Face {
    North,
    South,
    East,
    West,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
struct RaycastHit {
    block: WorldPos,
    previous_empty: Option<WorldPos>,
    face: Option<Face>,
    distance: f32,
}

#[derive(Clone, Copy, Debug)]
struct LinkPanel {
    center: Vec3,
}

impl LinkPanel {
    fn near_spawn(spawn: Vec3) -> Self {
        Self {
            center: Vec3::new(spawn.x + 4.0, spawn.y + 0.2, spawn.z),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct LinkHit {
    distance: f32,
}

#[derive(Clone, Copy, Debug)]
enum InteractionTarget {
    Block(RaycastHit),
    Link,
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

fn block_from_id(id: u16) -> BlockId {
    match id {
        1 => BlockId::Grass,
        2 => BlockId::Dirt,
        3 => BlockId::Stone,
        4 => BlockId::Sand,
        5 => BlockId::Water,
        6 => BlockId::Log,
        7 => BlockId::Leaves,
        8 => BlockId::Planks,
        9 => BlockId::Glass,
        10 => BlockId::Lantern,
        11 => BlockId::Storage,
        _ => BlockId::Air,
    }
}

fn face_from_empty_neighbor(block: WorldPos, empty: WorldPos) -> Option<Face> {
    let dx = empty.x - block.x;
    let dy = empty.y - block.y;
    let dz = empty.z - block.z;

    match (dx, dy, dz) {
        (0, 0, -1) => Some(Face::North),
        (0, 0, 1) => Some(Face::South),
        (1, 0, 0) => Some(Face::East),
        (-1, 0, 0) => Some(Face::West),
        (0, 1, 0) => Some(Face::Up),
        (0, -1, 0) => Some(Face::Down),
        _ => None,
    }
}

fn raycast_link_panel(origin: Vec3, direction: Vec3, panel: LinkPanel) -> Option<LinkHit> {
    let direction = direction.normalize_or_zero();
    if direction == Vec3::ZERO || direction.x.abs() < 0.0001 {
        return None;
    }

    let t = (panel.center.x - origin.x) / direction.x;
    if !(0.0..=6.0).contains(&t) {
        return None;
    }

    let hit = origin + direction * t;
    let local = hit - panel.center;
    if local.y.abs() > LINK_PANEL_HALF_HEIGHT || local.z.abs() > LINK_PANEL_HALF_WIDTH {
        return None;
    }
    if local.x.abs() > LINK_PANEL_HALF_DEPTH + 0.02 {
        return None;
    }

    Some(LinkHit { distance: t })
}

fn add_face_highlight(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    block: WorldPos,
    face: Face,
    thickness: f32,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let min = Vec3::new(block.x as f32, block.y as f32, block.z as f32);
    let max = min + Vec3::ONE;
    let inset = 0.04;
    let half = thickness * 0.5;

    match face {
        Face::North => add_box_oriented(
            vertices,
            indices,
            Vec3::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5, min.z - half),
            Vec3::new(0.5 - inset, 0.0, 0.0),
            Vec3::new(0.0, 0.5 - inset, 0.0),
            Vec3::new(0.0, 0.0, half),
            color,
            tile,
        ),
        Face::South => add_box_oriented(
            vertices,
            indices,
            Vec3::new((min.x + max.x) * 0.5, (min.y + max.y) * 0.5, max.z + half),
            Vec3::new(0.5 - inset, 0.0, 0.0),
            Vec3::new(0.0, 0.5 - inset, 0.0),
            Vec3::new(0.0, 0.0, half),
            color,
            tile,
        ),
        Face::East => add_box_oriented(
            vertices,
            indices,
            Vec3::new(max.x + half, (min.y + max.y) * 0.5, (min.z + max.z) * 0.5),
            Vec3::new(0.0, 0.0, 0.5 - inset),
            Vec3::new(0.0, 0.5 - inset, 0.0),
            Vec3::new(half, 0.0, 0.0),
            color,
            tile,
        ),
        Face::West => add_box_oriented(
            vertices,
            indices,
            Vec3::new(min.x - half, (min.y + max.y) * 0.5, (min.z + max.z) * 0.5),
            Vec3::new(0.0, 0.0, 0.5 - inset),
            Vec3::new(0.0, 0.5 - inset, 0.0),
            Vec3::new(half, 0.0, 0.0),
            color,
            tile,
        ),
        Face::Up => add_box_oriented(
            vertices,
            indices,
            Vec3::new((min.x + max.x) * 0.5, max.y + half, (min.z + max.z) * 0.5),
            Vec3::new(0.5 - inset, 0.0, 0.0),
            Vec3::new(0.0, half, 0.0),
            Vec3::new(0.0, 0.0, 0.5 - inset),
            color,
            tile,
        ),
        Face::Down => add_box_oriented(
            vertices,
            indices,
            Vec3::new((min.x + max.x) * 0.5, min.y - half, (min.z + max.z) * 0.5),
            Vec3::new(0.5 - inset, 0.0, 0.0),
            Vec3::new(0.0, half, 0.0),
            Vec3::new(0.0, 0.0, 0.5 - inset),
            color,
            tile,
        ),
    }
}

fn add_link_panel_mesh(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    panel: LinkPanel,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let center = panel.center;
    let screen_half_depth = 0.006;
    let frame_half_depth = 0.028;
    let screen_gap = 0.008;
    let axis_x = Vec3::new(0.0, 0.0, LINK_PANEL_HALF_WIDTH);
    let axis_y = Vec3::new(0.0, LINK_PANEL_HALF_HEIGHT, 0.0);
    let normal = Vec3::new(-1.0, 0.0, 0.0);
    let screen_center = center + normal * (frame_half_depth + screen_gap + screen_half_depth);
    let front_center = screen_center + normal * screen_half_depth;
    let back_center = screen_center - normal * screen_half_depth;
    let uvs = atlas_quad_raw(tile);

    let front = [
        front_center - axis_x + axis_y,
        front_center + axis_x + axis_y,
        front_center + axis_x - axis_y,
        front_center - axis_x - axis_y,
    ];
    let back = [
        back_center + axis_x + axis_y,
        back_center - axis_x + axis_y,
        back_center - axis_x - axis_y,
        back_center + axis_x - axis_y,
    ];

    add_face_indices(vertices, indices, front, color, uvs);
    add_face_indices(vertices, indices, back, color, uvs);

    add_box_oriented(
        vertices,
        indices,
        center,
        Vec3::new(frame_half_depth, 0.0, 0.0),
        Vec3::new(0.0, LINK_PANEL_HALF_HEIGHT + 0.08, 0.0),
        Vec3::new(0.0, 0.0, LINK_PANEL_HALF_WIDTH + 0.08),
        [0.22, 0.18, 0.12],
        (0, 1),
    );
}

fn add_media_panel_billboard(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    center: Vec3,
    camera_position: Vec3,
    color: [f32; 3],
    uvs: [[f32; 2]; 4],
) {
    let forward = (camera_position - center).normalize_or_zero();
    let right = Vec3::new(-forward.z, 0.0, forward.x).normalize_or_zero();
    let up = right.cross(forward).normalize_or_zero();
    add_double_sided_face(
        vertices,
        indices,
        [
            center - right * WEBCAM_PANEL_HALF_WIDTH + up * WEBCAM_PANEL_HALF_HEIGHT,
            center + right * WEBCAM_PANEL_HALF_WIDTH + up * WEBCAM_PANEL_HALF_HEIGHT,
            center + right * WEBCAM_PANEL_HALF_WIDTH - up * WEBCAM_PANEL_HALF_HEIGHT,
            center - right * WEBCAM_PANEL_HALF_WIDTH - up * WEBCAM_PANEL_HALF_HEIGHT,
        ],
        color,
        uvs,
    );
}

fn add_double_sided_face(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    positions: [Vec3; 4],
    color: [f32; 3],
    uvs: [[f32; 2]; 4],
) {
    add_face_indices(vertices, indices, positions, color, uvs);
    add_face_indices(
        vertices,
        indices,
        [positions[1], positions[0], positions[3], positions[2]],
        color,
        [uvs[1], uvs[0], uvs[3], uvs[2]],
    );
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

fn atlas_quad(tile: (u32, u32)) -> [[f32; 2]; 4] {
    atlas_quad_span(tile, 2)
}

fn atlas_quad_raw(tile: (u32, u32)) -> [[f32; 2]; 4] {
    atlas_quad_span(tile, 1)
}

fn full_uv_quad() -> [[f32; 2]; 4] {
    [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]
}

fn atlas_quad_span(tile: (u32, u32), span: u32) -> [[f32; 2]; 4] {
    const TILE_COUNT: f32 = 12.0;
    const EPS: f32 = 0.001;

    let span = span as f32;
    let min_u = (tile.0 as f32 * span) / TILE_COUNT + EPS;
    let max_u = ((tile.0 as f32 * span) + span) / TILE_COUNT - EPS;
    let min_v = (tile.1 as f32 * span) / TILE_COUNT + EPS;
    let max_v = ((tile.1 as f32 * span) + span) / TILE_COUNT - EPS;

    [[min_u, min_v], [max_u, min_v], [max_u, max_v], [min_u, max_v]]
}

fn remote_player_color(player_id: u64) -> [f32; 3] {
    let hue = (player_id as f32 * 0.173).fract();
    let r = 0.45 + 0.4 * (hue * std::f32::consts::TAU).sin().abs();
    let g = 0.45 + 0.4 * ((hue + 0.33) * std::f32::consts::TAU).sin().abs();
    let b = 0.45 + 0.4 * ((hue + 0.66) * std::f32::consts::TAU).sin().abs();
    [r, g, b]
}

fn player_anchor_from_eye(eye: Vec3) -> PlayerAnchor {
    player_anchor_from_eye_with_look(eye, Vec3::Z)
}

fn player_anchor_from_eye_with_look(eye: Vec3, look: Vec3) -> PlayerAnchor {
    let look = if look.length_squared() > 0.0001 {
        look.normalize()
    } else {
        Vec3::Z
    };
    let body = eye - Vec3::Y * PLAYER_EYE_HEIGHT;
    let head = body + Vec3::Y * PLAYER_HEIGHT;
    let media = head + Vec3::Y * 0.75 + look * 1.8;
    PlayerAnchor { body, head, media }
}
