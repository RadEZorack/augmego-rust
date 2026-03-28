#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Quat, Vec3};
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
    Event as WebEvent, FormData, HtmlCanvasElement, HtmlInputElement, HtmlVideoElement,
    MediaStream, MediaStreamConstraints, MessageEvent, Request, RequestCredentials,
    RequestInit, RequestMode, Response, RtcIceCandidateInit, RtcPeerConnection,
    RtcPeerConnectionIceEvent, RtcSdpType, RtcSessionDescriptionInit, RtcTrackEvent,
    WebSocket, Worker,
};
use wgpu_lite::{AnimatedMesh, AnimatedMeshDraw, AnimatedVertex, DynamicTexture, MAX_SKIN_JOINTS, Mesh, Renderer, TexturedMesh, Vertex};
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::WindowExtWebSys;
use winit::window::WindowBuilder;

const FEATURED_MODEL_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/models/Meshy_AI_A_cute_dog_0326155854_texture.glb"
));
const DEFAULT_WORLD_SEED: u64 = 0xA66D_E601;
const WEB_RADIUS: i32 = 6;
#[allow(dead_code)]
const INITIAL_WEB_RADIUS: i32 = 1;
const SPAWN_READY_RADIUS: i32 = 1;
const CHUNK_WORLD_RADIUS: f32 = (CHUNK_WIDTH as f32) * 0.5;
const DRAW_DISTANCE_CHUNKS: f32 = 14.0;
const MESH_WORKER_COUNT: usize = 3;
const DEFAULT_GENERATION_BUDGET_PER_UPDATE: usize = 2;
const DEFAULT_MESH_UPLOAD_BUDGET_PER_UPDATE: usize = 1;
const MAX_IDLE_MESH_UPLOAD_BUDGET_PER_UPDATE: usize = 2;
const DEFAULT_NETWORK_MESSAGE_BUDGET_PER_TICK: usize = 8;
const PENDING_REPRIORITIZE_DOT_THRESHOLD: f32 = 0.985;
const WEB_RENDER_SCALE: f32 = 0.8;
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
const FEATURED_MODEL_DESIRED_HEIGHT: f32 = 1.5;
const REMOTE_AVATAR_SCALE_FACTOR: f32 = 1.0 / 3.0;
const AUTH_STATUS_CHECKING: &str = "Checking your sign-in session...";
const AUTH_STATUS_SIGNED_OUT: &str = "Sign in with SSO, or continue as a guest.";

#[derive(Clone, Debug)]
struct AuthUser {
    id: String,
    name: Option<String>,
    email: Option<String>,
    avatar_selection: Option<PlayerAvatarSelection>,
}

#[derive(Clone, Debug)]
struct PlayerAvatarSelection {
    idle_model_url: Option<String>,
    run_model_url: Option<String>,
    dance_model_url: Option<String>,
}

impl AuthUser {
    fn guest() -> Self {
        let guest_id = format!("guest-{}", js_sys::Math::random().to_bits());
        Self {
            id: guest_id.clone(),
            name: Some(format!("Guest {}", &guest_id[6..guest_id.len().min(12)])),
            email: None,
            avatar_selection: None,
        }
    }

    fn display_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| format!("Player {}", &self.id[..self.id.len().min(8)]))
    }
}

#[derive(Debug)]
enum AuthStatus {
    Checking,
    SignedOut,
    SignedIn,
    Failed(String),
}

enum AuthEvent {
    Resolved(std::result::Result<Option<AuthUser>, String>),
}

enum RemoteAvatarEvent {
    Loaded {
        url: String,
        bytes: Vec<u8>,
    },
    Failed {
        url: String,
        message: String,
    },
}

struct RemoteAvatarAsset {
    mesh: AnimatedMesh,
    node_children: Vec<Vec<usize>>,
    root_nodes: Vec<usize>,
    rest_locals: Vec<NodeTransform>,
    joint_nodes: Vec<usize>,
    inverse_bind_matrices: Vec<Mat4>,
    animation: AvatarAnimationClip,
    model_normalization: Mat4,
}

#[derive(Clone, Copy)]
struct NodeTransform {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

impl NodeTransform {
    fn matrix(self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

#[derive(Clone)]
struct AvatarAnimationClip {
    duration_seconds: f32,
    channels: Vec<AvatarAnimationChannel>,
}

#[derive(Clone)]
struct AvatarAnimationChannel {
    node_index: usize,
    property: AnimationProperty,
    keyframe_times: Vec<f32>,
    outputs: AnimationOutputs,
}

#[derive(Clone, Copy)]
enum AnimationProperty {
    Translation,
    Rotation,
    Scale,
}

#[derive(Clone)]
enum AnimationOutputs {
    Vec3(Vec<Vec3>),
    Quat(Vec<Quat>),
}

thread_local! {
    static AUTH_GUEST_QUEUE: RefCell<bool> = const { RefCell::new(false) };
}

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

    let initial_window_size = window.inner_size();
    let renderer = Renderer::new_with_size(window, scaled_render_size(initial_window_size)).await?;
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
                    renderer.resize(scaled_render_size(size));
                    app.resize(size);
                }
                WindowEvent::KeyboardInput { event, .. } => app.handle_key(event),
                WindowEvent::MouseInput { state: ElementState::Pressed, button, .. } => {
                    app.handle_mouse_button(button);
                }
                WindowEvent::RedrawRequested => {
                    app.process_webcam_events();
                    app.process_remote_avatar_events(&renderer);
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
                    app.ensure_featured_model_loaded(&renderer);
                    app.update_remote_media_textures(&renderer);
                    let textured_meshes = app.build_remote_media_meshes(&renderer);
                    let mut overlay_meshes = Vec::new();
                    if let Some(mesh) = app.build_crosshair_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    if let Some(mesh) = app.build_target_highlight_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    let remote_avatar_meshes = app.build_remote_avatar_meshes(&renderer);
                    let mut textured_mesh_refs = textured_meshes.iter().collect::<Vec<_>>();
                    if let Some(featured_model) = app.featured_model.as_ref() {
                        textured_mesh_refs.push(featured_model);
                    }
                    let overlay_refs = overlay_meshes.iter().collect::<Vec<_>>();

                    if let Err(error) = renderer.render(
                        &visible_mesh_refs,
                        &textured_mesh_refs,
                        &remote_avatar_meshes,
                        &overlay_refs,
                    ) {
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
    completed_meshes: VecDeque<MeshBuildResult>,
    last_reprioritize_chunk: ChunkPos,
    last_reprioritize_forward: Vec3,
    movement_active: bool,
    mouse_captured: bool,
    spawn_settled: bool,
    chunk_edits: HashMap<ChunkPos, HashMap<(u8, u8, u8), BlockId>>,
    link_panel: LinkPanel,
    hotbar_slots: Vec<Element>,
    mouse_lock_prompt: Element,
    webcam_prompt: Element,
    hotbar_blocks: Vec<BlockId>,
    selected_hotbar: usize,
    player_id: Option<u64>,
    remote_players: HashMap<u64, [f32; 3]>,
    remote_player_yaws: HashMap<u64, f32>,
    remote_player_stationary_model_urls: HashMap<u64, Option<String>>,
    remote_player_animation_times: HashMap<u64, f32>,
    remote_media: HashMap<u64, RemotePeerMedia>,
    remote_avatar_assets: HashMap<String, RemoteAvatarAsset>,
    pending_remote_avatar_urls: HashSet<String>,
    featured_model: Option<TexturedMesh>,
    featured_model_attempted: bool,
    spawn_position: Option<WorldPos>,
    world_seed: u64,
    webcam_requested: bool,
    webcam_tx: Sender<WebcamEvent>,
    webcam_rx: Receiver<WebcamEvent>,
    webcam: Option<WebcamCapture>,
    last_sent_position: Option<[f32; 3]>,
    last_sent_velocity: Option<[f32; 3]>,
    last_sent_yaw: Option<f32>,
    tick_counter: u64,
    transport_open: bool,
    logged_in: bool,
    network_rx: Receiver<NetworkEvent>,
    pending_network_events: VecDeque<NetworkEvent>,
    websocket: WebSocket,
    _websocket_bindings: WebSocketBindings,
    _mouse_lock_prompt_onclick: Closure<dyn FnMut(WebEvent)>,
    _webcam_prompt_onclick: Closure<dyn FnMut(WebEvent)>,
    auth_status: AuthStatus,
    auth_user: Option<AuthUser>,
    auth_rx: Receiver<AuthEvent>,
    remote_avatar_tx: Sender<RemoteAvatarEvent>,
    remote_avatar_rx: Receiver<RemoteAvatarEvent>,
    auth_overlay: Element,
    auth_overlay_status: Element,
    player_avatar_panel: Element,
    player_avatar_panel_status: Element,
    server_ready_for_login: bool,
    login_request_sent: bool,
    _auth_button_onclicks: Vec<Closure<dyn FnMut(WebEvent)>>,
    _player_avatar_panel_onclick: Closure<dyn FnMut(WebEvent)>,
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
            BlockId::GoldOre,
            BlockId::Planks,
            BlockId::Glass,
            BlockId::Lantern,
        ];
        let hotbar_slots = create_hotbar(&hotbar_blocks);
        let (mouse_lock_prompt, mouse_lock_prompt_onclick) = create_mouse_lock_prompt(&canvas);
        let (webcam_prompt, webcam_prompt_onclick) = create_webcam_prompt();
        let auth_rx = request_auth_session();
        let (remote_avatar_tx, remote_avatar_rx) = mpsc::channel();
        let (auth_overlay, auth_overlay_status, auth_button_onclicks) = create_auth_overlay();
        let (player_avatar_panel, player_avatar_panel_status, player_avatar_panel_onclick) =
            create_player_avatar_panel();
        update_hotbar_ui(&hotbar_slots, &hotbar_blocks, 0);
        let current_chunk = chunk_from_world_position(camera.position);
        let desired_chunks = HashSet::new();
        let pending_generation = VecDeque::new();
        let last_reprioritize_forward = camera.forward();

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
            completed_meshes: VecDeque::new(),
            last_reprioritize_chunk: current_chunk,
            last_reprioritize_forward,
            movement_active: false,
            mouse_captured: false,
            spawn_settled: false,
            chunk_edits: HashMap::new(),
            link_panel,
            hotbar_slots,
            mouse_lock_prompt,
            webcam_prompt,
            hotbar_blocks,
            selected_hotbar: 0,
            player_id: None,
            remote_players: HashMap::new(),
            remote_player_yaws: HashMap::new(),
            remote_player_stationary_model_urls: HashMap::new(),
            remote_player_animation_times: HashMap::new(),
            remote_media: HashMap::new(),
            remote_avatar_assets: HashMap::new(),
            pending_remote_avatar_urls: HashSet::new(),
            featured_model: None,
            featured_model_attempted: false,
            spawn_position: None,
            world_seed: DEFAULT_WORLD_SEED,
            webcam_requested: false,
            webcam_tx,
            webcam_rx,
            webcam: None,
            last_sent_position: None,
            last_sent_velocity: None,
            last_sent_yaw: None,
            tick_counter: 0,
            transport_open: false,
            logged_in: false,
            network_rx,
            pending_network_events: VecDeque::new(),
            websocket,
            _websocket_bindings: websocket_bindings,
            _mouse_lock_prompt_onclick: mouse_lock_prompt_onclick,
            _webcam_prompt_onclick: webcam_prompt_onclick,
            auth_status: AuthStatus::Checking,
            auth_user: None,
            auth_rx,
            remote_avatar_tx,
            remote_avatar_rx,
            auth_overlay,
            auth_overlay_status,
            player_avatar_panel,
            player_avatar_panel_status,
            server_ready_for_login: false,
            login_request_sent: false,
            _auth_button_onclicks: auth_button_onclicks,
            _player_avatar_panel_onclick: player_avatar_panel_onclick,
            mesh_result_rx,
            workers,
            next_worker_index: 0,
            _worker_onmessages: worker_onmessages,
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
    }

    fn sync_pointer_lock_state(&mut self) {
        self.mouse_captured = pointer_is_locked(&self.canvas);
        if self.mouse_captured {
            let _ = self.mouse_lock_prompt.set_attribute("style", "display:none;");
        } else {
            let _ = self.mouse_lock_prompt.set_attribute(
                "style",
                "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);padding:18px 28px;border-radius:18px;border:1px solid rgba(255,255,255,0.28);background:rgba(18,24,32,0.88);color:#f6f8fb;font:600 18px/1.2 ui-sans-serif,system-ui,sans-serif;box-shadow:0 20px 60px rgba(0,0,0,0.35);cursor:pointer;z-index:40;backdrop-filter:blur(10px);",
            );
        }
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
        let should_request_webcam = WEBCAM_PROMPT_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_request = *queued;
            *queued = false;
            should_request
        });
        if should_request_webcam {
            self.ensure_webcam_requested();
        }

        while let Ok(event) = self.webcam_rx.try_recv() {
            match event {
                WebcamEvent::Ready(capture) => {
                    attach_local_webcam_overlay(&capture.video);
                    let _ = self.webcam_prompt.set_attribute("style", "display:none;");
                    self.webcam = Some(capture);
                    let remote_ids = self.remote_players.keys().copied().collect::<Vec<_>>();
                    for remote_id in remote_ids {
                        self.ensure_peer_connection(remote_id);
                        self.maybe_enable_peer_media(remote_id);
                    }
                }
                WebcamEvent::Failed(_message) => {
                    self.webcam_requested = false;
                }
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

    fn process_remote_avatar_events(&mut self, renderer: &Renderer<'_>) {
        while let Ok(event) = self.remote_avatar_rx.try_recv() {
            match event {
                RemoteAvatarEvent::Loaded { url, bytes } => {
                    self.pending_remote_avatar_urls.remove(&url);
                    match build_remote_avatar_asset(renderer, &bytes) {
                        Ok(asset) => {
                            self.remote_avatar_assets.insert(url, asset);
                        }
                        Err(error) => {
                            web_sys::console::warn_1(&JsValue::from_str(&format!(
                                "failed to build remote avatar asset: {error}"
                            )));
                        }
                    }
                }
                RemoteAvatarEvent::Failed { url, message } => {
                    self.pending_remote_avatar_urls.remove(&url);
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "failed to fetch remote avatar {url}: {message}"
                    )));
                }
            }
        }
    }

    fn drain_network(&mut self) {
        while let Ok(event) = self.network_rx.try_recv() {
            self.pending_network_events.push_back(event);
        }

        let mut processed_server_messages = 0usize;
        while let Some(event) = self.pending_network_events.pop_front() {
            match event {
                NetworkEvent::Opened => {
                    self.transport_open = true;
                    self.server_ready_for_login = false;
                    self.login_request_sent = false;
                    self.send_client_message(&ClientMessage::ClientHello(ClientHello {
                        protocol_version: PROTOCOL_VERSION,
                        client_name: "game-web".to_string(),
                    }));
                }
                NetworkEvent::ServerBytes(bytes) => {
                    if processed_server_messages >= DEFAULT_NETWORK_MESSAGE_BUDGET_PER_TICK {
                        self.pending_network_events
                            .push_front(NetworkEvent::ServerBytes(bytes));
                        break;
                    }
                    processed_server_messages += 1;

                    let message = match decode::<ServerMessage>(&bytes) {
                        Ok(message) => message,
                        Err(error) => {
                            self.transport_open = false;
                            self.logged_in = false;
                            self.server_ready_for_login = false;
                            self.login_request_sent = false;
                            self.player_id = None;
                            self.remote_players.clear();
                            self.remote_player_yaws.clear();
                            self.remote_player_stationary_model_urls.clear();
                            self.remote_player_animation_times.clear();
                            self.remote_media.clear();
                            self.remote_avatar_assets.clear();
                            self.pending_remote_avatar_urls.clear();
                            self.featured_model = None;
                            self.featured_model_attempted = false;
                            web_sys::console::error_1(&JsValue::from_str(&format!(
                                "multiplayer disconnected: decode websocket message: {error}"
                            )));
                            break;
                        }
                    };

                    match message {
                    ServerMessage::ServerHello(hello) => {
                        self.world_seed = hello.world_seed;
                        self.server_ready_for_login = true;
                        self.maybe_send_login_request();
                    }
                    ServerMessage::LoginResponse(response) => {
                        if response.accepted {
                            self.logged_in = true;
                            self.login_request_sent = false;
                            self.player_id = Some(response.player_id);
                            self.pending_network_events.clear();
                            self.remote_players.clear();
                            self.remote_player_yaws.clear();
                            self.remote_player_stationary_model_urls.clear();
                            self.remote_player_animation_times.clear();
                            self.remote_media.clear();
                            self.remote_avatar_assets.clear();
                            self.pending_remote_avatar_urls.clear();
                            self.last_sent_position = None;
                            self.last_sent_velocity = None;
                            self.last_sent_yaw = None;
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
                            self.completed_meshes.clear();
                            self.last_reprioritize_chunk = self.current_chunk;
                            self.last_reprioritize_forward = self.camera.forward();
                            self.pending_generation.clear();
                            self.inflight_generation.clear();
                            self.dirty_generation.clear();
                            let desired_positions =
                                ordered_desired_chunk_positions(self.current_chunk, WEB_RADIUS);
                            for position in desired_positions {
                                self.schedule_chunk_rebuild_deferred(position);
                            }
                            self.send_chunk_subscription(self.current_chunk);
                            self.link_panel = LinkPanel::near_spawn(self.camera.position);
                            self.spawn_position = Some(response.spawn_position);
                            self.featured_model = None;
                            self.featured_model_attempted = false;
                        }
                    }
                    ServerMessage::ChunkData(chunk) => {
                        let position = chunk.position;
                        self.chunk_edits.remove(&position);
                        let changed = self
                            .authoritative_chunks
                            .get(&position)
                            .map(|existing| existing != &chunk)
                            .unwrap_or(true);
                        self.authoritative_chunks.insert(position, chunk);
                        if changed && self.desired_chunks.contains(&position) {
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
                            self.completed_meshes.retain(|mesh| mesh.position != position);
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
                            self.remote_player_yaws.insert(snapshot.player_id, snapshot.yaw);
                            self.remote_player_stationary_model_urls.insert(
                                snapshot.player_id,
                                snapshot.stationary_model_url.clone(),
                            );
                            self.remote_player_animation_times
                                .entry(snapshot.player_id)
                                .or_insert(0.0);
                            self.ensure_remote_avatar_requested(
                                snapshot.stationary_model_url.as_deref(),
                            );
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
                    }
                }
                NetworkEvent::Disconnected(reason) => {
                    self.transport_open = false;
                    self.logged_in = false;
                    self.server_ready_for_login = false;
                    self.login_request_sent = false;
                    self.player_id = None;
                    self.pending_network_events.clear();
                    self.remote_players.clear();
                    self.remote_player_yaws.clear();
                    self.remote_player_stationary_model_urls.clear();
                    self.remote_player_animation_times.clear();
                    self.remote_media.clear();
                    self.remote_avatar_assets.clear();
                    self.pending_remote_avatar_urls.clear();
                    self.featured_model = None;
                    self.featured_model_attempted = false;
                    self.last_sent_yaw = None;
                    web_sys::console::error_1(&JsValue::from_str(&format!("multiplayer disconnected: {reason}")));
                }
            }
        }
    }

    fn process_auth_events(&mut self) {
        while let Ok(event) = self.auth_rx.try_recv() {
            match event {
                AuthEvent::Resolved(result) => match result {
                    Ok(Some(user)) => {
                        self.auth_user = Some(user);
                        self.auth_status = AuthStatus::SignedIn;
                    }
                    Ok(None) => {
                        self.auth_user = None;
                        self.auth_status = AuthStatus::SignedOut;
                    }
                    Err(message) => {
                        self.auth_user = None;
                        self.auth_status = AuthStatus::Failed(message);
                    }
                },
            }
        }

        let continue_as_guest = AUTH_GUEST_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_continue = *queued;
            *queued = false;
            should_continue
        });
        if continue_as_guest {
            self.auth_user = Some(AuthUser::guest());
            self.auth_status = AuthStatus::SignedIn;
        }

        self.sync_auth_overlay();
        self.sync_player_avatar_panel();
        self.maybe_send_login_request();
    }

    fn sync_auth_overlay(&self) {
        match &self.auth_status {
            AuthStatus::Checking => {
                let _ = self.auth_overlay.set_attribute("style", auth_overlay_style());
                self.auth_overlay_status
                    .set_text_content(Some(AUTH_STATUS_CHECKING));
            }
            AuthStatus::SignedOut => {
                let _ = self.auth_overlay.set_attribute("style", auth_overlay_style());
                self.auth_overlay_status
                    .set_text_content(Some(AUTH_STATUS_SIGNED_OUT));
            }
            AuthStatus::SignedIn => {
                let _ = self.auth_overlay.set_attribute("style", "display:none;");
            }
            AuthStatus::Failed(message) => {
                let _ = self.auth_overlay.set_attribute("style", auth_overlay_style());
                self.auth_overlay_status.set_text_content(Some(message));
            }
        }
    }

    fn sync_player_avatar_panel(&self) {
        match &self.auth_status {
            AuthStatus::SignedIn => {
                if self.auth_user.as_ref().is_some_and(auth_user_is_guest) {
                    let _ = self
                        .player_avatar_panel
                        .set_attribute("style", player_avatar_panel_style());
                    self.player_avatar_panel_status.set_text_content(Some(
                        "Sign in with SSO to save avatar animation uploads.",
                    ));
                } else {
                    let _ = self
                        .player_avatar_panel
                        .set_attribute("style", player_avatar_panel_style());
                    if let Some(user) = self.auth_user.as_ref() {
                        let selection = user.avatar_selection.as_ref();
                        let uploaded_count = [
                            selection.and_then(|value| value.idle_model_url.as_ref()),
                            selection.and_then(|value| value.run_model_url.as_ref()),
                            selection.and_then(|value| value.dance_model_url.as_ref()),
                        ]
                        .into_iter()
                        .flatten()
                        .count();
                        let message = if uploaded_count == 0 {
                            "Choose three GLBs for idle, run, and dance."
                        } else {
                            "Avatar animations ready. Upload again to replace any slot."
                        };
                        self.player_avatar_panel_status.set_text_content(Some(message));
                    }
                }
            }
            _ => {
                let _ = self
                    .player_avatar_panel
                    .set_attribute("style", "display:none;");
            }
        }
    }

    fn maybe_send_login_request(&mut self) {
        if !self.transport_open || !self.server_ready_for_login || self.logged_in || self.login_request_sent {
            return;
        }

        let Some(user) = self.auth_user.as_ref() else {
            return;
        };

        self.send_client_message(&ClientMessage::LoginRequest(LoginRequest {
            name: user.display_name(),
            stationary_model_url: user
                .avatar_selection
                .as_ref()
                .and_then(|selection| selection.idle_model_url.clone()),
        }));
        self.login_request_sent = true;
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
        self.sync_pointer_lock_state();
        self.process_auth_events();
        self.drain_network();
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick);
        self.last_tick = now;
        let dt_secs = dt.as_secs_f32();
        for (player_id, playback_time) in &mut self.remote_player_animation_times {
            let maybe_duration = self
                .remote_player_stationary_model_urls
                .get(player_id)
                .and_then(|value| value.as_ref())
                .and_then(|url| self.remote_avatar_assets.get(url))
                .map(|asset| asset.animation.duration_seconds);
            if let Some(duration) = maybe_duration.filter(|duration| *duration > 0.0) {
                *playback_time = (*playback_time + dt_secs) % duration;
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
        let yaw_changed = self
            .last_sent_yaw
            .map(|last| (self.camera.yaw - last).abs() > 0.0025)
            .unwrap_or(true);

        if (should_broadcast_motion && (position_changed || velocity_changed)) || yaw_changed {
            self.tick_counter = self.tick_counter.wrapping_add(1);
            self.send_client_message(&ClientMessage::PlayerInputTick(PlayerInputTick {
                tick: self.tick_counter,
                movement: [world_movement.x, 0.0, world_movement.z],
                position: Some(position),
                velocity: Some(velocity),
                yaw: Some(self.camera.yaw),
                jump,
            }));
            self.last_sent_position = Some(position);
            self.last_sent_velocity = Some(velocity);
            self.last_sent_yaw = Some(self.camera.yaw);
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
            if self
                .remote_player_stationary_model_urls
                .get(&player_id)
                .and_then(|url| url.as_ref())
                .is_some_and(|url| self.remote_avatar_assets.contains_key(url))
            {
                continue;
            }
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

    fn build_remote_avatar_meshes(&self, renderer: &Renderer<'_>) -> Vec<AnimatedMeshDraw<'_>> {
        let mut meshes = Vec::new();
        for (&player_id, position) in &self.remote_players {
            let Some(url) = self
                .remote_player_stationary_model_urls
                .get(&player_id)
                .and_then(|value| value.as_ref())
            else {
                continue;
            };
            let Some(asset) = self.remote_avatar_assets.get(url) else {
                continue;
            };
            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            let yaw = *self.remote_player_yaws.get(&player_id).unwrap_or(&0.0);
            let playback_time = *self.remote_player_animation_times.get(&player_id).unwrap_or(&0.0);
            let model =
                Mat4::from_translation(anchor.body)
                    * Mat4::from_rotation_y(-yaw)
                    * asset.model_normalization;
            let joints = evaluate_avatar_skin_matrices(asset, playback_time);
            meshes.push(renderer.create_animated_draw(&asset.mesh, model, &joints));
        }
        meshes
    }

    fn ensure_remote_avatar_requested(&mut self, maybe_url: Option<&str>) {
        let Some(url) = maybe_url.map(str::trim).filter(|url| !url.is_empty()) else {
            return;
        };
        if self.remote_avatar_assets.contains_key(url)
            || self.pending_remote_avatar_urls.contains(url)
        {
            return;
        }

        self.pending_remote_avatar_urls.insert(url.to_string());
        request_remote_avatar_model(url.to_string(), self.remote_avatar_tx.clone());
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

    fn ensure_featured_model_loaded(&mut self, renderer: &Renderer<'_>) {
        if self.featured_model.is_some() || self.featured_model_attempted {
            return;
        }

        let Some(spawn_position) = self.spawn_position else {
            return;
        };

        self.featured_model_attempted = true;
        match load_featured_model_mesh(renderer, spawn_position) {
            Ok(mesh) => {
                self.featured_model = Some(mesh);
            }
            Err(error) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "failed to load featured web model: {error:?}"
                )));
            }
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
            self.completed_meshes.push_back(result);
        }

        let completed_budget = self.mesh_upload_budget();
        for _ in 0..completed_budget {
            let Some(result) = self.completed_meshes.pop_front() else {
                break;
            };

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
                chunk_meshes.insert(
                    result.position,
                    renderer.create_mesh_from_f32(&result.vertex_floats, &result.indices),
                );
            }

            if self.dirty_generation.remove(&result.position) {
                self.pending_generation.push_front(result.position);
            }
        }

        self.reprioritize_pending_generation();

        let budget = self.generation_budget(default_budget);
        for _ in 0..budget {
            let Some(position) = self.pending_generation.pop_front() else {
                break;
            };

            if self.inflight_generation.contains(&position) {
                continue;
            }

            let worker_index = self.next_worker_index % self.workers.len();
            if let Some(chunk) = self.authoritative_chunks.get(&position).cloned() {
                dispatch_chunk_mesh_job(&self.workers[worker_index], &chunk);
            } else {
                dispatch_mesh_job(
                    &self.workers[worker_index],
                    position,
                    self.chunk_edits.get(&position),
                    self.world_seed,
                );
            }
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

    fn mesh_upload_budget(&self) -> usize {
        if self.completed_meshes.is_empty() {
            return 0;
        }

        if self.movement_active {
            DEFAULT_MESH_UPLOAD_BUDGET_PER_UPDATE
        } else {
            MAX_IDLE_MESH_UPLOAD_BUDGET_PER_UPDATE
        }
    }

    fn update_streaming_window(&mut self, chunk_meshes: &mut HashMap<ChunkPos, Mesh>) {
        let next_chunk = chunk_from_world_position(self.camera.position);
        if next_chunk == self.current_chunk {
            return;
        }

        let previous_desired = self.desired_chunks.clone();
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
        self.completed_meshes
            .retain(|mesh| self.desired_chunks.contains(&mesh.position));
        for position in ordered_desired_chunk_positions(self.current_chunk, WEB_RADIUS) {
            if !previous_desired.contains(&position) {
                self.schedule_chunk_rebuild_deferred(position);
            }
        }
    }

    fn reprioritize_pending_generation(&mut self) {
        if self.pending_generation.len() <= 1 {
            return;
        }

        let forward = self.camera.forward();
        if !self.should_reprioritize_pending_generation(forward) {
            return;
        }

        let mut pending = self.pending_generation.drain(..).collect::<Vec<_>>();
        pending.sort_by(|a, b| {
            chunk_priority(*a, self.current_chunk, self.camera.position, forward)
                .total_cmp(&chunk_priority(*b, self.current_chunk, self.camera.position, forward))
        });
        self.pending_generation = pending.into();
        self.last_reprioritize_chunk = self.current_chunk;
        self.last_reprioritize_forward = forward;
    }

    fn should_reprioritize_pending_generation(&self, forward: Vec3) -> bool {
        if self.current_chunk != self.last_reprioritize_chunk {
            return true;
        }

        if self.last_reprioritize_forward == Vec3::ZERO || forward == Vec3::ZERO {
            return true;
        }

        self.last_reprioritize_forward.dot(forward) < PENDING_REPRIORITIZE_DOT_THRESHOLD
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
        } else {
            let edits = self.chunk_edits.entry(chunk_pos).or_default();
            edits.insert((local.x, local.y, local.z), block);
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

    fn schedule_chunk_rebuild_deferred(&mut self, position: ChunkPos) {
        if self.inflight_generation.contains(&position) {
            self.dirty_generation.insert(position);
            return;
        }

        if !self.pending_generation.contains(&position) {
            self.pending_generation.push_back(position);
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

#[derive(Clone)]
struct ImportedVertex {
    position: Vec3,
    normal: Vec3,
    uv: [f32; 2],
}

#[derive(Clone)]
struct AnimatedImportedVertex {
    position: Vec3,
    normal: Vec3,
    uv: [f32; 2],
    joints: [u16; 4],
    weights: [f32; 4],
}

fn load_featured_model_mesh(renderer: &Renderer<'_>, spawn_position: WorldPos) -> Result<TexturedMesh> {
    let (mut vertices, indices, image) = load_glb_model(FEATURED_MODEL_BYTES)?;
    let (min, max) = model_bounds(&vertices).ok_or_else(|| anyhow::anyhow!("featured model has no vertices"))?;
    let scale = FEATURED_MODEL_DESIRED_HEIGHT / (max.y - min.y).max(0.001);
    let center_x = (min.x + max.x) * 0.5;
    let center_z = (min.z + max.z) * 0.5;
    let anchor = Vec3::new(
        spawn_position.x as f32 + 7.0,
        spawn_position.y as f32 + 1.0,
        spawn_position.z as f32 + 2.0,
    );

    let vertices = vertices
        .drain(..)
        .map(|vertex| {
            let normalized = Vec3::new(
                vertex.position.x - center_x,
                vertex.position.y - min.y,
                vertex.position.z - center_z,
            );
            Vertex {
                position: (normalized * scale + anchor).to_array(),
                color: [1.0, 1.0, 1.0],
                normal: vertex.normal.normalize_or_zero().to_array(),
                uv: vertex.uv,
                material_id: 0.0,
            }
        })
        .collect::<Vec<_>>();

    let rgba = match image.as_ref() {
        Some(image) => image_to_rgba_pixels(image)?,
        None => vec![255, 255, 255, 255],
    };
    let (width, height) = image
        .map(|image| (image.width.max(1), image.height.max(1)))
        .unwrap_or((1, 1));
    let texture = renderer.create_dynamic_texture(width, height);
    renderer.update_dynamic_texture_rgba(&texture, &rgba);
    Ok(renderer.create_textured_mesh(&vertices, &indices, &texture))
}

fn load_glb_model(bytes: &[u8]) -> Result<(Vec<ImportedVertex>, Vec<u32>, Option<gltf::image::Data>)> {
    let (document, buffers, images) = gltf::import_slice(bytes)?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or_else(|| anyhow::anyhow!("glb has no scene"))?;
    let mut vertices = Vec::new();
    let mut indices = Vec::new();
    let mut image_index = None;

    for node in scene.nodes() {
        append_gltf_node_meshes(
            &node,
            Mat4::IDENTITY,
            &buffers,
            &mut vertices,
            &mut indices,
            &mut image_index,
        );
    }

    if vertices.is_empty() {
        anyhow::bail!("glb did not contain any triangle vertices");
    }

    let image = image_index.and_then(|index| images.get(index).cloned());
    Ok((vertices, indices, image))
}

fn append_gltf_node_meshes(
    node: &gltf::Node<'_>,
    parent_transform: Mat4,
    buffers: &[gltf::buffer::Data],
    vertices: &mut Vec<ImportedVertex>,
    indices: &mut Vec<u32>,
    image_index: &mut Option<usize>,
) {
    let local = Mat4::from_cols_array_2d(&node.transform().matrix());
    let transform = parent_transform * local;

    if let Some(mesh) = node.mesh() {
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }

            if image_index.is_none() {
                *image_index = primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_texture()
                    .map(|texture| texture.texture().source().index());
            }

            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let primitive_positions = positions.collect::<Vec<_>>();
            let normals = reader.read_normals().map(|values| values.collect::<Vec<_>>());
            let texcoords = reader
                .read_tex_coords(0)
                .map(|coords| coords.into_f32().collect::<Vec<_>>());
            let base_vertex = vertices.len() as u32;

            for (index, position) in primitive_positions.iter().enumerate() {
                let world_position = transform.transform_point3(Vec3::from_array(*position));
                let normal = normals
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map(|value| transform.transform_vector3(Vec3::from_array(*value)).normalize_or_zero())
                    .unwrap_or(Vec3::Y);
                let uv = texcoords
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .copied()
                    .unwrap_or([0.5, 0.5]);
                vertices.push(ImportedVertex {
                    position: world_position,
                    normal,
                    uv,
                });
            }

            if let Some(read_indices) = reader.read_indices() {
                indices.extend(read_indices.into_u32().map(|index| base_vertex + index));
            } else {
                indices.extend((0..primitive_positions.len() as u32).map(|index| base_vertex + index));
            }
        }
    }

    for child in node.children() {
        append_gltf_node_meshes(&child, transform, buffers, vertices, indices, image_index);
    }
}

fn model_bounds(vertices: &[ImportedVertex]) -> Option<(Vec3, Vec3)> {
    let first = vertices.first()?;
    let mut min = first.position;
    let mut max = first.position;

    for vertex in &vertices[1..] {
        min = min.min(vertex.position);
        max = max.max(vertex.position);
    }

    Some((min, max))
}

fn image_to_rgba_pixels(image: &gltf::image::Data) -> Result<Vec<u8>> {
    use gltf::image::Format;

    let pixels = match image.format {
        Format::R8G8B8A8 => image.pixels.clone(),
        Format::R8G8B8 => image
            .pixels
            .chunks_exact(3)
            .flat_map(|rgb| [rgb[0], rgb[1], rgb[2], 255])
            .collect(),
        Format::R8 => image
            .pixels
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        other => anyhow::bail!("unsupported glb image format: {other:?}"),
    };

    Ok(pixels)
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
    static WEBCAM_PROMPT_QUEUE: RefCell<bool> = const { RefCell::new(false) };
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

fn create_mouse_lock_prompt(canvas: &HtmlCanvasElement) -> (Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let noop = Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        let fallback = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.create_element("div").ok())
            .expect("prompt fallback element");
        return (fallback, noop);
    };
    let body = document.body().expect("body");
    let prompt = document.create_element("button").expect("mouse lock prompt");
    prompt.set_text_content(Some("Click To Lock Mouse"));
    let _ = prompt.set_attribute(
        "style",
        "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);padding:18px 28px;border-radius:18px;border:1px solid rgba(255,255,255,0.28);background:rgba(18,24,32,0.88);color:#f6f8fb;font:600 18px/1.2 ui-sans-serif,system-ui,sans-serif;box-shadow:0 20px 60px rgba(0,0,0,0.35);cursor:pointer;z-index:40;backdrop-filter:blur(10px);",
    );
    let canvas = canvas.clone();
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        canvas.request_pointer_lock();
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = prompt.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    let _ = body.append_child(&prompt);
    (prompt, onclick)
}

fn create_webcam_prompt() -> (Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let noop = Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        let fallback = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.create_element("div").ok())
            .expect("webcam prompt fallback element");
        return (fallback, noop);
    };
    let body = document.body().expect("body");
    let prompt = document.create_element("button").expect("webcam prompt");
    prompt.set_text_content(Some("Activate Webcam"));
    let _ = prompt.set_attribute(
        "style",
        "position:fixed;top:16px;right:16px;width:192px;height:144px;border-radius:12px;border:1px solid rgba(255,255,255,0.28);background:rgba(18,24,32,0.88);color:#f6f8fb;font:600 18px/1.2 ui-sans-serif,system-ui,sans-serif;box-shadow:0 12px 28px rgba(0,0,0,0.35);cursor:pointer;z-index:20;backdrop-filter:blur(10px);",
    );
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        WEBCAM_PROMPT_QUEUE.with(|queue| {
            *queue.borrow_mut() = true;
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = prompt.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    let _ = body.append_child(&prompt);
    (prompt, onclick)
}

fn create_auth_overlay() -> (Element, Element, Vec<Closure<dyn FnMut(WebEvent)>>) {
    let Some(document) = document() else {
        return (fallback_element(), fallback_element(), Vec::new());
    };
    let Some(body) = document.body() else {
        return (fallback_element(), fallback_element(), Vec::new());
    };

    let root = document.create_element("div").expect("auth overlay");
    let _ = root.set_attribute("style", auth_overlay_style());

    let card = document.create_element("div").expect("auth card");
    let _ = card.set_attribute(
        "style",
        "width:min(92vw,460px);padding:28px;border-radius:24px;background:linear-gradient(180deg,rgba(18,24,32,0.92),rgba(8,12,18,0.96));border:1px solid rgba(255,255,255,0.12);box-shadow:0 30px 90px rgba(0,0,0,0.45);color:#f6f8fb;font-family:ui-sans-serif,system-ui,sans-serif;",
    );

    let eyebrow = document.create_element("div").expect("auth eyebrow");
    let _ = eyebrow.set_attribute(
        "style",
        "font-size:11px;letter-spacing:0.22em;text-transform:uppercase;color:rgba(183,230,255,0.72);margin-bottom:10px;",
    );
    eyebrow.set_text_content(Some("Augmego Login"));
    let _ = card.append_child(&eyebrow);

    let title = document.create_element("h1").expect("auth title");
    let _ = title.set_attribute(
        "style",
        "margin:0 0 10px 0;font:700 34px/1.05 Georgia,'Times New Roman',serif;",
    );
    title.set_text_content(Some("Enter the shared world"));
    let _ = card.append_child(&title);

    let body_copy = document.create_element("p").expect("auth body");
    let _ = body_copy.set_attribute(
        "style",
        "margin:0 0 18px 0;color:rgba(230,237,243,0.78);font-size:15px;line-height:1.5;",
    );
    body_copy.set_text_content(Some(
        "Use the existing Bun auth service to sign in before the web client joins multiplayer.",
    ));
    let _ = card.append_child(&body_copy);

    let status = document.create_element("p").expect("auth status");
    let _ = status.set_attribute(
        "style",
        "margin:0 0 18px 0;color:#f7d794;font-size:14px;line-height:1.4;",
    );
    status.set_text_content(Some(AUTH_STATUS_CHECKING));
    let _ = card.append_child(&status);

    let buttons = document.create_element("div").expect("auth buttons");
    let _ = buttons.set_attribute("style", "display:grid;gap:10px;");

    let mut onclicks = Vec::new();
    for (provider, label) in [
        ("google", "Continue With Google"),
        ("apple", "Continue With Apple"),
        ("linkedin", "Continue With LinkedIn"),
    ] {
        let button = document.create_element("button").expect("auth provider button");
        button.set_text_content(Some(label));
        let _ = button.set_attribute(
            "style",
            "width:100%;padding:14px 16px;border-radius:16px;border:1px solid rgba(255,255,255,0.14);background:rgba(255,255,255,0.06);color:#f6f8fb;font:600 15px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;transition:transform 120ms ease,background 120ms ease;",
        );
        let provider = provider.to_string();
        let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
            if let Ok(base_url) = api_base_url() {
                navigate_current_tab(&format!("{base_url}/auth/{provider}"));
            }
        }) as Box<dyn FnMut(WebEvent)>);
        let _ = button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
        let _ = buttons.append_child(&button);
        onclicks.push(onclick);
    }

    let guest_button = document.create_element("button").expect("auth guest button");
    guest_button.set_text_content(Some("Continue As Guest"));
    let _ = guest_button.set_attribute(
        "style",
        "width:100%;padding:14px 16px;border-radius:16px;border:1px solid rgba(247,215,148,0.35);background:rgba(247,215,148,0.12);color:#f7d794;font:600 15px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;transition:transform 120ms ease,background 120ms ease;",
    );
    let guest_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        AUTH_GUEST_QUEUE.with(|queue| {
            *queue.borrow_mut() = true;
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = guest_button
        .add_event_listener_with_callback("click", guest_onclick.as_ref().unchecked_ref());
    let _ = buttons.append_child(&guest_button);
    onclicks.push(guest_onclick);

    let _ = card.append_child(&buttons);

    let footnote = document.create_element("p").expect("auth footnote");
    let _ = footnote.set_attribute(
        "style",
        "margin:18px 0 0 0;color:rgba(230,237,243,0.56);font-size:12px;line-height:1.45;",
    );
    footnote.set_text_content(Some(
        "OAuth callbacks return to this page, then the game continues automatically.",
    ));
    let _ = card.append_child(&footnote);

    let _ = root.append_child(&card);
    let _ = body.append_child(&root);
    (root, status, onclicks)
}

fn auth_overlay_style() -> &'static str {
    "position:fixed;inset:0;display:grid;place-items:center;padding:24px;background:radial-gradient(circle at top,rgba(62,118,158,0.24),transparent 45%),rgba(5,8,12,0.72);backdrop-filter:blur(10px);z-index:60;"
}

fn player_avatar_panel_style() -> &'static str {
    "position:fixed;left:16px;top:16px;width:min(360px,calc(100vw - 32px));padding:16px;border-radius:18px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:45;"
}

fn auth_user_is_guest(user: &AuthUser) -> bool {
    user.id.starts_with("guest-")
}

fn fallback_element() -> Element {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.create_element("div").ok())
        .expect("fallback element")
}

fn request_auth_session() -> Receiver<AuthEvent> {
    let (tx, rx) = mpsc::channel();
    spawn_local(async move {
        let result = fetch_auth_user()
            .await
            .map_err(|error| format!("Unable to load login session: {error}"));
        let _ = tx.send(AuthEvent::Resolved(result));
    });
    rx
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
        BlockId::GoldOre => "Gold Ore",
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

fn navigate_current_tab(url: &str) {
    if let Some(window) = web_sys::window() {
        let _ = window.location().set_href(url);
    }
}

fn api_base_url() -> Result<String> {
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let location = window.location();
    let protocol = location
        .protocol()
        .map_err(|_| anyhow::anyhow!("window location protocol unavailable"))?;
    let host = location
        .host()
        .map_err(|_| anyhow::anyhow!("window location host unavailable"))?;
    Ok(format!("{protocol}//{host}/api/v1"))
}

async fn fetch_auth_user() -> Result<Option<AuthUser>> {
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);

    let request = Request::new_with_str_and_init(&format!("{}/auth/me", api_base_url()?), &init)
        .map_err(|error| anyhow::anyhow!("build auth request: {error:?}"))?;
    request
        .headers()
        .set("Accept", "application/json")
        .map_err(|error| anyhow::anyhow!("set auth headers: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("fetch auth session: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert auth response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!("auth endpoint returned HTTP {}", response.status()));
    }

    let body = JsFuture::from(
        response
            .json()
            .map_err(|error| anyhow::anyhow!("read auth response body: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse auth response body: {error:?}"))?;

    Ok(parse_auth_user(&body))
}

fn parse_auth_user(body: &JsValue) -> Option<AuthUser> {
    let user = js_get(body, "user")?;
    Some(AuthUser {
        id: js_get_string(&user, "id")?,
        name: js_get_string(&user, "name"),
        email: js_get_string(&user, "email"),
        avatar_selection: parse_avatar_selection(&user),
    })
}

fn parse_avatar_selection(user: &JsValue) -> Option<PlayerAvatarSelection> {
    let avatar_selection = js_get(user, "avatarSelection")?;
    Some(PlayerAvatarSelection {
        idle_model_url: js_get_string(&avatar_selection, "stationaryModelUrl"),
        run_model_url: js_get_string(&avatar_selection, "moveModelUrl"),
        dance_model_url: js_get_string(&avatar_selection, "specialModelUrl"),
    })
}

fn js_get(value: &JsValue, key: &str) -> Option<JsValue> {
    let value = js_sys::Reflect::get(value, &JsValue::from_str(key)).ok()?;
    if value.is_null() || value.is_undefined() {
        None
    } else {
        Some(value)
    }
}

fn js_get_string(value: &JsValue, key: &str) -> Option<String> {
    js_get(value, key)?.as_string()
}

fn create_player_avatar_panel() -> (Element, Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let closure = Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), fallback_element(), closure);
    };
    let Some(body) = document.body() else {
        let closure = Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), fallback_element(), closure);
    };

    let root = document.create_element("div").expect("player avatar panel");
    let _ = root.set_attribute("style", "display:none;");

    let title = document
        .create_element("h2")
        .expect("player avatar panel title");
    let _ = title.set_attribute(
        "style",
        "margin:0 0 6px 0;font:700 18px/1.2 ui-sans-serif,system-ui,sans-serif;",
    );
    title.set_text_content(Some("Player Avatar Animations"));
    let _ = root.append_child(&title);

    let copy = document
        .create_element("p")
        .expect("player avatar panel copy");
    let _ = copy.set_attribute(
        "style",
        "margin:0 0 14px 0;color:rgba(230,237,243,0.72);font-size:13px;line-height:1.45;",
    );
    copy.set_text_content(Some(
        "Upload one GLB each for idle, run, and dance. Press Esc if the mouse is locked so you can use the form.",
    ));
    let _ = root.append_child(&copy);

    let idle_input = create_player_avatar_file_input(&document, &root, "Idle", "player-avatar-idle");
    let run_input = create_player_avatar_file_input(&document, &root, "Run", "player-avatar-run");
    let dance_input = create_player_avatar_file_input(&document, &root, "Dance", "player-avatar-dance");

    let divider = document
        .create_element("div")
        .expect("player avatar divider");
    let _ = divider.set_attribute(
        "style",
        "margin:14px 0 10px 0;height:1px;background:rgba(255,255,255,0.10);",
    );
    let _ = root.append_child(&divider);

    let url_copy = document
        .create_element("p")
        .expect("player avatar url copy");
    let _ = url_copy.set_attribute(
        "style",
        "margin:0 0 8px 0;color:rgba(230,237,243,0.72);font-size:12px;line-height:1.45;",
    );
    url_copy.set_text_content(Some(
        "Slow connection? Paste public GLB links instead of uploading files.",
    ));
    let _ = root.append_child(&url_copy);

    let idle_url_input =
        create_player_avatar_url_input(&document, &root, "Idle URL", "player-avatar-idle-url");
    let run_url_input =
        create_player_avatar_url_input(&document, &root, "Run URL", "player-avatar-run-url");
    let dance_url_input =
        create_player_avatar_url_input(&document, &root, "Dance URL", "player-avatar-dance-url");

    let upload_button = document
        .create_element("button")
        .expect("player avatar upload button");
    upload_button.set_text_content(Some("Upload Avatar Set"));
    let _ = upload_button.set_attribute(
        "style",
        "margin-top:14px;width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.18);background:linear-gradient(180deg,#f6c665,#e8a93c);color:#1b1206;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = root.append_child(&upload_button);

    let save_urls_button = document
        .create_element("button")
        .expect("player avatar url save button");
    save_urls_button.set_text_content(Some("Save Avatar URLs"));
    let _ = save_urls_button.set_attribute(
        "style",
        "margin-top:10px;width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.12);background:rgba(255,255,255,0.08);color:#f2f6fb;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = root.append_child(&save_urls_button);

    let status = document
        .create_element("p")
        .expect("player avatar panel status");
    let _ = status.set_attribute(
        "style",
        "margin:12px 0 0 0;color:rgba(230,237,243,0.72);font-size:12px;line-height:1.45;",
    );
    status.set_text_content(Some("Choose three GLBs for idle, run, and dance."));
    let _ = root.append_child(&status);

    let status_for_click = status.clone();
    let idle_input_for_click = idle_input.clone();
    let run_input_for_click = run_input.clone();
    let dance_input_for_click = dance_input.clone();
    let upload_button_for_click = upload_button.clone();
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        let status = status_for_click.clone();
        let idle_input = idle_input_for_click.clone();
        let run_input = run_input_for_click.clone();
        let dance_input = dance_input_for_click.clone();
        let upload_button = upload_button_for_click.clone();
        spawn_local(async move {
            let _ = upload_button.set_attribute("disabled", "true");
            status.set_text_content(Some("Uploading avatar GLBs..."));
            if let Err(error) = upload_player_avatar_set(&idle_input, &run_input, &dance_input, &status).await {
                status.set_text_content(Some(&error.to_string()));
            }
            let _ = upload_button.remove_attribute("disabled");
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = upload_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());

    let status_for_url_click = status.clone();
    let idle_url_input_for_click = idle_url_input.clone();
    let run_url_input_for_click = run_url_input.clone();
    let dance_url_input_for_click = dance_url_input.clone();
    let save_urls_button_for_click = save_urls_button.clone();
    let save_urls_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        let status = status_for_url_click.clone();
        let idle_url_input = idle_url_input_for_click.clone();
        let run_url_input = run_url_input_for_click.clone();
        let dance_url_input = dance_url_input_for_click.clone();
        let save_urls_button = save_urls_button_for_click.clone();
        spawn_local(async move {
            let _ = save_urls_button.set_attribute("disabled", "true");
            status.set_text_content(Some("Saving avatar URLs..."));
            if let Err(error) = save_player_avatar_urls(
                &idle_url_input,
                &run_url_input,
                &dance_url_input,
                &status,
            )
            .await
            {
                status.set_text_content(Some(&error.to_string()));
            }
            let _ = save_urls_button.remove_attribute("disabled");
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = save_urls_button.add_event_listener_with_callback(
        "click",
        save_urls_onclick.as_ref().unchecked_ref(),
    );
    save_urls_onclick.forget();

    let _ = body.append_child(&root);
    (root, status, onclick)
}

fn create_player_avatar_file_input(
    document: &Document,
    root: &Element,
    label: &str,
    id: &str,
) -> HtmlInputElement {
    let wrapper = document
        .create_element("label")
        .expect("player avatar input wrapper");
    let _ = wrapper.set_attribute(
        "style",
        "display:grid;gap:6px;margin-top:10px;font-size:12px;font-weight:700;color:#f4f7fb;",
    );
    wrapper.set_text_content(Some(label));

    let input = document
        .create_element("input")
        .expect("player avatar file input")
        .dyn_into::<HtmlInputElement>()
        .expect("player avatar input cast");
    input.set_id(id);
    input.set_type("file");
    input.set_accept(".glb,model/gltf-binary");
    let _ = input.set_attribute(
        "style",
        "display:block;width:100%;padding:10px;border-radius:12px;border:1px solid rgba(255,255,255,0.12);background:rgba(255,255,255,0.06);color:#dce6ef;font:500 12px/1.3 ui-sans-serif,system-ui,sans-serif;",
    );

    let _ = wrapper.append_child(&input);
    let _ = root.append_child(&wrapper);
    input
}

fn create_player_avatar_url_input(
    document: &Document,
    root: &Element,
    label: &str,
    id: &str,
) -> HtmlInputElement {
    let wrapper = document
        .create_element("label")
        .expect("player avatar url input wrapper");
    let _ = wrapper.set_attribute(
        "style",
        "display:grid;gap:6px;margin-top:10px;font-size:12px;font-weight:700;color:#f4f7fb;",
    );
    wrapper.set_text_content(Some(label));

    let input = document
        .create_element("input")
        .expect("player avatar url input")
        .dyn_into::<HtmlInputElement>()
        .expect("player avatar url input cast");
    input.set_id(id);
    input.set_type("url");
    input.set_placeholder("https://...");
    let _ = input.set_attribute(
        "style",
        "display:block;width:100%;padding:10px;border-radius:12px;border:1px solid rgba(255,255,255,0.12);background:rgba(255,255,255,0.06);color:#dce6ef;font:500 12px/1.3 ui-sans-serif,system-ui,sans-serif;",
    );

    let _ = wrapper.append_child(&input);
    let _ = root.append_child(&wrapper);
    input
}

async fn upload_player_avatar_set(
    idle_input: &HtmlInputElement,
    run_input: &HtmlInputElement,
    dance_input: &HtmlInputElement,
    status: &Element,
) -> Result<()> {
    let idle_file = input_selected_file(idle_input);
    let run_file = input_selected_file(run_input);
    let dance_file = input_selected_file(dance_input);
    if idle_file.is_none() && run_file.is_none() && dance_file.is_none() {
        return Err(anyhow::anyhow!(
            "Choose at least one GLB before uploading."
        ));
    }

    let mut idle_url = None;
    let mut run_url = None;
    let mut dance_url = None;

    if let Some(file) = idle_file.as_ref() {
        status.set_text_content(Some("Uploading idle GLB..."));
        idle_url = upload_player_avatar_slot("idle", file).await?;
    }
    if let Some(file) = run_file.as_ref() {
        status.set_text_content(Some("Uploading run GLB..."));
        run_url = upload_player_avatar_slot("run", file).await?;
    }
    if let Some(file) = dance_file.as_ref() {
        status.set_text_content(Some("Uploading dance GLB..."));
        dance_url = upload_player_avatar_slot("dance", file).await?;
    }

    if idle_url.is_some() || run_url.is_some() || dance_url.is_some() {
        status.set_text_content(Some("Saving avatar URLs..."));
        save_player_avatar_url_values(
            idle_url.as_deref(),
            run_url.as_deref(),
            dance_url.as_deref(),
        )
        .await?;
    }

    if idle_file.is_some() {
        idle_input.set_value("");
    }
    if run_file.is_some() {
        run_input.set_value("");
    }
    if dance_file.is_some() {
        dance_input.set_value("");
    }
    status.set_text_content(Some("Avatar upload complete."));
    Ok(())
}

async fn upload_player_avatar_slot(slot: &str, file: &web_sys::File) -> Result<Option<String>> {
    if let Ok(public_url) = upload_player_avatar_slot_direct(slot, file).await {
        return Ok(Some(public_url));
    }

    let form_data = FormData::new().map_err(|error| anyhow::anyhow!("create form data: {error:?}"))?;
    form_data
        .append_with_str("slot", slot)
        .map_err(|error| anyhow::anyhow!("append upload slot: {error:?}"))?;
    form_data
        .append_with_blob_and_filename("file", file, &file.name())
        .map_err(|error| anyhow::anyhow!("append avatar file: {error:?}"))?;

    let init = RequestInit::new();
    init.set_method("POST");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);
    init.set_body(&JsValue::from(form_data));

    let request = Request::new_with_str_and_init(
        &format!("{}/auth/player-avatar/upload", api_base_url()?),
        &init,
    )
    .map_err(|error| anyhow::anyhow!("build player avatar upload request: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("upload player avatars: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert player avatar upload response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!(
            "{} avatar upload failed with HTTP {}.",
            slot,
            response.status()
        ));
    }

    Ok(None)
}

async fn upload_player_avatar_slot_direct(slot: &str, file: &web_sys::File) -> Result<String> {
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);

    let payload = js_sys::Object::new();
    js_sys::Reflect::set(&payload, &JsValue::from_str("slot"), &JsValue::from_str(slot))
        .map_err(|error| anyhow::anyhow!("set direct upload slot: {error:?}"))?;
    js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("fileName"),
        &JsValue::from_str(&file.name()),
    )
    .map_err(|error| anyhow::anyhow!("set direct upload file name: {error:?}"))?;
    js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("contentType"),
        &JsValue::from_str("model/gltf-binary"),
    )
    .map_err(|error| anyhow::anyhow!("set direct upload content type: {error:?}"))?;
    let json = js_sys::JSON::stringify(&payload)
        .map_err(|error| anyhow::anyhow!("stringify direct upload payload: {error:?}"))?
        .as_string()
        .ok_or_else(|| anyhow::anyhow!("direct upload payload was not a string"))?;
    init.set_body(&JsValue::from_str(&json));

    let request = Request::new_with_str_and_init(
        &format!("{}/auth/player-avatar/upload-url", api_base_url()?),
        &init,
    )
    .map_err(|error| anyhow::anyhow!("build direct upload request: {error:?}"))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|error| anyhow::anyhow!("set direct upload headers: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("request direct upload URL: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert direct upload URL response"))?;
    if !response.ok() {
        return Err(anyhow::anyhow!(
            "direct upload URL request failed with HTTP {}",
            response.status()
        ));
    }

    let body = JsFuture::from(
        response
            .json()
            .map_err(|error| anyhow::anyhow!("read direct upload URL response: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse direct upload URL response: {error:?}"))?;
    let upload_url = js_get_string(&body, "uploadUrl")
        .ok_or_else(|| anyhow::anyhow!("direct upload URL missing uploadUrl"))?;
    let public_url = js_get_string(&body, "publicUrl")
        .ok_or_else(|| anyhow::anyhow!("direct upload URL missing publicUrl"))?;
    let content_type =
        js_get_string(&body, "contentType").unwrap_or_else(|| "model/gltf-binary".to_string());
    let upload_headers = js_get(&body, "uploadHeaders");
    let upload_acl = upload_headers
        .as_ref()
        .and_then(|headers| js_get_string(headers, "x-amz-acl"))
        .unwrap_or_else(|| "public-read".to_string());
    let upload_cache_control = upload_headers
        .as_ref()
        .and_then(|headers| js_get_string(headers, "Cache-Control"));

    let upload_init = RequestInit::new();
    upload_init.set_method("PUT");
    upload_init.set_mode(RequestMode::Cors);
    upload_init.set_body(&JsValue::from(file.clone()));
    let upload_request = Request::new_with_str_and_init(&upload_url, &upload_init)
        .map_err(|error| anyhow::anyhow!("build direct PUT request: {error:?}"))?;
    upload_request
        .headers()
        .set("Content-Type", &content_type)
        .map_err(|error| anyhow::anyhow!("set direct PUT content type: {error:?}"))?;
    upload_request
        .headers()
        .set("x-amz-acl", &upload_acl)
        .map_err(|error| anyhow::anyhow!("set direct PUT ACL: {error:?}"))?;
    if let Some(upload_cache_control) = upload_cache_control.as_deref() {
        upload_request
            .headers()
            .set("Cache-Control", upload_cache_control)
            .map_err(|error| anyhow::anyhow!("set direct PUT cache control: {error:?}"))?;
    }

    let upload_response_value = JsFuture::from(window.fetch_with_request(&upload_request))
        .await
        .map_err(|error| anyhow::anyhow!("upload file to CDN: {error:?}"))?;
    let upload_response: Response = upload_response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert direct PUT response"))?;
    if !upload_response.ok() {
        return Err(anyhow::anyhow!(
            "direct file upload failed with HTTP {}",
            upload_response.status()
        ));
    }

    Ok(public_url)
}

async fn save_player_avatar_urls(
    idle_input: &HtmlInputElement,
    run_input: &HtmlInputElement,
    dance_input: &HtmlInputElement,
    status: &Element,
) -> Result<()> {
    let idle_url = idle_input.value().trim().to_string();
    let run_url = run_input.value().trim().to_string();
    let dance_url = dance_input.value().trim().to_string();

    if idle_url.is_empty() && run_url.is_empty() && dance_url.is_empty() {
        return Err(anyhow::anyhow!(
            "Paste at least one public avatar URL before saving."
        ));
    }

    save_player_avatar_url_values(
        (!idle_url.is_empty()).then_some(idle_url.as_str()),
        (!run_url.is_empty()).then_some(run_url.as_str()),
        (!dance_url.is_empty()).then_some(dance_url.as_str()),
    )
    .await?;
    status.set_text_content(Some("Saved avatar URL changes."));
    Ok(())
}

async fn save_player_avatar_url_values(
    idle_url: Option<&str>,
    run_url: Option<&str>,
    dance_url: Option<&str>,
) -> Result<()> {
    let payload = js_sys::Object::new();
    let mut field_count = 0usize;
    if let Some(idle_url) = idle_url {
        js_sys::Reflect::set(
            &payload,
            &JsValue::from_str("idleModelUrl"),
            &JsValue::from_str(idle_url),
        )
        .map_err(|error| anyhow::anyhow!("set idle URL payload: {error:?}"))?;
        field_count += 1;
    }
    if let Some(run_url) = run_url {
        js_sys::Reflect::set(
            &payload,
            &JsValue::from_str("runModelUrl"),
            &JsValue::from_str(run_url),
        )
        .map_err(|error| anyhow::anyhow!("set run URL payload: {error:?}"))?;
        field_count += 1;
    }
    if let Some(dance_url) = dance_url {
        js_sys::Reflect::set(
            &payload,
            &JsValue::from_str("danceModelUrl"),
            &JsValue::from_str(dance_url),
        )
        .map_err(|error| anyhow::anyhow!("set dance URL payload: {error:?}"))?;
        field_count += 1;
    }
    if field_count == 0 {
        return Err(anyhow::anyhow!("No avatar URL values were provided."));
    }

    let json = js_sys::JSON::stringify(&payload)
        .map_err(|error| anyhow::anyhow!("stringify avatar URL payload: {error:?}"))?
        .as_string()
        .ok_or_else(|| anyhow::anyhow!("avatar URL payload was not a string"))?;

    let init = RequestInit::new();
    init.set_method("PATCH");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);
    init.set_body(&JsValue::from_str(&json));

    let request = Request::new_with_str_and_init(
        &format!("{}/auth/player-avatar", api_base_url()?),
        &init,
    )
    .map_err(|error| anyhow::anyhow!("build avatar URL save request: {error:?}"))?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(|error| anyhow::anyhow!("set avatar URL headers: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("save avatar URLs: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert avatar URL response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!(
            "Saving avatar URLs failed with HTTP {}.",
            response.status()
        ));
    }

    Ok(())
}

fn input_selected_file(input: &HtmlInputElement) -> Option<web_sys::File> {
    input.files()?.get(0)
}

fn request_remote_avatar_model(url: String, sender: Sender<RemoteAvatarEvent>) {
    spawn_local(async move {
        let result = fetch_remote_avatar_bytes(&url).await;
        let event = match result {
            Ok(bytes) => RemoteAvatarEvent::Loaded { url, bytes },
            Err(error) => RemoteAvatarEvent::Failed {
                url,
                message: error.to_string(),
            },
        };
        let _ = sender.send(event);
    });
}

async fn fetch_remote_avatar_bytes(url: &str) -> Result<Vec<u8>> {
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_mode(RequestMode::Cors);

    let request = Request::new_with_str_and_init(url, &init)
        .map_err(|error| anyhow::anyhow!("build remote avatar request: {error:?}"))?;
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("fetch remote avatar bytes: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert remote avatar response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!(
            "remote avatar request returned HTTP {}",
            response.status()
        ));
    }

    let buffer = JsFuture::from(
        response
            .array_buffer()
            .map_err(|error| anyhow::anyhow!("read remote avatar bytes: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse remote avatar bytes: {error:?}"))?;

    Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}

fn build_remote_avatar_asset(renderer: &Renderer<'_>, bytes: &[u8]) -> Result<RemoteAvatarAsset> {
    let (document, buffers, images) = gltf::import_slice(bytes)?;
    let scene = document
        .default_scene()
        .or_else(|| document.scenes().next())
        .ok_or_else(|| anyhow::anyhow!("remote avatar glb has no scene"))?;
    let animation = document
        .animations()
        .next()
        .ok_or_else(|| anyhow::anyhow!("remote avatar glb has no animation clips"))?;
    let animation = parse_avatar_animation_clip(&animation, &buffers)?;

    let node_count = document.nodes().len();
    let mut node_children = vec![Vec::new(); node_count];
    let mut rest_locals = vec![
        NodeTransform {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
        };
        node_count
    ];
    for node in document.nodes() {
        let (translation, rotation, scale) = node.transform().decomposed();
        rest_locals[node.index()] = NodeTransform {
            translation: Vec3::from_array(translation),
            rotation: Quat::from_xyzw(rotation[0], rotation[1], rotation[2], rotation[3]),
            scale: Vec3::from_array(scale),
        };
        for child in node.children() {
            node_children[node.index()].push(child.index());
        }
    }
    let root_nodes = scene.nodes().map(|node| node.index()).collect::<Vec<_>>();

    let mut selected_skin = None;
    let mut skinned_vertices = Vec::new();
    let mut indices = Vec::new();
    let mut image_index = None;

    for node in scene.nodes() {
        append_skinned_gltf_node_meshes(
            &node,
            &buffers,
            &mut skinned_vertices,
            &mut indices,
            &mut image_index,
            &mut selected_skin,
        );
    }

    let skin = selected_skin.ok_or_else(|| anyhow::anyhow!("remote avatar glb has no skin"))?;
    if skinned_vertices.is_empty() || indices.is_empty() {
        anyhow::bail!("remote avatar glb has no skinned triangle vertices");
    }
    if skin.joint_nodes.len() > MAX_SKIN_JOINTS {
        anyhow::bail!("remote avatar joint count exceeds renderer limit");
    }
    let bind_globals = compute_global_node_matrices(&root_nodes, &node_children, &rest_locals);
    let bind_pose_bounds = skinned_model_bounds(
        &skinned_vertices,
        &skin.joint_nodes,
        &skin.inverse_bind_matrices,
        &bind_globals,
    )
    .ok_or_else(|| anyhow::anyhow!("remote avatar skinned bind pose has no bounds"))?;
    let model_normalization = build_remote_avatar_normalization(bind_pose_bounds);

    let vertices = skinned_vertices
        .into_iter()
        .map(|vertex| AnimatedVertex {
            position: vertex.position.to_array(),
            normal: vertex.normal.to_array(),
            uv: vertex.uv,
            joints: [
                vertex.joints[0] as f32,
                vertex.joints[1] as f32,
                vertex.joints[2] as f32,
                vertex.joints[3] as f32,
            ],
            weights: vertex.weights,
        })
        .collect::<Vec<_>>();

    let rgba = match image_index.and_then(|index| images.get(index)) {
        Some(image) => image_to_rgba_pixels(image)?,
        None => vec![255, 255, 255, 255],
    };
    let (width, height) = image_index
        .and_then(|index| images.get(index))
        .map(|image| (image.width.max(1), image.height.max(1)))
        .unwrap_or((1, 1));
    let texture = renderer.create_dynamic_texture(width, height);
    renderer.update_dynamic_texture_rgba(&texture, &rgba);
    let mesh = renderer.create_animated_mesh(&vertices, &indices, &texture);

    Ok(RemoteAvatarAsset {
        mesh,
        node_children,
        root_nodes,
        rest_locals,
        joint_nodes: skin.joint_nodes,
        inverse_bind_matrices: skin.inverse_bind_matrices,
        animation,
        model_normalization,
    })
}

struct ParsedSkin {
    joint_nodes: Vec<usize>,
    inverse_bind_matrices: Vec<Mat4>,
}

fn append_skinned_gltf_node_meshes(
    node: &gltf::Node<'_>,
    buffers: &[gltf::buffer::Data],
    vertices: &mut Vec<AnimatedImportedVertex>,
    indices: &mut Vec<u32>,
    image_index: &mut Option<usize>,
    selected_skin: &mut Option<ParsedSkin>,
) {
    if let Some(mesh) = node.mesh() {
        let Some(node_skin) = node.skin() else {
            for child in node.children() {
                append_skinned_gltf_node_meshes(
                    &child,
                    buffers,
                    vertices,
                    indices,
                    image_index,
                    selected_skin,
                );
            }
            return;
        };

        let parsed_skin = selected_skin.get_or_insert_with(|| {
            let joint_nodes = node_skin.joints().map(|joint| joint.index()).collect::<Vec<_>>();
            let inverse_bind_matrices = node_skin
                .reader(|buffer| Some(&buffers[buffer.index()].0))
                .read_inverse_bind_matrices()
                .map(|matrices| {
                    matrices
                        .map(|matrix| Mat4::from_cols_array_2d(&matrix))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_else(|| vec![Mat4::IDENTITY; joint_nodes.len()]);
            ParsedSkin {
                joint_nodes,
                inverse_bind_matrices,
            }
        });

        if parsed_skin.joint_nodes
            != node_skin.joints().map(|joint| joint.index()).collect::<Vec<_>>()
        {
            for child in node.children() {
                append_skinned_gltf_node_meshes(
                    &child,
                    buffers,
                    vertices,
                    indices,
                    image_index,
                    selected_skin,
                );
            }
            return;
        }

        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                continue;
            }
            if image_index.is_none() {
                *image_index = primitive
                    .material()
                    .pbr_metallic_roughness()
                    .base_color_texture()
                    .map(|texture| texture.texture().source().index());
            }

            let reader = primitive.reader(|buffer| Some(&buffers[buffer.index()].0));
            let Some(positions) = reader.read_positions() else {
                continue;
            };
            let Some(joints) = reader.read_joints(0) else {
                continue;
            };
            let Some(weights) = reader.read_weights(0) else {
                continue;
            };

            let primitive_positions = positions.collect::<Vec<_>>();
            let primitive_joints = joints.into_u16().collect::<Vec<_>>();
            let primitive_weights = weights.into_f32().collect::<Vec<_>>();
            let normals = reader.read_normals().map(|values| values.collect::<Vec<_>>());
            let texcoords = reader
                .read_tex_coords(0)
                .map(|coords| coords.into_f32().collect::<Vec<_>>());
            let base_vertex = vertices.len() as u32;

            for index in 0..primitive_positions.len() {
                let mut weight_values = primitive_weights.get(index).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]);
                let weight_sum = weight_values.iter().sum::<f32>();
                if weight_sum > 0.0001 {
                    for weight in &mut weight_values {
                        *weight /= weight_sum;
                    }
                } else {
                    weight_values = [1.0, 0.0, 0.0, 0.0];
                }

                vertices.push(AnimatedImportedVertex {
                    position: Vec3::from_array(primitive_positions[index]),
                    normal: normals
                        .as_ref()
                        .and_then(|values| values.get(index))
                        .map(|value| Vec3::from_array(*value).normalize_or_zero())
                        .unwrap_or(Vec3::Y),
                    uv: texcoords
                        .as_ref()
                        .and_then(|values| values.get(index))
                        .copied()
                        .unwrap_or([0.5, 0.5]),
                    joints: primitive_joints.get(index).copied().unwrap_or([0, 0, 0, 0]),
                    weights: weight_values,
                });
            }

            if let Some(read_indices) = reader.read_indices() {
                indices.extend(read_indices.into_u32().map(|index| base_vertex + index));
            } else {
                indices.extend((0..primitive_positions.len() as u32).map(|index| base_vertex + index));
            }
        }
    }

    for child in node.children() {
        append_skinned_gltf_node_meshes(
            &child,
            buffers,
            vertices,
            indices,
            image_index,
            selected_skin,
        );
    }
}

fn compute_global_node_matrices(
    root_nodes: &[usize],
    node_children: &[Vec<usize>],
    locals: &[NodeTransform],
) -> Vec<Mat4> {
    let mut globals = vec![Mat4::IDENTITY; locals.len()];
    for &root in root_nodes {
        populate_global_node_matrices(root, Mat4::IDENTITY, node_children, locals, &mut globals);
    }
    globals
}

fn populate_global_node_matrices(
    node_index: usize,
    parent: Mat4,
    node_children: &[Vec<usize>],
    locals: &[NodeTransform],
    globals: &mut [Mat4],
) {
    let current = parent * locals[node_index].matrix();
    globals[node_index] = current;
    for &child in &node_children[node_index] {
        populate_global_node_matrices(child, current, node_children, locals, globals);
    }
}

fn skinned_model_bounds(
    vertices: &[AnimatedImportedVertex],
    joint_nodes: &[usize],
    inverse_bind_matrices: &[Mat4],
    globals: &[Mat4],
) -> Option<(Vec3, Vec3)> {
    let mut bounds: Option<(Vec3, Vec3)> = None;
    for vertex in vertices {
        let position = skin_vertex_position(vertex, joint_nodes, inverse_bind_matrices, globals);
        bounds = Some(match bounds {
            Some((min, max)) => (min.min(position), max.max(position)),
            None => (position, position),
        });
    }
    bounds
}

fn skin_vertex_position(
    vertex: &AnimatedImportedVertex,
    joint_nodes: &[usize],
    inverse_bind_matrices: &[Mat4],
    globals: &[Mat4],
) -> Vec3 {
    let mut result = Vec3::ZERO;
    let mut total_weight = 0.0;
    for influence in 0..4 {
        let weight = vertex.weights[influence];
        if weight <= 0.0 {
            continue;
        }
        let joint_index = vertex.joints[influence] as usize;
        let Some(&joint_node) = joint_nodes.get(joint_index) else {
            continue;
        };
        let inverse_bind = inverse_bind_matrices
            .get(joint_index)
            .copied()
            .unwrap_or(Mat4::IDENTITY);
        let joint_matrix = globals[joint_node] * inverse_bind;
        result += joint_matrix.transform_point3(vertex.position) * weight;
        total_weight += weight;
    }
    if total_weight > 0.0 {
        result / total_weight
    } else {
        vertex.position
    }
}

fn build_remote_avatar_normalization((min, max): (Vec3, Vec3)) -> Mat4 {
    let import_rotation = Mat4::from_rotation_x(-std::f32::consts::FRAC_PI_2);
    let rotated_corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(max.x, max.y, max.z),
    ]
    .into_iter()
    .map(|corner| import_rotation.transform_point3(corner))
    .collect::<Vec<_>>();
    let mut rotated_min = rotated_corners[0];
    let mut rotated_max = rotated_corners[0];
    for corner in &rotated_corners[1..] {
        rotated_min = rotated_min.min(*corner);
        rotated_max = rotated_max.max(*corner);
    }

    let scale =
        (PLAYER_HEIGHT * REMOTE_AVATAR_SCALE_FACTOR) / (rotated_max.y - rotated_min.y).max(0.001);
    let center_x = (rotated_min.x + rotated_max.x) * 0.5;
    let center_z = (rotated_min.z + rotated_max.z) * 0.5;
    Mat4::from_translation(Vec3::new(
        -center_x * scale,
        -rotated_min.y * scale,
        -center_z * scale,
    )) * Mat4::from_scale(Vec3::splat(scale))
        * import_rotation
}

fn evaluate_avatar_skin_matrices(asset: &RemoteAvatarAsset, playback_time: f32) -> Vec<Mat4> {
    let mut locals = asset.rest_locals.clone();
    apply_animation_to_locals(&mut locals, &asset.animation, playback_time);
    let globals = compute_global_node_matrices(&asset.root_nodes, &asset.node_children, &locals);

    asset
        .joint_nodes
        .iter()
        .enumerate()
        .map(|(index, joint_node)| {
            globals[*joint_node]
                * asset
                    .inverse_bind_matrices
                    .get(index)
                    .copied()
                    .unwrap_or(Mat4::IDENTITY)
        })
        .collect()
}

fn apply_animation_to_locals(
    locals: &mut [NodeTransform],
    animation: &AvatarAnimationClip,
    playback_time: f32,
) {
    for channel in &animation.channels {
        if channel.keyframe_times.is_empty() {
            continue;
        }
        let node_local = &mut locals[channel.node_index];
        match (&channel.property, &channel.outputs) {
            (AnimationProperty::Translation, AnimationOutputs::Vec3(values)) => {
                node_local.translation = sample_vec3_channel(&channel.keyframe_times, values, playback_time);
            }
            (AnimationProperty::Scale, AnimationOutputs::Vec3(values)) => {
                node_local.scale = sample_vec3_channel(&channel.keyframe_times, values, playback_time);
            }
            (AnimationProperty::Rotation, AnimationOutputs::Quat(values)) => {
                node_local.rotation = sample_quat_channel(&channel.keyframe_times, values, playback_time);
            }
            _ => {}
        }
    }
}

fn sample_vec3_channel(times: &[f32], values: &[Vec3], playback_time: f32) -> Vec3 {
    if times.len() == 1 || values.len() == 1 {
        return values[0];
    }
    let (left, right, alpha) = animation_keyframe_span(times, playback_time);
    values[left].lerp(values[right], alpha)
}

fn sample_quat_channel(times: &[f32], values: &[Quat], playback_time: f32) -> Quat {
    if times.len() == 1 || values.len() == 1 {
        return values[0];
    }
    let (left, right, alpha) = animation_keyframe_span(times, playback_time);
    values[left].slerp(values[right], alpha).normalize()
}

fn animation_keyframe_span(times: &[f32], playback_time: f32) -> (usize, usize, f32) {
    if playback_time <= times[0] {
        return (0, 0, 0.0);
    }
    for index in 0..times.len().saturating_sub(1) {
        let start = times[index];
        let end = times[index + 1];
        if playback_time <= end {
            let alpha = if end > start {
                (playback_time - start) / (end - start)
            } else {
                0.0
            };
            return (index, index + 1, alpha.clamp(0.0, 1.0));
        }
    }
    let last = times.len().saturating_sub(1);
    (last, last, 0.0)
}

fn parse_avatar_animation_clip(
    animation: &gltf::Animation<'_>,
    buffers: &[gltf::buffer::Data],
) -> Result<AvatarAnimationClip> {
    let mut channels = Vec::new();
    let mut duration_seconds = 0.0_f32;

    for channel in animation.channels() {
        let reader = channel.reader(|buffer| Some(&buffers[buffer.index()].0));
        let Some(inputs) = reader.read_inputs() else {
            continue;
        };
        let keyframe_times = inputs.collect::<Vec<_>>();
        if let Some(last) = keyframe_times.last().copied() {
            duration_seconds = duration_seconds.max(last);
        }
        let property = match channel.target().property() {
            gltf::animation::Property::Translation => AnimationProperty::Translation,
            gltf::animation::Property::Rotation => AnimationProperty::Rotation,
            gltf::animation::Property::Scale => AnimationProperty::Scale,
            gltf::animation::Property::MorphTargetWeights => continue,
        };

        let outputs = match reader.read_outputs() {
            Some(gltf::animation::util::ReadOutputs::Translations(values)) => {
                AnimationOutputs::Vec3(values.map(Vec3::from_array).collect())
            }
            Some(gltf::animation::util::ReadOutputs::Scales(values)) => {
                AnimationOutputs::Vec3(values.map(Vec3::from_array).collect())
            }
            Some(gltf::animation::util::ReadOutputs::Rotations(values)) => {
                AnimationOutputs::Quat(
                    values
                        .into_f32()
                        .map(|value| Quat::from_xyzw(value[0], value[1], value[2], value[3]))
                        .collect(),
                )
            }
            _ => continue,
        };

        channels.push(AvatarAnimationChannel {
            node_index: channel.target().node().index(),
            property,
            keyframe_times,
            outputs,
        });
    }

    if channels.is_empty() {
        anyhow::bail!("remote avatar first animation clip has no supported channels");
    }

    Ok(AvatarAnimationClip {
        duration_seconds: duration_seconds.max(0.0001),
        channels,
    })
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
                    vertex_floats: Vec::new(),
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

            let _ = tx.send(MeshBuildResult {
                position: ChunkPos { x, z },
                vertex_floats,
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
        let _ = message_tx.send(NetworkEvent::ServerBytes(bytes));
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
    world_seed: u64,
) {
    let job = js_sys::Object::new();
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("kind"), &JsValue::from_str("build"));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("x"), &JsValue::from_f64(f64::from(position.x)));
    let _ = js_sys::Reflect::set(&job, &JsValue::from_str("z"), &JsValue::from_f64(f64::from(position.z)));
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("worldSeed"),
        &JsValue::from_str(&world_seed.to_string()),
    );
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
    let protocol = location
        .protocol()
        .unwrap_or_else(|_| "http:".to_string());
    let host = location
        .host()
        .unwrap_or_else(|_| "127.0.0.1:3001".to_string());
    let ws_protocol = if protocol == "https:" { "wss" } else { "ws" };
    Ok(format!("{ws_protocol}://{host}/ws"))
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

fn scaled_render_size(size: PhysicalSize<u32>) -> PhysicalSize<u32> {
    let width = ((size.width.max(1) as f32) * WEB_RENDER_SCALE).round().max(1.0) as u32;
    let height = ((size.height.max(1) as f32) * WEB_RENDER_SCALE).round().max(1.0) as u32;
    PhysicalSize::new(width, height)
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

fn ordered_desired_chunk_positions(center: ChunkPos, radius: i32) -> Vec<ChunkPos> {
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
    vertex_floats: Vec<f32>,
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
    ServerBytes(Vec<u8>),
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
            | BlockId::GoldOre
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
        12 => BlockId::GoldOre,
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
            material_id: 0.0,
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

fn player_anchor_from_eye_with_look(eye: Vec3, _look: Vec3) -> PlayerAnchor {
    let body = eye - Vec3::Y * PLAYER_EYE_HEIGHT;
    let head = body + Vec3::Y * PLAYER_HEIGHT;
    let media = head + Vec3::Y * 0.95;
    PlayerAnchor { body, head, media }
}
