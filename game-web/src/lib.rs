#![cfg(target_arch = "wasm32")]

use anyhow::Result;
use glam::{Mat4, Quat, Vec3, Vec4};
use shared_math::{CHUNK_DEPTH, CHUNK_HEIGHT, CHUNK_WIDTH, ChunkPos, LocalVoxelPos, WorldPos};
use shared_protocol::{
    BreakBlockRequest, CaptureWildPetResult, CaptureWildPetStatus, CapturedPet,
    CapturedPetsSnapshot, ClientHello, ClientMessage, ClientWebRtcSignal, CollectedWeapon,
    CollectedWeaponsSnapshot, InventorySnapshot, LoginRequest, PROTOCOL_VERSION, PeerRealtimeState,
    PetIdentity, PetStateSnapshot, PetWeaponAssignment, PetWeaponShot, PickupWorldWeaponResult,
    PlaceBlockRequest, PlayerInputTick, PlayerLeft, ServerMessage, ServerWebRtcSignal,
    SubscribeChunks, UpdatePetPartyRequest, UpdatePetPartyResult, WeaponIdentity,
    WebRtcSignalPayload, WildPetMotionSnapshot, WildPetSnapshot, WildPetUnload,
    WorldWeaponSnapshot, WorldWeaponUnload, decode, encode,
};
use shared_world::{BlockId, ChunkData, TerrainGenerator};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    BinaryType, Blob, CanvasRenderingContext2d, CloseEvent, Document, Element, ErrorEvent,
    Event as WebEvent, FormData, HtmlCanvasElement, HtmlInputElement, HtmlVideoElement,
    MediaStream, MediaStreamConstraints, MediaStreamTrack, MessageEvent, Request,
    RequestCredentials, RequestInit, RequestMode, Response, RtcDataChannel, RtcDataChannelEvent,
    RtcIceCandidateInit, RtcPeerConnection, RtcPeerConnectionIceEvent, RtcSdpType,
    RtcSessionDescriptionInit, RtcTrackEvent, Url, WebSocket, Worker,
};
use web_time::Instant;
use wgpu_lite::{
    AnimatedMesh, AnimatedMeshDraw, AnimatedVertex, DynamicTexture, MAX_SKIN_JOINTS, Mesh,
    Renderer, TexturedMesh, TexturedMeshDraw, Vertex,
};
use winit::dpi::PhysicalSize;
use winit::event::{
    DeviceEvent, ElementState, Event, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent,
};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::platform::web::WindowExtWebSys;
use winit::window::WindowBuilder;

const PET_MODEL_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/models/Meshy_AI_A_cute_dog_0326155854_texture.glb"
));
const DEFAULT_WORLD_SEED: u64 = 0xA66D_E601;
const WEB_RADIUS: i32 = 6;
#[allow(dead_code)]
const INITIAL_WEB_RADIUS: i32 = 1;
const SPAWN_READY_RADIUS: i32 = 1;
const CHUNK_QUALITY_STORAGE_KEY: &str = "augmego.chunk_quality";
const CHUNK_WORLD_RADIUS: f32 = (CHUNK_WIDTH as f32) * 0.5;
const DRAW_DISTANCE_CHUNKS: f32 = 6.0;
const MAX_VISIBLE_CHUNK_MESHES: usize = 56;
const MESH_WORKER_COUNT: usize = 3;
const DEFAULT_GENERATION_BUDGET_PER_UPDATE: usize = 2;
const DEFAULT_MESH_UPLOAD_BUDGET_PER_UPDATE: usize = 1;
const MAX_IDLE_MESH_UPLOAD_BUDGET_PER_UPDATE: usize = 2;
const DEFAULT_NETWORK_MESSAGE_BUDGET_PER_TICK: usize = 32;
const PENDING_REPRIORITIZE_DOT_THRESHOLD: f32 = 0.80;
const DEFAULT_WEB_RENDER_SCALE: f32 = 0.8;
const MIN_WEB_RENDER_SCALE: f32 = 0.6;
const WEB_RENDER_SCALE_STEP: f32 = 0.1;
const RENDER_SCALE_SMOOTHING: f32 = 0.08;
const RENDER_SCALE_SLOW_FRAME_SECS: f32 = 1.0 / 45.0;
const RENDER_SCALE_FAST_FRAME_SECS: f32 = 1.0 / 65.0;
const CAMERA_VERTICAL_FOV_RADIANS: f32 = 60.0_f32.to_radians();
const CHUNK_NEAR_VISIBILITY_DISTANCE: f32 = CHUNK_WIDTH as f32 * 2.0;
const CHUNK_HORIZONTAL_CULL_MARGIN_RADIANS: f32 = 10.0_f32.to_radians();
const RENDER_SCALE_WARMUP: Duration = Duration::from_secs(2);
const RENDER_SCALE_ADJUST_COOLDOWN: Duration = Duration::from_millis(900);
const PERF_PANEL_UPDATE_INTERVAL: Duration = Duration::from_millis(250);
const REMOTE_MEDIA_UPLOAD_INTERVAL: Duration = Duration::from_millis(66);
const PET_IMPOSTOR_REFRESH_INTERVAL: Duration = Duration::from_millis(200);
const PLAYER_RADIUS: f32 = 0.35;
const PLAYER_HEIGHT: f32 = 1.8;
const PLAYER_EYE_HEIGHT: f32 = 1.62;
const PLAYER_WALK_SPEED: f32 = 7.5;
const PLAYER_SPRINT_SPEED: f32 = 11.0;
const PLAYER_JUMP_SPEED: f32 = 9.5;
const PLAYER_GRAVITY: f32 = 28.0;
const PLAYER_CLIMB_BOOST_SPEED: f32 = 5.5;
const STEP_HEIGHT: f32 = 0.6;
const COLLISION_STEP: f32 = 0.2;
const CROSSHAIR_DISTANCE: f32 = 0.6;
const CROSSHAIR_LENGTH: f32 = 0.035;
const CROSSHAIR_THICKNESS: f32 = 0.004;
const TARGET_OUTLINE_THICKNESS: f32 = 0.035;
const WILD_PET_TARGET_OUTLINE_THICKNESS: f32 = 0.045;
const WILD_PET_TARGET_OUTLINE_PADDING: f32 = 0.06;
const WORLD_WEAPON_TARGET_OUTLINE_THICKNESS: f32 = 0.04;
const WORLD_WEAPON_TARGET_OUTLINE_PADDING: f32 = 0.05;
const THIRD_PERSON_ENTRY_ZOOM: f32 = 3.25;
const THIRD_PERSON_SCROLL_STEP: f32 = 0.75;
const THIRD_PERSON_MAX_ZOOM: f32 = 6.0;
const THIRD_PERSON_MIN_ACTIVE_ZOOM: f32 = 0.6;
const THIRD_PERSON_ZOOM_SPEED: f32 = 16.0;
const THIRD_PERSON_CAMERA_COLLISION_STEP: f32 = 0.1;
const THIRD_PERSON_CAMERA_CLEARANCE: f32 = 0.15;
const THIRD_PERSON_SHOULDER_RIGHT_OFFSET: f32 = 0.45;
const THIRD_PERSON_SHOULDER_UP_OFFSET: f32 = 0.2;
const LOCAL_AVATAR_TURN_SPEED: f32 = 10.0;
const LOCAL_AVATAR_PLACEHOLDER_TINT: [f32; 3] = [0.82, 0.73, 0.44];
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
const PET_MODEL_DESIRED_HEIGHT: f32 = 1.2;
const WEAPON_MODEL_DESIRED_SIZE: f32 = 1.15;
const PET_PARTY_THUMBNAIL_SIZE: u32 = 88;
const PET_PARTY_THUMBNAIL_SOURCE_DEFAULT: &str = "__default-pet-thumbnail__";
const PET_MODEL_RENDER_DISTANCE: f32 = CHUNK_WIDTH as f32 * 2.75;
const PET_IMPOSTOR_RENDER_DISTANCE: f32 = CHUNK_WIDTH as f32 * 6.0;
const HOSTED_WILD_PET_FULL_SIM_DISTANCE: f32 = CHUNK_WIDTH as f32 * 2.5;
const HOSTED_WILD_PET_REDUCED_UPDATE_STRIDE: u64 = 4;
const PET_IMPOSTOR_HALF_WIDTH: f32 = 0.24;
const PET_IMPOSTOR_HALF_HEIGHT: f32 = 0.5;
const PET_IMPOSTOR_TILE: (u32, u32) = (2, 0);
const PET_FOLLOWER_COUNT: usize = 6;
const PET_PARTY_DUPLICATE_WEAPON_MESSAGE: &str =
    "Each equipped weapon can only be assigned to one pet.";
const PET_FOLLOW_SPEED: f32 = 5.8;
const PET_FOLLOW_ACCELERATION: f32 = 18.0;
const PET_AIR_ACCELERATION: f32 = 8.0;
const PET_GRAVITY: f32 = 16.0;
const PET_STOP_DISTANCE: f32 = 0.45;
const PET_SLOW_RADIUS: f32 = 1.0;
const PET_TELEPORT_DISTANCE: f32 = 10.0;
const PET_STUCK_PROGRESS_EPSILON: f32 = 0.05;
const PET_STUCK_DISTANCE: f32 = 1.5;
const PET_STUCK_TIMEOUT_SECS: f32 = 1.25;
const PET_CLIMB_BOOST_SPEED: f32 = 5.0;
const PET_FALL_RESET_Y: f32 = -8.0;
const WILD_PET_ROAM_SPEED: f32 = 3.4;
const WILD_PET_ROAM_ACCELERATION: f32 = 12.0;
const WILD_PET_AIR_ACCELERATION: f32 = 6.0;
const WILD_PET_IDLE_MIN_SECS: f32 = 0.8;
const WILD_PET_IDLE_MAX_SECS: f32 = 1.2;
const WILD_PET_TARGET_REACHED_DISTANCE: f32 = 1.55;
const WILD_PET_SLOW_RADIUS: f32 = 1.4;
const WILD_PET_MIN_WANDER_DISTANCE: f32 = 3.0;
const WILD_PET_MAX_WANDER_DISTANCE: f32 = 10.0;
const WILD_PET_CAPTURE_TARGET_DISTANCE: f32 = 7.5;
const WILD_PET_CAPTURE_BOX_RADIUS: f32 = 0.95;
const WILD_PET_CAPTURE_BOX_HEIGHT: f32 = 1.45;
const WILD_PET_CAPTURE_BOX_FOOT_PADDING: f32 = 0.2;
const DEFAULT_WILD_PET_MAX_HEALTH: u8 = 30;
const WORLD_WEAPON_PICKUP_TARGET_DISTANCE: f32 = 7.5;
const WORLD_WEAPON_PICKUP_BOX_RADIUS: f32 = 0.75;
const WORLD_WEAPON_PICKUP_BOX_HEIGHT: f32 = 0.95;
const WORLD_WEAPON_PICKUP_BOX_FOOT_PADDING: f32 = 0.18;
const PET_WEAPON_LASER_LIFETIME: Duration = Duration::from_millis(150);
const PET_WEAPON_GUN_LIFETIME: Duration = Duration::from_millis(220);
const PET_WEAPON_FLAME_LIFETIME: Duration = Duration::from_millis(260);
const PET_WEAPON_SWORD_LIFETIME: Duration = Duration::from_millis(520);
const PET_ATTACK_REACTION_DURATION: Duration = Duration::from_millis(220);
const PET_ATTACK_REACTION_MATCH_RADIUS: f32 = 0.8;
const PET_ATTACK_REACTION_ORIGIN_HEIGHT: f32 = 1.55;
const PET_ATTACK_REACTION_FORWARD_OFFSET: f32 = 0.45;
const PET_WEAPON_SHOT_HALF_WIDTH: f32 = 0.03;
const PET_WEAPON_SHOT_HALF_DEPTH: f32 = 0.02;
const PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT: f32 = 0.055;
const PET_WEAPON_GUN_TRAIL_HALF_LENGTH: f32 = 0.18;
const PET_WEAPON_FLAME_BURST_RANGE: f32 = 1.85;
const PET_WEAPON_SWORD_HALF_LENGTH: f32 = 0.34;
const PET_WEAPON_SWORD_HALF_WIDTH: f32 = 0.06;
const PET_WEAPON_SWORD_HALF_DEPTH: f32 = 0.018;
const PET_SLOT_OFFSETS: [(f32, f32); PET_FOLLOWER_COUNT] = [
    (-1.0, 3.3),
    (1.0, 3.3),
    (-2.0, 3.1),
    (2.0, 3.1),
    (-0.6, 3.9),
    (0.6, 3.9),
];
const INPUT_BROADCAST_INTERVAL: Duration = Duration::from_millis(67);
const PEER_REALTIME_BROADCAST_INTERVAL: Duration = Duration::from_millis(33);
const PEER_REALTIME_RADIUS: f32 = CHUNK_WIDTH as f32 * WEB_RADIUS as f32;
const REMOTE_AVATAR_RUN_SPEED_THRESHOLD: f32 = 0.15;
const REMOTE_AVATAR_IDLE_DELAY_SECS: f32 = 0.35;
const REMOTE_AVATAR_DANCE_DELAY_SECS: f32 = 5.0;
const AUTH_STATUS_CHECKING: &str = "Checking your sign-in session...";

#[derive(Clone, Debug)]
struct AuthUser {
    id: String,
    name: Option<String>,
    email: Option<String>,
    avatar_selection: Option<PlayerAvatarSelection>,
    game_auth_token: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PlayerAvatarSelection {
    idle_model_url: Option<String>,
    run_model_url: Option<String>,
    dance_model_url: Option<String>,
}

impl PlayerAvatarSelection {
    fn idle_url(&self) -> Option<&str> {
        self.idle_model_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }

    fn run_url(&self) -> Option<&str> {
        self.run_model_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }

    fn dance_url(&self) -> Option<&str> {
        self.dance_model_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }

    fn first_available_url(&self) -> Option<&str> {
        self.idle_url()
            .or_else(|| self.run_url())
            .or_else(|| self.dance_url())
    }

    fn url_for_animation(&self, animation: RemoteAvatarAnimation) -> Option<&str> {
        match animation {
            RemoteAvatarAnimation::Idle => self
                .idle_url()
                .or_else(|| self.run_url())
                .or_else(|| self.dance_url()),
            RemoteAvatarAnimation::Run => self
                .run_url()
                .or_else(|| self.idle_url())
                .or_else(|| self.dance_url()),
            RemoteAvatarAnimation::Dance => self
                .dance_url()
                .or_else(|| self.idle_url())
                .or_else(|| self.run_url()),
        }
    }
}

impl AuthUser {
    fn guest() -> Self {
        let guest_id = format!("guest-{}", js_sys::Math::random().to_bits());
        Self {
            id: guest_id.clone(),
            name: Some(format!("Guest {}", &guest_id[6..guest_id.len().min(12)])),
            email: None,
            avatar_selection: None,
            game_auth_token: None,
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

#[derive(Clone, Debug)]
struct AvatarGenerationTaskClient {
    id: String,
    status: String,
    phase: String,
    progress_percent: i32,
    message: String,
    selfie_url: Option<String>,
    portrait_url: Option<String>,
    failure_reason: Option<String>,
    avatar_selection: Option<PlayerAvatarSelection>,
}

#[derive(Default)]
struct AvatarGenerationModalState {
    selfie_preview_url: Option<String>,
    selfie_object_url: Option<String>,
    selected_selfie_file: Option<web_sys::File>,
    active_task_id: Option<String>,
    polling: bool,
}

enum RemoteAvatarEvent {
    Loaded { url: String, bytes: Vec<u8> },
    Failed { url: String, message: String },
}

enum RemotePetEvent {
    Loaded { url: String, bytes: Vec<u8> },
    Failed { url: String, message: String },
}

enum RemoteWeaponEvent {
    Loaded { url: String, bytes: Vec<u8> },
    Failed { url: String, message: String },
}

struct PetPartyThumbnailRendererState {
    canvas: HtmlCanvasElement,
    renderer: Renderer<'static>,
    meshes: HashMap<String, TexturedMesh>,
    image_urls: HashMap<String, String>,
    failed_keys: HashSet<String>,
}

enum PeerRealtimeEvent {
    Opened { player_id: u64 },
    Closed { player_id: u64 },
    Message { player_id: u64, bytes: Vec<u8> },
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RemoteAvatarAnimation {
    Idle,
    Run,
    Dance,
}

#[derive(Clone, Debug)]
struct RemoteAvatarPlaybackState {
    animation: RemoteAvatarAnimation,
    playback_time: f32,
    time_since_motion: f32,
    active_url: Option<String>,
}

impl Default for RemoteAvatarPlaybackState {
    fn default() -> Self {
        Self {
            animation: RemoteAvatarAnimation::Idle,
            playback_time: 0.0,
            time_since_motion: 0.0,
            active_url: None,
        }
    }
}

#[derive(Clone, Debug)]
struct LocalAvatarPlaybackState {
    animation: RemoteAvatarAnimation,
    playback_time: f32,
    active_url: Option<String>,
}

impl Default for LocalAvatarPlaybackState {
    fn default() -> Self {
        Self {
            animation: RemoteAvatarAnimation::Idle,
            playback_time: 0.0,
            active_url: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct ThirdPersonCameraState {
    current_zoom: f32,
    target_zoom: f32,
    shoulder_offset: Vec3,
    render_eye: Vec3,
}

impl Default for ThirdPersonCameraState {
    fn default() -> Self {
        Self {
            current_zoom: 0.0,
            target_zoom: 0.0,
            shoulder_offset: Vec3::new(
                THIRD_PERSON_SHOULDER_RIGHT_OFFSET,
                THIRD_PERSON_SHOULDER_UP_OFFSET,
                0.0,
            ),
            render_eye: Vec3::ZERO,
        }
    }
}

thread_local! {
    static AUTH_GUEST_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static AUTH_REFRESH_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static CHUNK_QUALITY_CYCLE_QUEUE: RefCell<u8> = const { RefCell::new(0) };
    static PERF_PANEL_TOGGLE_QUEUE: RefCell<u8> = const { RefCell::new(0) };
    static PET_PARTY_MODAL_OPEN_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static PET_PARTY_MODAL_CLOSE_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static PET_PARTY_SAVE_QUEUE: RefCell<bool> = const { RefCell::new(false) };
    static PET_PARTY_TOGGLE_QUEUE: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static PET_PARTY_WEAPON_ASSIGN_QUEUE: RefCell<Vec<(String, Option<String>)>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChunkQualityPreset {
    Low,
    Balanced,
    High,
}

impl ChunkQualityPreset {
    fn chunk_radius(self) -> i32 {
        match self {
            Self::Low => 3,
            Self::Balanced => 5,
            Self::High => WEB_RADIUS,
        }
    }

    fn draw_distance_chunks(self) -> f32 {
        match self {
            Self::Low => 3.5,
            Self::Balanced => 5.0,
            Self::High => DRAW_DISTANCE_CHUNKS,
        }
    }

    fn max_visible_chunk_meshes(self) -> usize {
        match self {
            Self::Low => 18,
            Self::Balanced => 40,
            Self::High => MAX_VISIBLE_CHUNK_MESHES,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Balanced => "Balanced",
            Self::High => "High",
        }
    }

    fn storage_value(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Balanced => "balanced",
            Self::High => "high",
        }
    }

    fn from_storage_value(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "balanced" => Some(Self::Balanced),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    fn cycle(self) -> Self {
        match self {
            Self::Low => Self::Balanced,
            Self::Balanced => Self::High,
            Self::High => Self::Low,
        }
    }

    fn step(self, delta: i8) -> Self {
        if delta >= 0 {
            self.cycle()
        } else {
            match self {
                Self::Low => Self::High,
                Self::Balanced => Self::Low,
                Self::High => Self::Balanced,
            }
        }
    }
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
    let renderer = Renderer::new_with_size(
        window,
        scaled_render_size(initial_window_size, DEFAULT_WEB_RENDER_SCALE),
    )
    .await?;
    let pet_party_thumbnail_state = match create_pet_party_thumbnail_renderer().await {
        Ok(state) => Some(state),
        Err(error) => {
            web_sys::console::warn_1(&JsValue::from_str(&format!(
                "pet party thumbnails unavailable: {error:?}"
            )));
            None
        }
    };
    let (mesh_result_rx, workers, worker_onmessage) = start_mesh_worker_pool(MESH_WORKER_COUNT)?;
    let (network_rx, websocket, websocket_handlers) = start_websocket_client()?;
    let (webcam_tx, webcam_rx) = mpsc::channel();
    let mut app = WebApp::new(
        initial_window_size,
        canvas,
        pet_party_thumbnail_state,
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
        target.set_control_flow(ControlFlow::Wait);

        match event {
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => target.exit(),
                WindowEvent::Resized(size) => {
                    app.resize(size);
                    renderer.resize(app.scaled_render_size());
                }
                WindowEvent::KeyboardInput { event, .. } => app.handle_key(event),
                WindowEvent::MouseWheel { delta, .. } => app.handle_mouse_wheel(delta),
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button,
                    ..
                } => {
                    app.handle_mouse_button(button);
                }
                WindowEvent::RedrawRequested => {
                    app.tick();
                    if let Some(render_size) = app.maybe_adjust_render_scale() {
                        renderer.resize(render_size);
                    }
                    app.process_webcam_events();
                    app.process_remote_avatar_events(&renderer);
                    app.process_remote_pet_events(&renderer);
                    app.process_remote_weapon_events(&renderer);
                    app.process_pet_party_thumbnail_generation();
                    app.process_generation_updates(
                        &renderer,
                        &mut chunk_meshes,
                        DEFAULT_GENERATION_BUDGET_PER_UPDATE,
                    );
                    renderer.update_camera(app.camera_matrix());
                    let visible_meshes = app.collect_visible_chunk_meshes(&chunk_meshes);
                    app.update_perf_panel(visible_meshes.len(), chunk_meshes.len());
                    app.update_wild_pet_health_overlay();
                    let link_panel_mesh = app.build_link_panel_mesh(&renderer);
                    let mut visible_mesh_refs = visible_meshes;
                    if let Some(mesh) = &link_panel_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    let remote_players_mesh = app.build_remote_players_mesh(&renderer);
                    if let Some(mesh) = &remote_players_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    let remote_media_placeholder_mesh =
                        app.build_remote_media_placeholder_mesh(&renderer);
                    if let Some(mesh) = &remote_media_placeholder_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    let local_avatar_placeholder_mesh =
                        app.build_local_avatar_placeholder_mesh(&renderer);
                    if let Some(mesh) = &local_avatar_placeholder_mesh {
                        visible_mesh_refs.push(mesh);
                    }
                    app.prepare_pet_assets(&renderer);
                    app.prepare_weapon_assets(&renderer);
                    app.refresh_pet_impostor_mesh(&renderer);
                    app.update_remote_media_textures(&renderer);
                    let textured_meshes = app.build_remote_media_meshes(&renderer);
                    let mut overlay_meshes = Vec::new();
                    if let Some(mesh) = app.build_crosshair_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    if let Some(mesh) = app.build_target_highlight_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    if let Some(mesh) = app.build_pet_weapon_shot_mesh(&renderer) {
                        overlay_meshes.push(mesh);
                    }
                    let textured_mesh_refs = textured_meshes.iter().collect::<Vec<_>>();
                    let mut pet_mesh_draws = app.build_pet_mesh_draws(&renderer);
                    pet_mesh_draws.extend(app.build_weapon_mesh_draws(&renderer));
                    let mut remote_avatar_meshes = app.build_remote_avatar_meshes(&renderer);
                    if let Some(draw) = app.build_local_avatar_mesh_draw(&renderer) {
                        remote_avatar_meshes.push(draw);
                    }
                    let overlay_refs = overlay_meshes.iter().collect::<Vec<_>>();
                    if let Some(mesh) = app.pet_impostor_mesh.as_ref() {
                        visible_mesh_refs.push(mesh);
                    }

                    if let Err(error) = renderer.render(
                        &visible_mesh_refs,
                        &textured_mesh_refs,
                        &pet_mesh_draws,
                        &remote_avatar_meshes,
                        &overlay_refs,
                    ) {
                        panic!("{error:?}");
                    }
                }
                _ => {}
            },
            Event::DeviceEvent {
                event: DeviceEvent::MouseMotion { delta },
                ..
            } => {
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
    third_person: ThirdPersonCameraState,
    avatar_yaw: f32,
    local_avatar_state: LocalAvatarPlaybackState,
    authoritative_chunks: HashMap<ChunkPos, ChunkData>,
    collision_voxels: HashMap<ChunkPos, Vec<u16>>,
    pressed: HashSet<KeyCode>,
    last_tick: Instant,
    size: PhysicalSize<u32>,
    render_scale: f32,
    chunk_quality_preset: ChunkQualityPreset,
    applied_chunk_radius: i32,
    smoothed_frame_time_secs: f32,
    startup_at: Instant,
    last_render_scale_change: Instant,
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
    remote_player_latest_ticks: HashMap<u64, u64>,
    remote_player_velocities: HashMap<u64, [f32; 3]>,
    remote_player_yaws: HashMap<u64, f32>,
    remote_player_avatar_selections: HashMap<u64, PlayerAvatarSelection>,
    remote_player_avatar_states: HashMap<u64, RemoteAvatarPlaybackState>,
    remote_player_pet_identities: HashMap<u64, Vec<PetIdentity>>,
    remote_pet_states: HashMap<u64, Vec<PetStateSnapshot>>,
    wild_pets: HashMap<u64, WildPetClientState>,
    hosted_wild_pets: HashMap<u64, HostedWildPetState>,
    world_weapons: HashMap<u64, WorldWeaponClientState>,
    pet_weapon_shots: Vec<PetWeaponShotClientState>,
    remote_media: HashMap<u64, RemotePeerMedia>,
    remote_avatar_assets: HashMap<String, RemoteAvatarAsset>,
    pending_remote_avatar_urls: HashSet<String>,
    remote_pet_assets: HashMap<String, TexturedMesh>,
    remote_pet_model_bytes: HashMap<String, Vec<u8>>,
    pending_remote_pet_urls: HashSet<String>,
    remote_weapon_assets: HashMap<String, TexturedMesh>,
    pending_remote_weapon_urls: HashSet<String>,
    pet_asset: Option<TexturedMesh>,
    pet_asset_attempted: bool,
    pet_impostor_mesh: Option<Mesh>,
    last_pet_impostor_update: Instant,
    pet_followers: Vec<PetFollowerState>,
    pet_followers_need_reset: bool,
    captured_pets: Vec<CapturedPet>,
    collected_weapons: Vec<CollectedWeapon>,
    pet_notice: Option<String>,
    weapon_notice: Option<String>,
    pending_pet_notice_after_login_snapshot: Option<String>,
    pending_weapon_notice_after_login_snapshot: Option<String>,
    pet_party_selected_ids: HashSet<String>,
    pet_party_equipped_weapon_ids: HashMap<String, Option<String>>,
    pet_party_save_in_flight: bool,
    pet_party_modal_message: Option<String>,
    pet_party_thumbnail_state: Option<PetPartyThumbnailRendererState>,
    spawn_position: Option<WorldPos>,
    world_seed: u64,
    webcam_requested: bool,
    webcam_tx: Sender<WebcamEvent>,
    webcam_rx: Receiver<WebcamEvent>,
    peer_realtime_tx: Sender<PeerRealtimeEvent>,
    peer_realtime_rx: Receiver<PeerRealtimeEvent>,
    webcam: Option<WebcamCapture>,
    last_sent_position: Option<[f32; 3]>,
    last_sent_velocity: Option<[f32; 3]>,
    last_sent_yaw: Option<f32>,
    last_input_broadcast_at: Option<Instant>,
    last_peer_realtime_broadcast_at: Option<Instant>,
    tick_counter: u64,
    simulation_frame_index: u64,
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
    auth_tx: Sender<AuthEvent>,
    auth_rx: Receiver<AuthEvent>,
    remote_avatar_tx: Sender<RemoteAvatarEvent>,
    remote_avatar_rx: Receiver<RemoteAvatarEvent>,
    remote_pet_tx: Sender<RemotePetEvent>,
    remote_pet_rx: Receiver<RemotePetEvent>,
    remote_weapon_tx: Sender<RemoteWeaponEvent>,
    remote_weapon_rx: Receiver<RemoteWeaponEvent>,
    auth_overlay: Element,
    auth_overlay_status: Element,
    perf_panel_toggle_button: Element,
    perf_panel: Element,
    perf_panel_visible: bool,
    chunk_quality_button: Element,
    captured_pets_panel: Element,
    weapons_panel: Element,
    wild_pet_health_overlay: Element,
    pet_party_modal: Element,
    pet_party_modal_copy: Element,
    pet_party_modal_count: Element,
    pet_party_modal_list: Element,
    pet_party_modal_status: Element,
    pet_party_save_button: Element,
    player_avatar_panel: Element,
    player_avatar_modal: Element,
    player_avatar_panel_status: Element,
    logout_button: Element,
    server_ready_for_login: bool,
    login_request_sent: bool,
    _auth_button_onclicks: Vec<Closure<dyn FnMut(WebEvent)>>,
    _player_avatar_panel_onclick: Closure<dyn FnMut(WebEvent)>,
    _logout_button_onclick: Closure<dyn FnMut(WebEvent)>,
    mesh_result_rx: Receiver<MeshBuildResult>,
    workers: Vec<Worker>,
    next_worker_index: usize,
    last_perf_panel_update: Instant,
    _perf_panel_toggle_onclick: Closure<dyn FnMut(WebEvent)>,
    _worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
    _chunk_quality_button_onclick: Closure<dyn FnMut(WebEvent)>,
}

impl WebApp {
    fn new(
        size: PhysicalSize<u32>,
        canvas: HtmlCanvasElement,
        pet_party_thumbnail_state: Option<PetPartyThumbnailRendererState>,
        workers: Vec<Worker>,
        worker_onmessages: Vec<Closure<dyn FnMut(MessageEvent)>>,
        mesh_result_rx: Receiver<MeshBuildResult>,
        network_rx: Receiver<NetworkEvent>,
        websocket: WebSocket,
        websocket_bindings: WebSocketBindings,
        webcam_tx: Sender<WebcamEvent>,
        webcam_rx: Receiver<WebcamEvent>,
    ) -> Self {
        let now = Instant::now();
        let chunk_quality_preset = load_chunk_quality_preset();
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
        let (auth_tx, auth_rx) = request_auth_session();
        let (remote_avatar_tx, remote_avatar_rx) = mpsc::channel();
        let (remote_pet_tx, remote_pet_rx) = mpsc::channel();
        let (remote_weapon_tx, remote_weapon_rx) = mpsc::channel();
        let (peer_realtime_tx, peer_realtime_rx) = mpsc::channel();
        let (auth_overlay, auth_overlay_status, auth_button_onclicks) = create_auth_overlay();
        let (perf_panel_toggle_button, perf_panel_toggle_onclick) =
            create_perf_panel_toggle_button();
        let perf_panel = create_perf_panel();
        let (chunk_quality_button, chunk_quality_button_onclick) =
            create_chunk_quality_button(chunk_quality_preset);
        let captured_pets_panel = create_captured_pets_panel();
        let weapons_panel = create_weapon_collection_panel();
        let wild_pet_health_overlay = create_wild_pet_health_overlay();
        let (
            pet_party_modal,
            pet_party_modal_copy,
            pet_party_modal_count,
            pet_party_modal_list,
            pet_party_modal_status,
            pet_party_save_button,
        ) = create_pet_party_modal();
        let (
            player_avatar_panel,
            player_avatar_modal,
            player_avatar_panel_status,
            player_avatar_panel_onclick,
        ) = create_player_avatar_panel();
        let (logout_button, logout_button_onclick) = create_logout_button();
        update_hotbar_ui(&hotbar_slots, &hotbar_blocks, 0);
        let current_chunk = chunk_from_world_position(camera.position);
        let desired_chunks = HashSet::new();
        let pending_generation = VecDeque::new();
        let last_reprioritize_forward = camera.forward();
        let mut third_person = ThirdPersonCameraState::default();
        third_person.current_zoom = THIRD_PERSON_MAX_ZOOM;
        third_person.target_zoom = THIRD_PERSON_MAX_ZOOM;
        third_person.render_eye = camera.position;

        let mut app = Self {
            canvas,
            camera,
            third_person,
            avatar_yaw: 0.0,
            local_avatar_state: LocalAvatarPlaybackState::default(),
            authoritative_chunks: HashMap::new(),
            collision_voxels: HashMap::new(),
            pressed: HashSet::new(),
            last_tick: now,
            size,
            render_scale: DEFAULT_WEB_RENDER_SCALE,
            chunk_quality_preset,
            applied_chunk_radius: chunk_quality_preset.chunk_radius(),
            smoothed_frame_time_secs: 1.0 / 60.0,
            startup_at: now,
            last_render_scale_change: now,
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
            remote_player_latest_ticks: HashMap::new(),
            remote_player_velocities: HashMap::new(),
            remote_player_yaws: HashMap::new(),
            remote_player_avatar_selections: HashMap::new(),
            remote_player_avatar_states: HashMap::new(),
            remote_player_pet_identities: HashMap::new(),
            remote_pet_states: HashMap::new(),
            wild_pets: HashMap::new(),
            hosted_wild_pets: HashMap::new(),
            world_weapons: HashMap::new(),
            pet_weapon_shots: Vec::new(),
            remote_media: HashMap::new(),
            remote_avatar_assets: HashMap::new(),
            pending_remote_avatar_urls: HashSet::new(),
            remote_pet_assets: HashMap::new(),
            remote_pet_model_bytes: HashMap::new(),
            pending_remote_pet_urls: HashSet::new(),
            remote_weapon_assets: HashMap::new(),
            pending_remote_weapon_urls: HashSet::new(),
            pet_asset: None,
            pet_asset_attempted: false,
            pet_impostor_mesh: None,
            last_pet_impostor_update: now,
            pet_followers: Vec::new(),
            pet_followers_need_reset: false,
            captured_pets: Vec::new(),
            collected_weapons: Vec::new(),
            pet_notice: None,
            weapon_notice: None,
            pending_pet_notice_after_login_snapshot: None,
            pending_weapon_notice_after_login_snapshot: None,
            pet_party_selected_ids: HashSet::new(),
            pet_party_equipped_weapon_ids: HashMap::new(),
            pet_party_save_in_flight: false,
            pet_party_modal_message: None,
            pet_party_thumbnail_state,
            spawn_position: None,
            world_seed: DEFAULT_WORLD_SEED,
            webcam_requested: false,
            webcam_tx,
            webcam_rx,
            peer_realtime_tx,
            peer_realtime_rx,
            webcam: None,
            last_sent_position: None,
            last_sent_velocity: None,
            last_sent_yaw: None,
            last_input_broadcast_at: None,
            last_peer_realtime_broadcast_at: None,
            tick_counter: 0,
            simulation_frame_index: 0,
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
            auth_tx,
            auth_rx,
            remote_avatar_tx,
            remote_avatar_rx,
            remote_pet_tx,
            remote_pet_rx,
            remote_weapon_tx,
            remote_weapon_rx,
            auth_overlay,
            auth_overlay_status,
            perf_panel_toggle_button,
            perf_panel,
            perf_panel_visible: false,
            chunk_quality_button,
            captured_pets_panel,
            weapons_panel,
            wild_pet_health_overlay,
            pet_party_modal,
            pet_party_modal_copy,
            pet_party_modal_count,
            pet_party_modal_list,
            pet_party_modal_status,
            pet_party_save_button,
            player_avatar_panel,
            player_avatar_modal,
            player_avatar_panel_status,
            logout_button,
            server_ready_for_login: false,
            login_request_sent: false,
            _auth_button_onclicks: auth_button_onclicks,
            _player_avatar_panel_onclick: player_avatar_panel_onclick,
            _logout_button_onclick: logout_button_onclick,
            mesh_result_rx,
            workers,
            next_worker_index: 0,
            last_perf_panel_update: now,
            _perf_panel_toggle_onclick: perf_panel_toggle_onclick,
            _worker_onmessages: worker_onmessages,
            _chunk_quality_button_onclick: chunk_quality_button_onclick,
        };
        app.avatar_yaw = app.camera.yaw;
        app.update_chunk_quality_button();
        app.update_captured_pets_panel();
        app.update_weapon_collection_panel();
        app.update_pet_party_modal();
        app.update_perf_panel(0, 0);
        app.sync_perf_panel_visibility();
        app
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.size = size;
    }

    fn scaled_render_size(&self) -> PhysicalSize<u32> {
        scaled_render_size(self.size, self.render_scale)
    }

    fn maybe_adjust_render_scale(&mut self) -> Option<PhysicalSize<u32>> {
        let now = Instant::now();
        if now.duration_since(self.startup_at) < RENDER_SCALE_WARMUP
            || now.duration_since(self.last_render_scale_change) < RENDER_SCALE_ADJUST_COOLDOWN
        {
            return None;
        }

        let next_scale = if self.smoothed_frame_time_secs > RENDER_SCALE_SLOW_FRAME_SECS
            && self.render_scale > MIN_WEB_RENDER_SCALE
        {
            (self.render_scale - WEB_RENDER_SCALE_STEP).max(MIN_WEB_RENDER_SCALE)
        } else if self.smoothed_frame_time_secs < RENDER_SCALE_FAST_FRAME_SECS
            && self.render_scale < DEFAULT_WEB_RENDER_SCALE
            && !self.movement_active
            && self.pending_generation.is_empty()
            && self.inflight_generation.is_empty()
            && self.completed_meshes.is_empty()
            && self.pending_network_events.is_empty()
        {
            (self.render_scale + WEB_RENDER_SCALE_STEP).min(DEFAULT_WEB_RENDER_SCALE)
        } else {
            return None;
        };

        if (next_scale - self.render_scale).abs() <= f32::EPSILON {
            return None;
        }

        self.render_scale = next_scale;
        self.last_render_scale_change = now;
        Some(self.scaled_render_size())
    }

    fn record_frame_time(&mut self, dt_secs: f32) {
        let dt_secs = dt_secs.clamp(1.0 / 240.0, 0.25);
        self.smoothed_frame_time_secs +=
            (dt_secs - self.smoothed_frame_time_secs) * RENDER_SCALE_SMOOTHING;
    }

    fn chunk_quality_button_text(&self) -> String {
        format!("Chunk Quality: {}", self.chunk_quality_preset.label())
    }

    fn update_chunk_quality_button(&self) {
        self.chunk_quality_button
            .set_text_content(Some(&self.chunk_quality_button_text()));
    }

    fn set_chunk_quality_preset(&mut self, preset: ChunkQualityPreset) {
        if self.chunk_quality_preset == preset {
            return;
        }

        self.chunk_quality_preset = preset;
        save_chunk_quality_preset(preset);
        self.update_chunk_quality_button();
        self.last_perf_panel_update = Instant::now()
            .checked_sub(PERF_PANEL_UPDATE_INTERVAL)
            .unwrap_or_else(Instant::now);
    }

    fn step_chunk_quality_preset(&mut self, delta: i8) {
        self.set_chunk_quality_preset(self.chunk_quality_preset.step(delta));
    }

    fn process_chunk_quality_requests(&mut self) {
        let cycle_count = CHUNK_QUALITY_CYCLE_QUEUE.with(|queue| {
            let mut value = queue.borrow_mut();
            let count = *value;
            *value = 0;
            count
        });
        for _ in 0..cycle_count {
            self.step_chunk_quality_preset(1);
        }
    }

    fn process_perf_panel_toggle_requests(&mut self) {
        let toggle_count = PERF_PANEL_TOGGLE_QUEUE.with(|queue| {
            let mut value = queue.borrow_mut();
            let count = *value;
            *value = 0;
            count
        });
        if toggle_count % 2 == 1 {
            self.perf_panel_visible = !self.perf_panel_visible;
            self.sync_perf_panel_visibility();
        }
    }

    fn update_perf_panel(&mut self, visible_chunks: usize, loaded_chunks: usize) {
        let now = Instant::now();
        if now.duration_since(self.last_perf_panel_update) < PERF_PANEL_UPDATE_INTERVAL {
            return;
        }
        self.last_perf_panel_update = now;

        let frame_ms = self.smoothed_frame_time_secs * 1000.0;
        let fps = if self.smoothed_frame_time_secs > 0.0 {
            1.0 / self.smoothed_frame_time_secs
        } else {
            0.0
        };
        let remote_video_peers = self
            .remote_media
            .values()
            .filter(|media| media.video.is_some())
            .count();

        self.perf_panel.set_text_content(Some(&format!(
            "Perf\n{fps:.0} fps | {frame_ms:.1} ms\nrender scale {render_scale:.2}\nchunk quality {chunk_quality}\nchunks {visible_chunks}/{loaded_chunks}\nmesh {pending}/{inflight}/{completed}\npets wild {wild_pets} hosted {hosted_pets}\nremote players {remote_players}\nvideo peers {remote_video_peers}\nnet queue {net_queue}",
            render_scale = self.render_scale,
            chunk_quality = self.chunk_quality_preset.label(),
            pending = self.pending_generation.len(),
            inflight = self.inflight_generation.len(),
            completed = self.completed_meshes.len(),
            wild_pets = self.wild_pets.len(),
            hosted_pets = self.hosted_wild_pets.len(),
            remote_players = self.remote_players.len(),
            net_queue = self.pending_network_events.len(),
        )));
    }

    fn sync_perf_panel_visibility(&self) {
        let _ = self
            .perf_panel
            .set_attribute("style", perf_panel_style(self.perf_panel_visible));
        let label = if self.perf_panel_visible {
            "Hide performance stats"
        } else {
            "Show performance stats"
        };
        let _ = self.perf_panel_toggle_button.set_attribute(
            "style",
            perf_panel_toggle_button_style(self.perf_panel_visible),
        );
        let _ = self.perf_panel_toggle_button.set_attribute("title", label);
        let _ = self
            .perf_panel_toggle_button
            .set_attribute("aria-label", label);
        let _ = self.perf_panel_toggle_button.set_attribute(
            "aria-pressed",
            if self.perf_panel_visible {
                "true"
            } else {
                "false"
            },
        );
    }

    fn sync_pointer_lock_state(&mut self) {
        self.mouse_captured = pointer_is_locked(&self.canvas);
        sync_mouse_lock_prompt(
            &self.mouse_lock_prompt,
            self.spawn_settled,
            self.mouse_captured,
        );
    }

    fn is_third_person_active(&self) -> bool {
        self.third_person.target_zoom >= THIRD_PERSON_MIN_ACTIVE_ZOOM
    }

    fn should_render_local_avatar(&self) -> bool {
        self.is_third_person_active()
            || self.third_person.current_zoom >= THIRD_PERSON_MIN_ACTIVE_ZOOM
    }

    fn interaction_ray(&self) -> Option<(Vec3, Vec3)> {
        let direction = self.camera.forward().normalize_or_zero();
        if direction == Vec3::ZERO {
            None
        } else {
            Some((self.render_camera_eye(), direction))
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
                KeyCode::BracketLeft | KeyCode::Minus => self.step_chunk_quality_preset(-1),
                KeyCode::BracketRight | KeyCode::Equal => self.step_chunk_quality_preset(1),
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
            update_hotbar_ui(
                &self.hotbar_slots,
                &self.hotbar_blocks,
                self.selected_hotbar,
            );
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

    fn handle_mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let normalized = normalize_scroll_delta(delta);
        if normalized.abs() <= f32::EPSILON {
            return;
        }

        if normalized < 0.0 && self.third_person.target_zoom < THIRD_PERSON_MIN_ACTIVE_ZOOM {
            self.third_person.target_zoom = THIRD_PERSON_ENTRY_ZOOM;
            return;
        }

        let next_zoom = (self.third_person.target_zoom - normalized * THIRD_PERSON_SCROLL_STEP)
            .clamp(0.0, THIRD_PERSON_MAX_ZOOM);
        if next_zoom < THIRD_PERSON_MIN_ACTIVE_ZOOM {
            self.third_person.current_zoom = 0.0;
            self.third_person.target_zoom = 0.0;
            self.third_person.render_eye = self.camera.position;
        } else {
            self.third_person.target_zoom = next_zoom;
        }
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

        if button == MouseButton::Left {
            if self.current_link_target().is_some() {
                if confirm_open_url(LINK_PANEL_LABEL_URL) {
                    open_url(LINK_PANEL_OPEN_URL);
                }
                return;
            }
        }

        let Some(target) = self.current_interaction_target() else {
            return;
        };

        match target {
            InteractionTarget::WildPet(hit) if button == MouseButton::Left => {
                self.send_client_message(&ClientMessage::StartPetCombatRequest {
                    pet_id: hit.pet_id,
                });
            }
            InteractionTarget::WorldWeapon(hit) if button == MouseButton::Left => {
                if !self.can_collect_generated_weapons() {
                    self.set_weapon_notice(
                        "Sign in or continue as a guest to collect generated weapons.",
                    );
                    return;
                }
                self.send_client_message(&ClientMessage::PickupWorldWeaponRequest {
                    weapon_id: hit.weapon_id,
                });
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
                    if self.player_collides_with_world_pos(
                        self.camera.position,
                        place_at,
                        selected_block,
                    ) {
                        return;
                    }

                    self.apply_local_block_edit(place_at, selected_block);
                }
                _ => {}
            },
            InteractionTarget::Link
            | InteractionTarget::WildPet(_)
            | InteractionTarget::WorldWeapon(_) => {}
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

    fn process_peer_realtime_events(&mut self) {
        REMOTE_DATA_CHANNEL_REGISTRY.with(|registry| {
            let mut registry = registry.borrow_mut();
            for (player_id, registration) in registry.drain() {
                if let Some(remote) = self.remote_media.get_mut(&player_id) {
                    remote.data_channel = Some(registration.channel);
                    remote.data_channel_bindings = Some(registration.bindings);
                }
            }
        });

        while let Ok(event) = self.peer_realtime_rx.try_recv() {
            match event {
                PeerRealtimeEvent::Opened { player_id } => {
                    if let Some(remote) = self.remote_media.get_mut(&player_id) {
                        remote.data_channel_open = true;
                    }
                    self.maybe_enable_peer_media(player_id);
                }
                PeerRealtimeEvent::Closed { player_id } => {
                    if let Some(remote) = self.remote_media.get_mut(&player_id) {
                        remote.data_channel_open = false;
                    }
                }
                PeerRealtimeEvent::Message { player_id, bytes } => {
                    let Ok(state) = decode::<PeerRealtimeState>(&bytes) else {
                        continue;
                    };
                    self.apply_remote_motion_state(
                        player_id,
                        state.tick,
                        state.position,
                        state.velocity,
                        state.yaw,
                        state.pet_states,
                    );
                    self.apply_remote_wild_pet_motion(player_id, state.tick, state.wild_pet_states);
                }
            }
        }
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

    fn process_remote_pet_events(&mut self, renderer: &Renderer<'_>) {
        while let Ok(event) = self.remote_pet_rx.try_recv() {
            match event {
                RemotePetEvent::Loaded { url, bytes } => {
                    self.pending_remote_pet_urls.remove(&url);
                    self.remote_pet_model_bytes
                        .insert(url.clone(), bytes.clone());
                    if let Some(state) = self.pet_party_thumbnail_state.as_mut() {
                        state.failed_keys.remove(&url);
                    }
                    match build_pet_mesh(renderer, &bytes) {
                        Ok(asset) => {
                            self.remote_pet_assets.insert(url, asset);
                        }
                        Err(error) => {
                            web_sys::console::warn_1(&JsValue::from_str(&format!(
                                "failed to build remote pet asset: {error}"
                            )));
                        }
                    }
                }
                RemotePetEvent::Failed { url, message } => {
                    self.pending_remote_pet_urls.remove(&url);
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "failed to fetch remote pet {url}: {message}"
                    )));
                }
            }
        }
    }

    fn process_remote_weapon_events(&mut self, renderer: &Renderer<'_>) {
        while let Ok(event) = self.remote_weapon_rx.try_recv() {
            match event {
                RemoteWeaponEvent::Loaded { url, bytes } => {
                    self.pending_remote_weapon_urls.remove(&url);
                    match build_weapon_mesh(renderer, &bytes) {
                        Ok(asset) => {
                            self.remote_weapon_assets.insert(url, asset);
                        }
                        Err(error) => {
                            web_sys::console::warn_1(&JsValue::from_str(&format!(
                                "failed to build remote weapon asset: {error}"
                            )));
                        }
                    }
                }
                RemoteWeaponEvent::Failed { url, message } => {
                    self.pending_remote_weapon_urls.remove(&url);
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "failed to fetch remote weapon {url}: {message}"
                    )));
                }
            }
        }
    }

    fn despawn_remote_player(&mut self, player_id: u64) {
        self.remote_players.remove(&player_id);
        self.remote_player_latest_ticks.remove(&player_id);
        self.remote_player_velocities.remove(&player_id);
        self.remote_player_yaws.remove(&player_id);
        self.remote_player_avatar_selections.remove(&player_id);
        self.remote_player_avatar_states.remove(&player_id);
        self.remote_player_pet_identities.remove(&player_id);
        self.remote_pet_states.remove(&player_id);

        if let Some(remote) = self.remote_media.remove(&player_id) {
            if let Some(channel) = remote.data_channel {
                channel.close();
            }
            remote.connection.close();
        }

        remove_peer_connection_registry_entry(player_id);

        for pet in self.wild_pets.values_mut() {
            if pet.host_player_id == Some(player_id) {
                pet.host_player_id = None;
            }
        }
    }

    fn active_captured_pet_identities(&self) -> Vec<PetIdentity> {
        self.captured_pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| PetIdentity {
                id: pet.id.clone(),
                display_name: pet.display_name.clone(),
                model_url: pet.model_url.clone(),
                equipped_weapon: pet
                    .equipped_weapon_id
                    .as_deref()
                    .and_then(|weapon_id| self.collected_weapon_identity(weapon_id)),
            })
            .collect()
    }

    fn collected_weapon_identity(&self, weapon_id: &str) -> Option<WeaponIdentity> {
        self.collected_weapons
            .iter()
            .find(|weapon| weapon.id == weapon_id)
            .map(|weapon| WeaponIdentity {
                id: weapon.id.clone(),
                kind: weapon.kind.clone(),
                display_name: weapon.display_name.clone(),
                model_url: weapon.model_url.clone(),
            })
    }

    fn guest_pet_captures_are_temporary(&self) -> bool {
        self.auth_user.as_ref().is_some_and(auth_user_is_guest)
    }

    fn can_capture_generated_pets(&self) -> bool {
        self.auth_user.is_some()
    }

    fn can_collect_generated_weapons(&self) -> bool {
        self.auth_user.is_some()
    }

    fn set_pet_notice(&mut self, message: impl Into<String>) {
        self.pet_notice = Some(message.into());
        self.update_captured_pets_panel();
    }

    fn set_weapon_notice(&mut self, message: impl Into<String>) {
        self.weapon_notice = Some(message.into());
        self.update_weapon_collection_panel();
    }

    fn clear_pending_login_notices(&mut self) {
        self.pending_pet_notice_after_login_snapshot = None;
        self.pending_weapon_notice_after_login_snapshot = None;
    }

    fn queue_login_notices(&mut self, message: String) {
        self.pending_pet_notice_after_login_snapshot = Some(message.clone());
        self.pending_weapon_notice_after_login_snapshot = Some(message);
    }

    fn apply_pending_pet_login_notice(&mut self) {
        if let Some(message) = self.pending_pet_notice_after_login_snapshot.take() {
            self.pet_notice = Some(message);
        }
    }

    fn apply_pending_weapon_login_notice(&mut self) {
        if let Some(message) = self.pending_weapon_notice_after_login_snapshot.take() {
            self.weapon_notice = Some(message);
        }
    }

    fn sync_pet_party_draft_from_snapshot(&mut self) {
        self.pet_party_selected_ids = self
            .captured_pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| pet.id.clone())
            .collect();
        self.pet_party_equipped_weapon_ids = self
            .captured_pets
            .iter()
            .map(|pet| (pet.id.clone(), pet.equipped_weapon_id.clone()))
            .collect();
    }

    fn current_pet_party_selection_ids(&self) -> Vec<String> {
        self.captured_pets
            .iter()
            .filter(|pet| self.pet_party_selected_ids.contains(&pet.id))
            .map(|pet| pet.id.clone())
            .collect()
    }

    fn current_pet_party_weapon_assignments(&self) -> Vec<PetWeaponAssignment> {
        self.captured_pets
            .iter()
            .filter(|pet| self.pet_party_selected_ids.contains(&pet.id))
            .map(|pet| PetWeaponAssignment {
                pet_id: pet.id.clone(),
                weapon_id: self
                    .pet_party_equipped_weapon_ids
                    .get(&pet.id)
                    .cloned()
                    .flatten(),
            })
            .collect()
    }

    fn pet_party_has_unsaved_changes(&self) -> bool {
        let saved_active_pet_ids = self
            .captured_pets
            .iter()
            .filter(|pet| pet.active)
            .map(|pet| pet.id.as_str())
            .collect::<HashSet<_>>();

        saved_active_pet_ids.len() != self.pet_party_selected_ids.len()
            || self
                .pet_party_selected_ids
                .iter()
                .any(|pet_id| !saved_active_pet_ids.contains(pet_id.as_str()))
            || self
                .captured_pets
                .iter()
                .filter(|pet| self.pet_party_selected_ids.contains(&pet.id))
                .any(|pet| {
                    let saved_weapon_id = pet.equipped_weapon_id.as_deref();
                    let draft_weapon_id = self
                        .pet_party_equipped_weapon_ids
                        .get(&pet.id)
                        .and_then(|weapon_id| weapon_id.as_deref());
                    saved_weapon_id != draft_weapon_id
                })
    }

    fn pet_party_weapon_is_assigned_to_other_selected_pet(
        &self,
        pet_id: &str,
        weapon_id: &str,
    ) -> bool {
        self.captured_pets
            .iter()
            .filter(|pet| pet.id != pet_id && self.pet_party_selected_ids.contains(&pet.id))
            .any(|pet| {
                self.pet_party_equipped_weapon_ids
                    .get(&pet.id)
                    .and_then(|assigned_weapon_id| assigned_weapon_id.as_deref())
                    == Some(weapon_id)
            })
    }

    fn pet_party_duplicate_weapon_message(&self) -> Option<&'static str> {
        self.captured_pets
            .iter()
            .filter(|pet| self.pet_party_selected_ids.contains(&pet.id))
            .filter_map(|pet| {
                self.pet_party_equipped_weapon_ids
                    .get(&pet.id)
                    .and_then(|weapon_id| weapon_id.as_deref())
                    .filter(|weapon_id| !weapon_id.trim().is_empty())
                    .map(|weapon_id| (pet.id.as_str(), weapon_id))
            })
            .find_map(|(pet_id, weapon_id)| {
                self.pet_party_weapon_is_assigned_to_other_selected_pet(pet_id, weapon_id)
                    .then_some(PET_PARTY_DUPLICATE_WEAPON_MESSAGE)
            })
    }

    fn show_pet_party_modal(&self) {
        let _ = self.pet_party_modal.set_attribute(
            "style",
            "position:fixed;inset:0;display:flex;align-items:center;justify-content:center;padding:20px;background:rgba(5,8,12,0.72);backdrop-filter:blur(10px);z-index:65;",
        );
    }

    fn hide_pet_party_modal(&self) {
        let _ = self
            .pet_party_modal
            .set_attribute("style", player_avatar_modal_style());
    }

    fn pet_party_thumbnail_source_key(pet: &CapturedPet) -> String {
        pet.model_url
            .clone()
            .unwrap_or_else(|| PET_PARTY_THUMBNAIL_SOURCE_DEFAULT.to_string())
    }

    fn pet_party_thumbnail_image_url_for_pet(&self, pet: &CapturedPet) -> Option<&str> {
        let key = Self::pet_party_thumbnail_source_key(pet);
        self.pet_party_thumbnail_state
            .as_ref()?
            .image_urls
            .get(&key)
            .map(String::as_str)
    }

    fn pet_party_thumbnail_bytes_for_key<'a>(&'a self, key: &str) -> Option<&'a [u8]> {
        if key == PET_PARTY_THUMBNAIL_SOURCE_DEFAULT {
            Some(PET_MODEL_BYTES)
        } else {
            self.remote_pet_model_bytes.get(key).map(Vec::as_slice)
        }
    }

    fn process_pet_party_thumbnail_generation(&mut self) {
        let Some(source_key) = self
            .captured_pets
            .iter()
            .map(Self::pet_party_thumbnail_source_key)
            .find(|key| {
                self.pet_party_thumbnail_state
                    .as_ref()
                    .is_some_and(|state| {
                        !state.image_urls.contains_key(key)
                            && !state.failed_keys.contains(key)
                            && self.pet_party_thumbnail_bytes_for_key(key).is_some()
                    })
            })
        else {
            return;
        };

        let Some(bytes) = self
            .pet_party_thumbnail_bytes_for_key(&source_key)
            .map(|bytes| bytes.to_vec())
        else {
            return;
        };

        let result = (|| -> Result<()> {
            let Some(state) = self.pet_party_thumbnail_state.as_mut() else {
                return Ok(());
            };
            if !state.meshes.contains_key(&source_key) {
                let mesh = build_pet_mesh(&state.renderer, &bytes)?;
                state.meshes.insert(source_key.clone(), mesh);
            }
            let mesh = state
                .meshes
                .get(&source_key)
                .ok_or_else(|| anyhow::anyhow!("thumbnail mesh missing"))?;

            state
                .renderer
                .update_camera(pet_party_thumbnail_camera_matrix());
            let draw = state
                .renderer
                .create_textured_draw(mesh, pet_party_thumbnail_model_matrix());
            state.renderer.render(&[], &[], &[draw], &[], &[])?;
            let image_url = state
                .canvas
                .to_data_url()
                .map_err(|error| anyhow::anyhow!("thumbnail canvas toDataURL: {error:?}"))?;
            state.image_urls.insert(source_key.clone(), image_url);
            Ok(())
        })();

        match result {
            Ok(()) => {
                self.update_captured_pets_panel();
                self.update_pet_party_modal();
            }
            Err(error) => {
                if let Some(state) = self.pet_party_thumbnail_state.as_mut() {
                    state.failed_keys.insert(source_key.clone());
                }
                web_sys::console::warn_1(&JsValue::from_str(&format!(
                    "pet thumbnail generation failed for {source_key}: {error:?}"
                )));
            }
        }
    }

    fn update_pet_party_modal(&self) {
        let selected_count = self.pet_party_selected_ids.len();
        let duplicate_weapon_message = self.pet_party_duplicate_weapon_message();
        let copy = if self.guest_pet_captures_are_temporary() {
            "Choose up to 6 active followers for this guest session, then assign one collected weapon to each active pet if you want. Party choices reset when you leave."
        } else {
            "Choose up to 6 active followers and assign their collected weapons. Your saved account keeps this party between sessions."
        };
        self.pet_party_modal_copy.set_text_content(Some(copy));
        self.pet_party_modal_count.set_text_content(Some(&format!(
            "Selected {selected_count}/{PET_FOLLOWER_COUNT}"
        )));

        let list_markup = if self.captured_pets.is_empty() {
            "<div style=\"padding:14px 16px;border-radius:16px;border:1px dashed rgba(255,255,255,0.14);color:rgba(230,237,243,0.68);font-size:13px;line-height:1.5;\">Capture pets in the world to build a party.</div>".to_string()
        } else {
            self.captured_pets
                .iter()
                .map(|pet| {
                    let is_selected = self.pet_party_selected_ids.contains(&pet.id);
                    let is_disabled = self.pet_party_save_in_flight
                        || (!is_selected && selected_count >= PET_FOLLOWER_COUNT);
                    let thumbnail = self
                        .pet_party_thumbnail_image_url_for_pet(pet)
                        .map(|image_url| {
                            format!(
                                "<div style=\"width:60px;height:60px;flex:none;\"><img alt=\"{name} thumbnail\" src=\"{image_url}\" style=\"width:100%;height:100%;display:block;object-fit:cover;border-radius:14px;border:1px solid rgba(255,255,255,0.12);box-shadow:0 12px 24px rgba(0,0,0,0.28);background:radial-gradient(circle at top,rgba(255,255,255,0.14),rgba(8,12,18,0.9));\" /></div>",
                                name = pet.display_name,
                            )
                        })
                        .unwrap_or_else(|| {
                            "<div style=\"width:60px;height:60px;flex:none;border-radius:14px;border:1px solid rgba(255,255,255,0.10);background:radial-gradient(circle at 35% 25%,rgba(255,255,255,0.18),rgba(87,130,166,0.14) 30%,rgba(9,14,21,0.92) 75%);display:grid;place-items:center;color:rgba(230,237,243,0.58);font:700 10px/1.2 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.10em;text-transform:uppercase;\">Loading</div>".to_string()
                        });
                    let button_style = if is_selected {
                        "width:100%;padding:12px 14px;border-radius:16px;border:1px solid rgba(246,198,101,0.38);background:linear-gradient(180deg,rgba(73,54,22,0.92),rgba(45,33,13,0.92));color:#f9e1aa;text-align:left;font:600 14px/1.35 ui-sans-serif,system-ui,sans-serif;cursor:pointer;"
                    } else if is_disabled {
                        "width:100%;padding:12px 14px;border-radius:16px;border:1px solid rgba(255,255,255,0.08);background:rgba(255,255,255,0.04);color:rgba(230,237,243,0.42);text-align:left;font:600 14px/1.35 ui-sans-serif,system-ui,sans-serif;cursor:not-allowed;"
                    } else {
                        "width:100%;padding:12px 14px;border-radius:16px;border:1px solid rgba(255,255,255,0.10);background:rgba(255,255,255,0.05);color:#e6edf3;text-align:left;font:600 14px/1.35 ui-sans-serif,system-ui,sans-serif;cursor:pointer;"
                    };
                    let state_label = if is_selected {
                        "Active follower"
                    } else {
                        "Inactive"
                    };
                    let disabled_attr = if is_disabled { "disabled" } else { "" };
                    let selected_weapon_id = self
                        .pet_party_equipped_weapon_ids
                        .get(&pet.id)
                        .and_then(|weapon_id| weapon_id.as_deref());
                    let loadout_markup = if is_selected {
                        if self.collected_weapons.is_empty() {
                            "<div style=\"margin-top:10px;padding:10px 12px;border-radius:12px;border:1px dashed rgba(255,255,255,0.10);color:rgba(230,237,243,0.62);font:500 12px/1.4 ui-sans-serif,system-ui,sans-serif;\">Collect a world weapon to equip this pet.</div>".to_string()
                        } else {
                            let options = std::iter::once(
                                "<option value=\"\">No weapon equipped</option>".to_string(),
                            )
                            .chain(self.collected_weapons.iter().map(|weapon| {
                                let selected_attr = if selected_weapon_id == Some(weapon.id.as_str()) {
                                    " selected"
                                } else {
                                    ""
                                };
                                let duplicate_attr = if self
                                    .pet_party_weapon_is_assigned_to_other_selected_pet(
                                        &pet.id,
                                        &weapon.id,
                                    ) && selected_weapon_id != Some(weapon.id.as_str())
                                {
                                    " disabled"
                                } else {
                                    ""
                                };
                                let weapon_label = if duplicate_attr.is_empty() {
                                    weapon.display_name.clone()
                                } else {
                                    format!("{} (Assigned)", weapon.display_name)
                                };
                                format!(
                                    "<option value=\"{}\"{selected_attr}{duplicate_attr}>{} [{}]</option>",
                                    weapon.id,
                                    weapon_label,
                                    format_weapon_kind_label(&weapon.kind),
                                )
                            }))
                            .collect::<Vec<_>>()
                            .join("");
                            format!(
                                "<div style=\"margin-top:10px;display:grid;gap:6px;\"><label for=\"pet-weapon-{pet_id}\" style=\"color:rgba(230,237,243,0.72);font:600 11px/1.2 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.08em;text-transform:uppercase;\">Equipped weapon</label><select id=\"pet-weapon-{pet_id}\" data-pet-weapon-id=\"{pet_id}\" style=\"width:100%;padding:10px 12px;border-radius:12px;border:1px solid rgba(255,255,255,0.12);background:rgba(7,11,18,0.92);color:#e6edf3;font:600 13px/1.35 ui-sans-serif,system-ui,sans-serif;\">{options}</select></div>",
                                pet_id = pet.id,
                                options = options,
                            )
                        }
                    } else {
                        "<div style=\"margin-top:10px;color:rgba(230,237,243,0.54);font:500 12px/1.4 ui-sans-serif,system-ui,sans-serif;\">Activate this pet to give it a weapon.</div>".to_string()
                    };
                    format!(
                        "<div style=\"padding:0;border-radius:18px;border:1px solid rgba(255,255,255,0.08);background:rgba(255,255,255,0.03);overflow:hidden;\"><button type=\"button\" data-pet-toggle-id=\"{}\" {disabled_attr} style=\"{button_style}\"><div style=\"display:flex;align-items:center;gap:12px;min-width:0;\">{thumbnail}<div style=\"min-width:0;\"><div style=\"font:700 14px/1.25 ui-sans-serif,system-ui,sans-serif;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;\">{name}</div><div style=\"margin-top:4px;color:rgba(230,237,243,0.70);font:500 11px/1.3 ui-sans-serif,system-ui,sans-serif;text-transform:uppercase;letter-spacing:0.08em;\">{state_label}</div></div></div></button><div style=\"padding:0 14px 14px 14px;\">{loadout_markup}</div></div>",
                        pet.id,
                        thumbnail = thumbnail,
                        name = pet.display_name,
                        loadout_markup = loadout_markup,
                    )
                })
                .collect::<Vec<_>>()
                .join("<div style=\"height:10px;\"></div>")
        };
        self.pet_party_modal_list.set_inner_html(&list_markup);

        let save_disabled = self.pet_party_save_in_flight
            || !self.can_capture_generated_pets()
            || !self.pet_party_has_unsaved_changes()
            || duplicate_weapon_message.is_some();
        if save_disabled {
            let _ = self.pet_party_save_button.set_attribute("disabled", "true");
        } else {
            let _ = self.pet_party_save_button.remove_attribute("disabled");
        }
        let save_style = if save_disabled {
            "width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.10);background:rgba(255,255,255,0.06);color:rgba(230,237,243,0.52);font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:not-allowed;"
        } else {
            "width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.18);background:linear-gradient(180deg,#f6c665,#e8a93c);color:#1b1206;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;"
        };
        let _ = self
            .pet_party_save_button
            .set_attribute("style", save_style);
        self.pet_party_save_button
            .set_text_content(Some(if self.pet_party_save_in_flight {
                "Saving Party..."
            } else {
                "Save Party"
            }));

        let default_status = if self.captured_pets.is_empty() {
            "Capture pets to choose which ones follow you."
        } else if let Some(message) = duplicate_weapon_message {
            message
        } else if self.pet_party_has_unsaved_changes() {
            "Choose up to 6 pets, assign their weapons, then save your party."
        } else {
            "Your current party is saved."
        };
        self.pet_party_modal_status.set_text_content(Some(
            self.pet_party_modal_message
                .as_deref()
                .unwrap_or(default_status),
        ));
    }

    fn process_pet_party_requests(&mut self) {
        let should_close_modal = PET_PARTY_MODAL_CLOSE_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_close = *queued;
            *queued = false;
            should_close
        });
        if should_close_modal {
            self.sync_pet_party_draft_from_snapshot();
            self.pet_party_save_in_flight = false;
            self.pet_party_modal_message = None;
            self.hide_pet_party_modal();
            self.update_pet_party_modal();
        }

        let should_open_modal = PET_PARTY_MODAL_OPEN_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_open = *queued;
            *queued = false;
            should_open
        });
        if should_open_modal {
            if !self.can_capture_generated_pets() {
                self.set_pet_notice("Sign in or continue as a guest to manage a pet party.");
            } else {
                self.sync_pet_party_draft_from_snapshot();
                self.pet_party_save_in_flight = false;
                self.pet_party_modal_message = None;
                self.show_pet_party_modal();
                self.update_pet_party_modal();
            }
        }

        let toggled_pet_ids = PET_PARTY_TOGGLE_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            std::mem::take(&mut *queued)
        });
        for pet_id in toggled_pet_ids {
            if self.pet_party_save_in_flight || !self.can_capture_generated_pets() {
                continue;
            }
            if !self.captured_pets.iter().any(|pet| pet.id == pet_id) {
                continue;
            }
            if self.pet_party_selected_ids.contains(&pet_id) {
                self.pet_party_selected_ids.remove(&pet_id);
                self.pet_party_modal_message = None;
            } else if self.pet_party_selected_ids.len() >= PET_FOLLOWER_COUNT {
                self.pet_party_modal_message = Some(format!(
                    "You can only have {} active followers.",
                    PET_FOLLOWER_COUNT
                ));
            } else {
                self.pet_party_selected_ids.insert(pet_id);
                self.pet_party_modal_message = None;
            }
            self.update_pet_party_modal();
        }

        let queued_weapon_assignments = PET_PARTY_WEAPON_ASSIGN_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            std::mem::take(&mut *queued)
        });
        for (pet_id, weapon_id) in queued_weapon_assignments {
            if self.pet_party_save_in_flight || !self.can_capture_generated_pets() {
                continue;
            }
            if !self.captured_pets.iter().any(|pet| pet.id == pet_id) {
                continue;
            }
            if let Some(weapon_id) = weapon_id.as_deref() {
                if self.pet_party_weapon_is_assigned_to_other_selected_pet(&pet_id, weapon_id) {
                    self.pet_party_modal_message =
                        Some(PET_PARTY_DUPLICATE_WEAPON_MESSAGE.to_string());
                    self.update_pet_party_modal();
                    continue;
                }
            }
            self.pet_party_equipped_weapon_ids.insert(pet_id, weapon_id);
            self.pet_party_modal_message = None;
            self.update_pet_party_modal();
        }

        let should_save_party = PET_PARTY_SAVE_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_save = *queued;
            *queued = false;
            should_save
        });
        if should_save_party {
            if !self.can_capture_generated_pets() {
                self.pet_party_modal_message =
                    Some("Sign in or continue as a guest to build a pet party.".to_string());
            } else if self.pet_party_save_in_flight {
                return;
            } else if let Some(message) = self.pet_party_duplicate_weapon_message() {
                self.pet_party_modal_message = Some(message.to_string());
            } else {
                self.pet_party_save_in_flight = true;
                self.pet_party_modal_message = Some("Saving pet party...".to_string());
                self.send_client_message(&ClientMessage::UpdatePetPartyRequest(
                    UpdatePetPartyRequest {
                        active_pet_ids: self.current_pet_party_selection_ids(),
                        equipped_weapon_assignments: self.current_pet_party_weapon_assignments(),
                    },
                ));
            }
            self.update_pet_party_modal();
        }
    }

    fn reset_pet_party_state(&mut self) {
        self.captured_pets.clear();
        self.pet_notice = None;
        self.pending_pet_notice_after_login_snapshot = None;
        self.pet_party_selected_ids.clear();
        self.pet_party_equipped_weapon_ids.clear();
        self.pet_party_save_in_flight = false;
        self.pet_party_modal_message = None;
        self.hide_pet_party_modal();
        self.update_captured_pets_panel();
        self.update_pet_party_modal();
    }

    fn reset_weapon_collection_state(&mut self) {
        self.collected_weapons.clear();
        self.weapon_notice = None;
        self.pending_weapon_notice_after_login_snapshot = None;
        self.update_weapon_collection_panel();
    }

    fn update_captured_pets_panel(&self) {
        let label = if self.captured_pets.is_empty() {
            "Pet Party".to_string()
        } else {
            format!("Pet Party ({})", self.captured_pets.len())
        };
        self.captured_pets_panel.set_text_content(Some(&label));
    }

    fn update_weapon_collection_panel(&self) {
        let mut lines = vec![format!("Weapons ({})", self.collected_weapons.len())];
        if let Some(notice) = self.weapon_notice.as_deref() {
            lines.push(notice.to_string());
        }
        if self.collected_weapons.is_empty() {
            lines.push("No collected weapons yet.".to_string());
        } else {
            for weapon in self.collected_weapons.iter().take(6) {
                lines.push(format!(
                    "{} [{}]",
                    weapon.display_name,
                    format_weapon_kind_label(&weapon.kind)
                ));
            }
            if self.collected_weapons.len() > 6 {
                lines.push(format!("+{} more", self.collected_weapons.len() - 6));
            }
        }
        self.weapons_panel.set_text_content(Some(&lines.join("\n")));
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
                    self.clear_pending_login_notices();
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
                            self.remote_player_latest_ticks.clear();
                            self.remote_player_velocities.clear();
                            self.remote_player_yaws.clear();
                            self.remote_player_avatar_selections.clear();
                            self.remote_player_avatar_states.clear();
                            self.remote_player_pet_identities.clear();
                            self.remote_pet_states.clear();
                            self.wild_pets.clear();
                            self.hosted_wild_pets.clear();
                            self.world_weapons.clear();
                            self.remote_media.clear();
                            self.remote_avatar_assets.clear();
                            self.pending_remote_avatar_urls.clear();
                            self.remote_pet_assets.clear();
                            self.remote_pet_model_bytes.clear();
                            self.pending_remote_pet_urls.clear();
                            self.remote_weapon_assets.clear();
                            self.pending_remote_weapon_urls.clear();
                            clear_peer_connection_registries();
                            self.pet_asset = None;
                            self.pet_asset_attempted = false;
                            self.pet_followers.clear();
                            self.pet_followers_need_reset = false;
                            self.last_sent_position = None;
                            self.last_sent_velocity = None;
                            self.last_sent_yaw = None;
                            self.last_input_broadcast_at = None;
                            self.last_peer_realtime_broadcast_at = None;
                            self.reset_pet_party_state();
                            self.reset_weapon_collection_state();
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
                                self.remote_players.clear();
                                self.remote_player_latest_ticks.clear();
                                self.remote_player_velocities.clear();
                                self.remote_player_yaws.clear();
                                self.remote_player_avatar_selections.clear();
                                self.remote_player_avatar_states.clear();
                                self.remote_player_pet_identities.clear();
                                self.remote_pet_states.clear();
                                self.wild_pets.clear();
                                self.hosted_wild_pets.clear();
                                self.world_weapons.clear();
                                self.remote_media.clear();
                                self.remote_avatar_assets.clear();
                                self.pending_remote_avatar_urls.clear();
                                self.remote_pet_assets.clear();
                                self.remote_pet_model_bytes.clear();
                                self.pending_remote_pet_urls.clear();
                                self.remote_weapon_assets.clear();
                                self.pending_remote_weapon_urls.clear();
                                clear_peer_connection_registries();
                                self.last_sent_position = None;
                                self.last_sent_velocity = None;
                                self.last_sent_yaw = None;
                                self.last_input_broadcast_at = None;
                                self.last_peer_realtime_broadcast_at = None;
                                self.reset_pet_party_state();
                                self.reset_weapon_collection_state();
                                if !response.message.trim().is_empty() {
                                    self.queue_login_notices(response.message.clone());
                                }
                                self.camera.position = Vec3::new(
                                    response.spawn_position.x as f32 + 0.5,
                                    response.spawn_position.y as f32 + PLAYER_EYE_HEIGHT,
                                    response.spawn_position.z as f32 + 0.5,
                                );
                                self.camera.vertical_velocity = 0.0;
                                self.camera.on_ground = false;
                                self.spawn_settled = false;
                                self.current_chunk =
                                    chunk_from_world_position(self.camera.position);
                                let chunk_radius = self.chunk_quality_preset.chunk_radius();
                                self.desired_chunks =
                                    desired_chunk_set(self.current_chunk, chunk_radius);
                                self.applied_chunk_radius = chunk_radius;
                                self.completed_meshes.clear();
                                self.last_reprioritize_chunk = self.current_chunk;
                                self.last_reprioritize_forward = self.camera.forward();
                                self.pending_generation.clear();
                                self.inflight_generation.clear();
                                self.dirty_generation.clear();
                                let desired_positions = ordered_desired_chunk_positions(
                                    self.current_chunk,
                                    chunk_radius,
                                );
                                for position in desired_positions {
                                    self.schedule_chunk_rebuild_deferred(position);
                                }
                                self.send_chunk_subscription(self.current_chunk);
                                self.link_panel = LinkPanel::near_spawn(self.camera.position);
                                self.spawn_position = Some(response.spawn_position);
                                self.pet_asset = None;
                                self.pet_asset_attempted = false;
                                self.pet_followers.clear();
                                self.pet_followers_need_reset = true;
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
                                self.pending_generation
                                    .retain(|pending| *pending != position);
                                self.inflight_generation.remove(&position);
                                self.dirty_generation.remove(&position);
                                self.completed_meshes
                                    .retain(|mesh| mesh.position != position);
                            }
                        }
                        ServerMessage::InventorySnapshot(InventorySnapshot { slots }) => {
                            self.hotbar_blocks = slots.into_iter().map(|slot| slot.block).collect();
                            if self.hotbar_blocks.is_empty() {
                                self.hotbar_blocks =
                                    vec![BlockId::Grass, BlockId::Stone, BlockId::Planks];
                            }
                            if self.selected_hotbar >= self.hotbar_blocks.len() {
                                self.selected_hotbar = self.hotbar_blocks.len().saturating_sub(1);
                            }
                            update_hotbar_ui(
                                &self.hotbar_slots,
                                &self.hotbar_blocks,
                                self.selected_hotbar,
                            );
                        }
                        ServerMessage::PlayerStateSnapshot(snapshot) => {
                            if Some(snapshot.player_id) != self.player_id {
                                self.apply_remote_motion_state(
                                    snapshot.player_id,
                                    snapshot.tick,
                                    snapshot.position,
                                    snapshot.velocity,
                                    snapshot.yaw,
                                    snapshot.pet_states.clone(),
                                );
                                self.remote_player_pet_identities
                                    .insert(snapshot.player_id, snapshot.active_pet_models.clone());
                                let selection = PlayerAvatarSelection {
                                    idle_model_url: snapshot.idle_model_url.clone(),
                                    run_model_url: snapshot.run_model_url.clone(),
                                    dance_model_url: snapshot.dance_model_url.clone(),
                                };
                                let selection_changed = self
                                    .remote_player_avatar_selections
                                    .get(&snapshot.player_id)
                                    .map(|existing| existing != &selection)
                                    .unwrap_or(true);
                                self.remote_player_avatar_selections
                                    .insert(snapshot.player_id, selection.clone());
                                let state = self
                                    .remote_player_avatar_states
                                    .entry(snapshot.player_id)
                                    .or_default();
                                if selection_changed {
                                    state.active_url = None;
                                }
                                self.ensure_remote_avatar_selection_requested(&selection);
                                self.ensure_pet_identities_requested(&snapshot.active_pet_models);
                                let peer_position = self
                                    .remote_players
                                    .get(&snapshot.player_id)
                                    .copied()
                                    .unwrap_or(snapshot.position);
                                if self.player_is_nearby_for_peer_realtime(peer_position) {
                                    self.ensure_peer_connection(snapshot.player_id);
                                }
                            }
                        }
                        ServerMessage::PlayerLeft(PlayerLeft { player_id }) => {
                            self.despawn_remote_player(player_id);
                        }
                        ServerMessage::WildPetSnapshot(snapshot) => {
                            self.apply_wild_pet_snapshot(snapshot);
                        }
                        ServerMessage::WildPetUnload(WildPetUnload { pet_ids }) => {
                            for pet_id in pet_ids {
                                self.wild_pets.remove(&pet_id);
                                self.hosted_wild_pets.remove(&pet_id);
                            }
                        }
                        ServerMessage::WorldWeaponSnapshot(snapshot) => {
                            self.apply_world_weapon_snapshot(snapshot);
                        }
                        ServerMessage::WorldWeaponUnload(WorldWeaponUnload { weapon_ids }) => {
                            for weapon_id in weapon_ids {
                                self.world_weapons.remove(&weapon_id);
                            }
                        }
                        ServerMessage::PetWeaponShot(shot) => {
                            self.apply_pet_weapon_shot(shot);
                        }
                        ServerMessage::CapturedPetsSnapshot(CapturedPetsSnapshot { pets }) => {
                            self.captured_pets = pets;
                            self.pet_notice = None;
                            self.apply_pending_pet_login_notice();
                            self.pet_party_modal_message = None;
                            self.sync_pet_party_draft_from_snapshot();
                            self.pet_followers_need_reset = true;
                            self.update_captured_pets_panel();
                            self.update_pet_party_modal();
                        }
                        ServerMessage::CollectedWeaponsSnapshot(CollectedWeaponsSnapshot {
                            weapons,
                        }) => {
                            self.collected_weapons = weapons;
                            self.weapon_notice = None;
                            self.apply_pending_weapon_login_notice();
                            self.update_weapon_collection_panel();
                        }
                        ServerMessage::CaptureWildPetResult(CaptureWildPetResult {
                            status,
                            message,
                            ..
                        }) => {
                            self.set_pet_notice(message);
                            if matches!(status, CaptureWildPetStatus::Captured) {
                                self.pet_followers_need_reset = true;
                            }
                        }
                        ServerMessage::StartPetCombatResult(result) => {
                            self.set_pet_notice(result.message);
                        }
                        ServerMessage::PickupWorldWeaponResult(PickupWorldWeaponResult {
                            message,
                            ..
                        }) => {
                            self.set_weapon_notice(message);
                        }
                        ServerMessage::UpdatePetPartyResult(UpdatePetPartyResult {
                            accepted,
                            message,
                        }) => {
                            self.pet_party_save_in_flight = false;
                            self.pet_party_modal_message = Some(message.clone());
                            self.set_pet_notice(message);
                            if accepted {
                                self.pet_followers_need_reset = true;
                            }
                            self.update_pet_party_modal();
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
                    self.remote_player_latest_ticks.clear();
                    self.remote_player_velocities.clear();
                    self.remote_player_yaws.clear();
                    self.remote_player_avatar_selections.clear();
                    self.remote_player_avatar_states.clear();
                    self.remote_player_pet_identities.clear();
                    self.remote_pet_states.clear();
                    self.wild_pets.clear();
                    self.hosted_wild_pets.clear();
                    self.world_weapons.clear();
                    self.remote_media.clear();
                    self.remote_avatar_assets.clear();
                    self.pending_remote_avatar_urls.clear();
                    self.remote_pet_assets.clear();
                    self.remote_pet_model_bytes.clear();
                    self.pending_remote_pet_urls.clear();
                    self.remote_weapon_assets.clear();
                    self.pending_remote_weapon_urls.clear();
                    clear_peer_connection_registries();
                    self.pet_asset = None;
                    self.pet_asset_attempted = false;
                    self.pet_followers.clear();
                    self.pet_followers_need_reset = false;
                    self.last_sent_position = None;
                    self.last_sent_velocity = None;
                    self.last_sent_yaw = None;
                    self.last_input_broadcast_at = None;
                    self.last_peer_realtime_broadcast_at = None;
                    self.reset_pet_party_state();
                    self.reset_weapon_collection_state();
                    web_sys::console::error_1(&JsValue::from_str(&format!(
                        "multiplayer disconnected: {reason}"
                    )));
                }
            }
        }
    }

    fn process_auth_events(&mut self) {
        let should_refresh_auth = AUTH_REFRESH_QUEUE.with(|queue| {
            let mut queued = queue.borrow_mut();
            let should_refresh = *queued;
            *queued = false;
            should_refresh
        });
        if should_refresh_auth {
            request_auth_session_refresh(self.auth_tx.clone());
        }

        while let Ok(event) = self.auth_rx.try_recv() {
            match event {
                AuthEvent::Resolved(result) => match result {
                    Ok(Some(user)) => {
                        self.auth_user = Some(user);
                        self.auth_status = AuthStatus::SignedIn;
                        self.pet_notice = None;
                        self.weapon_notice = None;
                        self.clear_pending_login_notices();
                        self.pet_party_modal_message = None;
                        self.update_captured_pets_panel();
                        self.update_weapon_collection_panel();
                        self.update_pet_party_modal();
                    }
                    Ok(None) => {
                        self.auth_user = None;
                        self.auth_status = AuthStatus::SignedOut;
                        self.pet_notice = None;
                        self.weapon_notice = None;
                        self.clear_pending_login_notices();
                        self.pet_party_modal_message = None;
                        self.hide_pet_party_modal();
                        self.update_captured_pets_panel();
                        self.update_weapon_collection_panel();
                        self.update_pet_party_modal();
                    }
                    Err(message) => {
                        self.auth_user = None;
                        self.auth_status = AuthStatus::Failed(message);
                        self.pet_notice = None;
                        self.weapon_notice = None;
                        self.clear_pending_login_notices();
                        self.pet_party_modal_message = None;
                        self.hide_pet_party_modal();
                        self.update_captured_pets_panel();
                        self.update_weapon_collection_panel();
                        self.update_pet_party_modal();
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
            self.pet_notice = None;
            self.weapon_notice = None;
            self.clear_pending_login_notices();
            self.pet_party_modal_message = None;
            self.update_captured_pets_panel();
            self.update_weapon_collection_panel();
            self.update_pet_party_modal();
        }

        self.sync_auth_overlay();
        self.sync_player_avatar_panel();
        self.sync_logout_button();
        self.maybe_send_login_request();
    }

    fn sync_auth_overlay(&self) {
        match &self.auth_status {
            AuthStatus::Checking => {
                let _ = self
                    .auth_overlay
                    .set_attribute("style", auth_overlay_style());
                let _ = self
                    .auth_overlay_status
                    .set_attribute("style", auth_overlay_status_style());
                self.auth_overlay_status
                    .set_text_content(Some(AUTH_STATUS_CHECKING));
            }
            AuthStatus::SignedOut => {
                let _ = self
                    .auth_overlay
                    .set_attribute("style", auth_overlay_style());
                let _ = self
                    .auth_overlay_status
                    .set_attribute("style", "display:none;");
                self.auth_overlay_status.set_text_content(None);
            }
            AuthStatus::SignedIn => {
                let _ = self.auth_overlay.set_attribute("style", "display:none;");
            }
            AuthStatus::Failed(message) => {
                let _ = self
                    .auth_overlay
                    .set_attribute("style", auth_overlay_style());
                let _ = self
                    .auth_overlay_status
                    .set_attribute("style", auth_overlay_status_style());
                self.auth_overlay_status.set_text_content(Some(message));
            }
        }
    }

    fn sync_player_avatar_panel(&self) {
        let should_show = matches!(self.auth_status, AuthStatus::SignedIn)
            && self
                .auth_user
                .as_ref()
                .is_some_and(|user| !auth_user_is_guest(user));
        if should_show {
            let _ = self
                .player_avatar_panel
                .set_attribute("style", player_avatar_launcher_row_style());
            let _ = self
                .player_avatar_modal
                .set_attribute("style", "display:block;");
        } else {
            let _ = self
                .player_avatar_panel
                .set_attribute("style", "display:none;");
            let _ = self
                .player_avatar_modal
                .set_attribute("style", "display:none;");
        }
    }

    fn sync_logout_button(&self) {
        match &self.auth_status {
            AuthStatus::SignedIn => {
                let is_guest = self
                    .auth_user
                    .as_ref()
                    .filter(|user| auth_user_is_guest(user))
                    .is_some();
                let label = if is_guest {
                    "Exit Guest Session"
                } else {
                    "Log Out"
                };
                self.logout_button.set_text_content(Some(label));
                let _ = self.logout_button.set_attribute(
                    "data-session-kind",
                    if is_guest { "guest" } else { "account" },
                );
                let _ = self
                    .logout_button
                    .set_attribute("style", logout_button_style());
            }
            _ => {
                let _ = self.logout_button.remove_attribute("data-session-kind");
                let _ = self.logout_button.set_attribute("style", "display:none;");
            }
        }
    }

    fn maybe_send_login_request(&mut self) {
        if !self.transport_open
            || !self.server_ready_for_login
            || self.logged_in
            || self.login_request_sent
        {
            return;
        }

        let Some(user) = self.auth_user.as_ref() else {
            return;
        };

        self.send_client_message(&ClientMessage::LoginRequest(LoginRequest {
            name: user.display_name(),
            idle_model_url: user
                .avatar_selection
                .as_ref()
                .and_then(|selection| selection.idle_model_url.clone()),
            run_model_url: user
                .avatar_selection
                .as_ref()
                .and_then(|selection| selection.run_model_url.clone()),
            dance_model_url: user
                .avatar_selection
                .as_ref()
                .and_then(|selection| selection.dance_model_url.clone()),
            auth_token: user.game_auth_token.clone(),
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
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "encode client message: {error}"
                )));
            }
        }
    }

    fn send_chunk_subscription(&self, center: ChunkPos) {
        if !self.logged_in {
            return;
        }
        self.send_client_message(&ClientMessage::SubscribeChunks(SubscribeChunks {
            center,
            radius: self.chunk_quality_preset.chunk_radius() as u8,
        }));
    }

    fn next_state_tick(&mut self) -> u64 {
        self.tick_counter = self.tick_counter.wrapping_add(1);
        self.tick_counter
    }

    fn current_pet_snapshots(&self) -> Vec<PetStateSnapshot> {
        self.pet_followers
            .iter()
            .map(|pet| PetStateSnapshot {
                position: pet.feet_position.to_array(),
                yaw: pet.yaw,
            })
            .collect()
    }

    fn current_wild_pet_motion_snapshots(&self) -> Vec<WildPetMotionSnapshot> {
        self.hosted_wild_pets
            .values()
            .map(|pet| WildPetMotionSnapshot {
                pet_id: pet.pet_id,
                position: pet.feet_position.to_array(),
                velocity: [
                    pet.horizontal_velocity.x,
                    pet.vertical_velocity,
                    pet.horizontal_velocity.z,
                ],
                yaw: pet.yaw,
            })
            .collect()
    }

    fn apply_remote_motion_state(
        &mut self,
        player_id: u64,
        tick: u64,
        position: [f32; 3],
        velocity: [f32; 3],
        yaw: f32,
        pet_states: Vec<PetStateSnapshot>,
    ) {
        if Some(player_id) == self.player_id {
            return;
        }

        let should_apply = self
            .remote_player_latest_ticks
            .get(&player_id)
            .map(|latest| tick >= *latest)
            .unwrap_or(true);
        if !should_apply {
            return;
        }

        self.remote_player_latest_ticks.insert(player_id, tick);
        self.remote_players.insert(player_id, position);
        self.remote_player_velocities.insert(player_id, velocity);
        self.remote_player_yaws.insert(player_id, yaw);
        self.remote_pet_states.insert(player_id, pet_states);
    }

    fn apply_remote_wild_pet_motion(
        &mut self,
        host_player_id: u64,
        tick: u64,
        wild_pet_states: Vec<WildPetMotionSnapshot>,
    ) {
        for motion in wild_pet_states {
            if self.hosted_wild_pets.contains_key(&motion.pet_id) {
                continue;
            }
            let Some(pet) = self.wild_pets.get_mut(&motion.pet_id) else {
                continue;
            };
            if pet.host_player_id != Some(host_player_id) || tick < pet.latest_tick {
                continue;
            }

            pet.position = Vec3::from_array(motion.position);
            pet.velocity = Vec3::from_array(motion.velocity);
            pet.yaw = motion.yaw;
            pet.latest_tick = tick;
        }
    }

    fn apply_wild_pet_snapshot(&mut self, snapshot: WildPetSnapshot) {
        let host_player_id = snapshot.host_player_id;
        if let Some(host_player_id) = host_player_id.filter(|id| Some(*id) != self.player_id) {
            self.ensure_peer_connection(host_player_id);
        }

        let pet_id = snapshot.pet_id;
        let mut state = self
            .wild_pets
            .get(&pet_id)
            .cloned()
            .unwrap_or_else(|| WildPetClientState::from_snapshot(&snapshot));
        let host_changed = state.host_player_id != snapshot.host_player_id;
        let is_local_host = snapshot.host_player_id == self.player_id;

        state.spawn_position = Vec3::from_array(snapshot.spawn_position);
        state.pet_identity = snapshot.pet_identity.clone();
        state.health = snapshot.health;
        state.max_health = snapshot.max_health;
        state.host_player_id = snapshot.host_player_id;
        if (snapshot.tick >= state.latest_tick && !is_local_host)
            || host_changed
            || !self.hosted_wild_pets.contains_key(&pet_id)
        {
            state.position = Vec3::from_array(snapshot.position);
            state.velocity = Vec3::from_array(snapshot.velocity);
            state.yaw = snapshot.yaw;
            state.latest_tick = snapshot.tick;
        }
        self.wild_pets.insert(pet_id, state.clone());

        if is_local_host {
            if let Some(hosted) = self.hosted_wild_pets.get_mut(&pet_id) {
                hosted.spawn_position = state.spawn_position;
            } else {
                let wander_target = self.choose_wild_pet_wander_target(state.position);
                self.hosted_wild_pets.insert(
                    pet_id,
                    HostedWildPetState::from_client_state(&state, wander_target),
                );
            }
        } else {
            self.hosted_wild_pets.remove(&pet_id);
        }
    }

    fn apply_world_weapon_snapshot(&mut self, snapshot: WorldWeaponSnapshot) {
        let weapon_id = snapshot.weapon_id;
        self.world_weapons
            .entry(weapon_id)
            .and_modify(|weapon| {
                if snapshot.tick >= weapon.latest_tick {
                    weapon.position = Vec3::from_array(snapshot.position);
                    weapon.latest_tick = snapshot.tick;
                }
                weapon.weapon_identity = snapshot.weapon_identity.clone();
            })
            .or_insert_with(|| WorldWeaponClientState::from_snapshot(&snapshot));
    }

    fn apply_pet_weapon_shot(&mut self, shot: PetWeaponShot) {
        let now = Instant::now();
        let kind = PetWeaponEffectKind::from_weapon_kind(&shot.weapon_kind);
        self.pet_weapon_shots
            .retain(|active_shot| active_shot.expires_at > now);
        self.pet_weapon_shots.push(PetWeaponShotClientState {
            kind,
            origin: Vec3::from_array(shot.origin),
            target: Vec3::from_array(shot.target),
            started_at: now,
            expires_at: now + kind.lifetime(),
        });
    }

    fn player_is_nearby_for_peer_realtime(&self, position: [f32; 3]) -> bool {
        let delta = Vec3::from_array(position) - self.camera.position;
        delta.length_squared() <= PEER_REALTIME_RADIUS * PEER_REALTIME_RADIUS
    }

    fn local_avatar_selection(&self) -> Option<&PlayerAvatarSelection> {
        self.auth_user
            .as_ref()
            .and_then(|user| user.avatar_selection.as_ref())
    }

    fn ensure_local_avatar_assets_requested(&mut self) {
        let selection = self.local_avatar_selection().cloned();
        if let Some(selection) = selection.as_ref() {
            self.ensure_remote_avatar_selection_requested(selection);
        }
    }

    fn update_local_avatar_playback(
        &mut self,
        dt_secs: f32,
        desired_movement: Vec3,
        actual_velocity: Vec3,
    ) {
        if !self.is_third_person_active() {
            self.avatar_yaw = self.camera.yaw;
        } else {
            let mut desired_facing = Vec3::new(actual_velocity.x, 0.0, actual_velocity.z);
            if desired_facing.length_squared() <= 0.0001 {
                desired_facing = Vec3::new(desired_movement.x, 0.0, desired_movement.z);
            }
            if desired_facing.length_squared() > 0.0001 {
                let target_yaw = desired_facing.x.atan2(desired_facing.z);
                self.avatar_yaw = rotate_angle_towards(
                    self.avatar_yaw,
                    target_yaw,
                    LOCAL_AVATAR_TURN_SPEED * dt_secs.max(0.0),
                );
            }
        }

        let moving = self.movement_active
            || Vec3::new(actual_velocity.x, 0.0, actual_velocity.z).length_squared() > 0.01;
        let selection = self.local_avatar_selection().cloned().unwrap_or_default();
        let desired_animation = if moving {
            RemoteAvatarAnimation::Run
        } else {
            RemoteAvatarAnimation::Idle
        };
        let active_url = selection
            .url_for_animation(desired_animation)
            .or_else(|| selection.first_available_url())
            .map(str::to_owned);

        if self.local_avatar_state.animation != desired_animation
            || self.local_avatar_state.active_url != active_url
        {
            self.local_avatar_state.animation = desired_animation;
            self.local_avatar_state.active_url = active_url.clone();
            self.local_avatar_state.playback_time = 0.0;
        }

        let maybe_duration = active_url
            .as_deref()
            .and_then(|url| self.remote_avatar_assets.get(url))
            .map(|asset| asset.animation.duration_seconds);
        if let Some(duration) = maybe_duration.filter(|duration| *duration > 0.0) {
            self.local_avatar_state.playback_time =
                (self.local_avatar_state.playback_time + dt_secs) % duration;
        }
    }

    fn update_camera_view(&mut self, dt_secs: f32) {
        if !self.is_third_person_active() {
            self.third_person.current_zoom = 0.0;
            self.third_person.render_eye = self.camera.position;
            return;
        }

        self.third_person.current_zoom = move_towards_scalar(
            self.third_person.current_zoom,
            self.third_person.target_zoom,
            THIRD_PERSON_ZOOM_SPEED * dt_secs.max(0.0),
        );

        if self.third_person.current_zoom < THIRD_PERSON_MIN_ACTIVE_ZOOM {
            self.third_person.render_eye = self.camera.position;
            return;
        }

        let pivot = self.render_camera_pivot();
        let desired_eye = pivot - self.camera.forward() * self.third_person.current_zoom;
        let travel = desired_eye - pivot;
        let distance = travel.length();
        let mut last_clear = if self.point_collides_with_world(pivot, THIRD_PERSON_CAMERA_CLEARANCE)
        {
            self.camera.position
        } else {
            pivot
        };

        if distance <= f32::EPSILON {
            self.third_person.render_eye = last_clear;
            return;
        }

        let direction = travel / distance;
        let steps = (distance / THIRD_PERSON_CAMERA_COLLISION_STEP)
            .ceil()
            .max(1.0) as usize;
        for step_index in 1..=steps {
            let step_distance =
                (step_index as f32 * THIRD_PERSON_CAMERA_COLLISION_STEP).min(distance);
            let candidate = pivot + direction * step_distance;
            if self.point_collides_with_world(candidate, THIRD_PERSON_CAMERA_CLEARANCE) {
                break;
            }
            last_clear = candidate;
        }

        self.third_person.render_eye = last_clear;
    }

    fn render_camera_eye(&self) -> Vec3 {
        if self.is_third_person_active() {
            self.third_person.render_eye
        } else {
            self.camera.position
        }
    }

    fn render_camera_pivot(&self) -> Vec3 {
        let (_, right) = horizontal_basis_from_yaw(self.camera.yaw);
        self.camera.position
            + right * self.third_person.shoulder_offset.x
            + Vec3::Y * self.third_person.shoulder_offset.y
    }

    fn local_avatar_active_url(&self) -> Option<&str> {
        self.local_avatar_state.active_url.as_deref()
    }

    fn local_avatar_asset_ready(&self) -> bool {
        self.local_avatar_active_url()
            .or_else(|| {
                self.local_avatar_selection()
                    .and_then(|selection| selection.first_available_url())
            })
            .and_then(|url| self.remote_avatar_assets.get(url))
            .is_some()
    }

    fn tick(&mut self) {
        self.sync_pointer_lock_state();
        self.process_chunk_quality_requests();
        self.process_perf_panel_toggle_requests();
        self.process_auth_events();
        self.process_pet_party_requests();
        self.drain_network();
        self.process_peer_realtime_events();
        self.ensure_local_avatar_assets_requested();
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick);
        self.last_tick = now;
        let dt_secs = dt.as_secs_f32();
        self.record_frame_time(dt_secs);
        self.simulation_frame_index = self.simulation_frame_index.wrapping_add(1);
        self.update_remote_avatar_playback(dt_secs);

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
                self.update_local_avatar_playback(dt_secs, Vec3::ZERO, Vec3::ZERO);
                self.update_camera_view(dt_secs);
                return;
            }
        }

        let mut movement_for_server = Vec3::new(movement.x, 0.0, movement.z);
        if movement_for_server.length_squared() > 1.0 {
            movement_for_server = movement_for_server.normalize();
        }
        let forward =
            Vec3::new(self.camera.yaw.sin(), 0.0, self.camera.yaw.cos()).normalize_or_zero();
        let right = Vec3::new(-forward.z, 0.0, forward.x);
        let world_movement = forward * -movement_for_server.z + right * movement_for_server.x;

        let previous_position = self.camera.position;
        self.update_camera_physics(dt, movement, jump, sprint);
        let actual_velocity = if dt_secs > 0.0 {
            (self.camera.position - previous_position) / dt_secs
        } else {
            Vec3::ZERO
        };
        self.update_local_avatar_playback(dt_secs, world_movement, actual_velocity);
        if self.logged_in {
            self.ensure_pet_followers_initialized();
            self.update_pet_followers(dt);
            self.update_hosted_wild_pets(dt);
        }
        self.update_camera_view(dt_secs);
        if !self.logged_in {
            return;
        }
        let position = self.camera.position.to_array();
        let velocity = [
            actual_velocity.x,
            self.camera.vertical_velocity,
            actual_velocity.z,
        ];
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
            .map(|last| angle_delta(self.avatar_yaw, last).abs() > 0.0025)
            .unwrap_or(true);
        let always_broadcast_motion = true;
        let websocket_send_due = self
            .last_input_broadcast_at
            .map(|last| now.duration_since(last) >= INPUT_BROADCAST_INTERVAL)
            .unwrap_or(true);
        let peer_channels = self
            .remote_media
            .iter()
            .filter_map(|(&player_id, remote)| {
                if !remote.data_channel_open {
                    return None;
                }
                let channel = remote.data_channel.as_ref()?;
                let position = *self.remote_players.get(&player_id)?;
                self.player_is_nearby_for_peer_realtime(position)
                    .then(|| channel.clone())
            })
            .collect::<Vec<_>>();
        let peer_send_due = !peer_channels.is_empty()
            && self
                .last_peer_realtime_broadcast_at
                .map(|last| now.duration_since(last) >= PEER_REALTIME_BROADCAST_INTERVAL)
                .unwrap_or(true);

        if peer_send_due || websocket_send_due {
            let state_tick = self.next_state_tick();
            let pet_states = self.current_pet_snapshots();
            let wild_pet_states = self.current_wild_pet_motion_snapshots();

            if peer_send_due {
                if let Ok(bytes) = encode(&PeerRealtimeState {
                    tick: state_tick,
                    position,
                    velocity,
                    yaw: self.avatar_yaw,
                    pet_states: pet_states.clone(),
                    wild_pet_states: wild_pet_states.clone(),
                }) {
                    for channel in peer_channels {
                        let _ = channel.send_with_u8_array(&bytes);
                    }
                    self.last_peer_realtime_broadcast_at = Some(now);
                }
            }

            if websocket_send_due
                && (always_broadcast_motion
                    || (should_broadcast_motion && (position_changed || velocity_changed))
                    || yaw_changed)
            {
                self.send_client_message(&ClientMessage::PlayerInputTick(PlayerInputTick {
                    tick: state_tick,
                    client_sent_at_ms: Some(js_sys::Date::now().max(0.0) as u64),
                    movement: [world_movement.x, 0.0, world_movement.z],
                    position: Some(position),
                    velocity: Some(velocity),
                    yaw: Some(self.avatar_yaw),
                    jump,
                    pet_states,
                    wild_pet_states,
                }));
                self.last_sent_position = Some(position);
                self.last_sent_velocity = Some(velocity);
                self.last_sent_yaw = Some(self.avatar_yaw);
                self.last_input_broadcast_at = Some(now);
            }
        }
    }

    fn player_feet_position(&self) -> Vec3 {
        self.camera.position - Vec3::Y * PLAYER_EYE_HEIGHT
    }

    fn ensure_pet_followers_initialized(&mut self) {
        if !self.spawn_settled {
            return;
        }
        let desired_count = self
            .active_captured_pet_identities()
            .len()
            .min(PET_FOLLOWER_COUNT);
        if desired_count == 0 {
            self.pet_followers.clear();
            self.pet_followers_need_reset = false;
            return;
        }
        if !self.pet_followers_need_reset && self.pet_followers.len() == desired_count {
            return;
        }

        let player_feet = self.player_feet_position();
        let (forward, right) = horizontal_basis_from_yaw(self.camera.yaw);
        self.pet_followers = PET_SLOT_OFFSETS
            .iter()
            .copied()
            .take(desired_count)
            .map(|(right_offset, forward_offset)| {
                let slot_target = player_feet + right * right_offset + forward * forward_offset;
                let feet_position = self.find_safe_pet_position(slot_target, player_feet);
                let distance = horizontal_distance(feet_position, slot_target);
                PetFollowerState::new(feet_position, self.camera.yaw, distance)
            })
            .collect();
        self.pet_followers_need_reset = false;
    }

    fn update_pet_followers(&mut self, dt: Duration) {
        let dt_secs = dt.as_secs_f32();
        let desired_count = self
            .active_captured_pet_identities()
            .len()
            .min(PET_FOLLOWER_COUNT);
        if desired_count == 0 {
            self.pet_followers.clear();
            return;
        }
        if self.pet_followers.len() != desired_count {
            self.pet_followers_need_reset = true;
            self.ensure_pet_followers_initialized();
        }
        if dt_secs <= 0.0 || self.pet_followers.is_empty() {
            return;
        }

        let player_feet = self.player_feet_position();
        let (forward, right) = horizontal_basis_from_yaw(self.camera.yaw);
        for index in 0..self.pet_followers.len() {
            let (right_offset, forward_offset) = PET_SLOT_OFFSETS[index];
            let slot_target = player_feet + right * right_offset + forward * forward_offset;
            let mut pet = self.pet_followers[index];
            self.update_pet_follower(&mut pet, slot_target, player_feet, dt_secs);
            self.pet_followers[index] = pet;
        }
    }

    fn update_hosted_wild_pets(&mut self, dt: Duration) {
        let dt_secs = dt.as_secs_f32();
        if dt_secs <= 0.0 || self.hosted_wild_pets.is_empty() {
            return;
        }

        let reduced_dt_secs = dt_secs * HOSTED_WILD_PET_REDUCED_UPDATE_STRIDE as f32;
        let simulation_frame_index = self.simulation_frame_index;
        let pet_ids = self.hosted_wild_pets.keys().copied().collect::<Vec<_>>();
        for pet_id in pet_ids {
            let Some(mut pet) = self.hosted_wild_pets.get(&pet_id).copied() else {
                continue;
            };
            if self.hosted_wild_pet_uses_full_simulation(pet.feet_position) {
                self.update_hosted_wild_pet(&mut pet, dt_secs);
            } else if simulation_frame_index.wrapping_add(pet_id)
                % HOSTED_WILD_PET_REDUCED_UPDATE_STRIDE
                == 0
            {
                self.update_hosted_wild_pet_reduced(&mut pet, reduced_dt_secs);
            }
            self.hosted_wild_pets.insert(pet_id, pet);
            let fallback_identity = self
                .wild_pets
                .get(&pet_id)
                .map(|state| state.pet_identity.clone())
                .unwrap_or(PetIdentity {
                    id: format!("wild-{pet_id}"),
                    display_name: "Wild Dog".to_string(),
                    model_url: None,
                    equipped_weapon: None,
                });
            self.wild_pets
                .entry(pet_id)
                .and_modify(|state| {
                    state.position = pet.feet_position;
                    state.velocity = Vec3::new(
                        pet.horizontal_velocity.x,
                        pet.vertical_velocity,
                        pet.horizontal_velocity.z,
                    );
                    state.yaw = pet.yaw;
                    state.host_player_id = self.player_id;
                })
                .or_insert_with(|| WildPetClientState {
                    pet_id,
                    pet_identity: fallback_identity,
                    spawn_position: pet.spawn_position,
                    position: pet.feet_position,
                    velocity: Vec3::new(
                        pet.horizontal_velocity.x,
                        pet.vertical_velocity,
                        pet.horizontal_velocity.z,
                    ),
                    yaw: pet.yaw,
                    health: DEFAULT_WILD_PET_MAX_HEALTH,
                    max_health: DEFAULT_WILD_PET_MAX_HEALTH,
                    host_player_id: self.player_id,
                    latest_tick: 0,
                });
        }
    }

    fn hosted_wild_pet_uses_full_simulation(&self, position: Vec3) -> bool {
        let delta = position - self.camera.position;
        delta.length_squared()
            <= HOSTED_WILD_PET_FULL_SIM_DISTANCE * HOSTED_WILD_PET_FULL_SIM_DISTANCE
    }

    fn update_hosted_wild_pet(&mut self, pet: &mut HostedWildPetState, dt_secs: f32) {
        if self.hosted_wild_pet_needs_reset(pet) {
            self.reset_hosted_wild_pet(pet);
            return;
        }

        if pet.idle_timer > 0.0 {
            pet.idle_timer = (pet.idle_timer - dt_secs).max(0.0);
            if pet.idle_timer <= 0.0 {
                pet.wander_target = self.choose_wild_pet_wander_target(pet.feet_position);
                pet.last_goal_distance = horizontal_distance(pet.feet_position, pet.wander_target);
            }
        } else if horizontal_distance(pet.feet_position, pet.wander_target)
            <= WILD_PET_TARGET_REACHED_DISTANCE
        {
            pet.idle_timer = wild_pet_idle_duration();
            pet.wander_target = pet.feet_position;
            pet.horizontal_velocity = Vec3::ZERO;
            pet.last_goal_distance = 0.0;
        }

        let to_target = Vec3::new(
            pet.wander_target.x - pet.feet_position.x,
            0.0,
            pet.wander_target.z - pet.feet_position.z,
        );
        let target_distance = to_target.length();
        let desired_velocity =
            if pet.idle_timer > 0.0 || target_distance <= WILD_PET_TARGET_REACHED_DISTANCE {
                Vec3::ZERO
            } else {
                let slow_factor = if target_distance < WILD_PET_SLOW_RADIUS {
                    ((target_distance - WILD_PET_TARGET_REACHED_DISTANCE)
                        / (WILD_PET_SLOW_RADIUS - WILD_PET_TARGET_REACHED_DISTANCE))
                        .clamp(0.0, 1.0)
                } else {
                    1.0
                };
                to_target.normalize_or_zero() * (WILD_PET_ROAM_SPEED * slow_factor)
            };
        let acceleration = if pet.on_ground {
            WILD_PET_ROAM_ACCELERATION
        } else {
            WILD_PET_AIR_ACCELERATION
        };
        pet.horizontal_velocity = move_towards_vec3(
            pet.horizontal_velocity,
            desired_velocity,
            acceleration * dt_secs,
        );
        pet.horizontal_velocity.y = 0.0;

        pet.vertical_velocity -= PET_GRAVITY * dt_secs;
        let previous_feet = pet.feet_position;
        let horizontal_delta = pet.horizontal_velocity * dt_secs;
        self.sweep_collider_axis(
            &mut pet.feet_position,
            horizontal_delta.x,
            Axis::X,
            pet.on_ground,
            PET_COLLIDER,
        );
        self.sweep_collider_axis(
            &mut pet.feet_position,
            horizontal_delta.z,
            Axis::Z,
            pet.on_ground,
            PET_COLLIDER,
        );

        let requested_horizontal = Vec3::new(horizontal_delta.x, 0.0, horizontal_delta.z).length();
        let moved_horizontal = horizontal_distance(previous_feet, pet.feet_position);
        if target_distance > PET_STUCK_DISTANCE
            && requested_horizontal > 0.01
            && moved_horizontal + 0.02 < requested_horizontal
        {
            pet.vertical_velocity = pet.vertical_velocity.max(PET_CLIMB_BOOST_SPEED);
            pet.on_ground = false;
        }

        let moved_vertically = self.sweep_collider_axis(
            &mut pet.feet_position,
            pet.vertical_velocity * dt_secs,
            Axis::Y,
            false,
            PET_COLLIDER,
        );
        if moved_vertically {
            pet.on_ground = false;
        } else {
            if pet.vertical_velocity < 0.0 {
                pet.on_ground = true;
            }
            pet.vertical_velocity = 0.0;
        }

        if pet.horizontal_velocity.length_squared() > 0.0025 {
            pet.yaw = pet.horizontal_velocity.x.atan2(pet.horizontal_velocity.z);
        }

        let next_distance = horizontal_distance(pet.feet_position, pet.wander_target);
        let progress = pet.last_goal_distance - next_distance;
        if pet.idle_timer <= 0.0 && next_distance > PET_STUCK_DISTANCE {
            if progress < PET_STUCK_PROGRESS_EPSILON {
                pet.stuck_timer += dt_secs;
            } else {
                pet.stuck_timer = 0.0;
            }
        } else {
            pet.stuck_timer = 0.0;
        }
        pet.last_goal_distance = next_distance;

        if self.hosted_wild_pet_needs_reset(pet) {
            self.reset_hosted_wild_pet(pet);
        }
    }

    fn update_hosted_wild_pet_reduced(&mut self, pet: &mut HostedWildPetState, dt_secs: f32) {
        if self.hosted_wild_pet_needs_reset(pet) {
            self.reset_hosted_wild_pet(pet);
            return;
        }

        if pet.idle_timer > 0.0 {
            pet.idle_timer = (pet.idle_timer - dt_secs).max(0.0);
            if pet.idle_timer <= 0.0 {
                pet.wander_target = self.choose_wild_pet_wander_target(pet.feet_position);
                pet.last_goal_distance = horizontal_distance(pet.feet_position, pet.wander_target);
            }
        } else if horizontal_distance(pet.feet_position, pet.wander_target)
            <= WILD_PET_TARGET_REACHED_DISTANCE
        {
            pet.idle_timer = wild_pet_idle_duration();
            pet.wander_target = pet.feet_position;
            pet.horizontal_velocity = Vec3::ZERO;
            pet.last_goal_distance = 0.0;
        }

        let to_target = Vec3::new(
            pet.wander_target.x - pet.feet_position.x,
            0.0,
            pet.wander_target.z - pet.feet_position.z,
        );
        let target_distance = to_target.length();
        let desired_velocity =
            if pet.idle_timer > 0.0 || target_distance <= WILD_PET_TARGET_REACHED_DISTANCE {
                Vec3::ZERO
            } else {
                let slow_factor = if target_distance < WILD_PET_SLOW_RADIUS {
                    ((target_distance - WILD_PET_TARGET_REACHED_DISTANCE)
                        / (WILD_PET_SLOW_RADIUS - WILD_PET_TARGET_REACHED_DISTANCE))
                        .clamp(0.0, 1.0)
                } else {
                    1.0
                };
                to_target.normalize_or_zero() * (WILD_PET_ROAM_SPEED * slow_factor)
            };

        pet.horizontal_velocity = move_towards_vec3(
            pet.horizontal_velocity,
            desired_velocity,
            WILD_PET_ROAM_ACCELERATION * dt_secs,
        );
        pet.horizontal_velocity.y = 0.0;
        pet.feet_position += pet.horizontal_velocity * dt_secs;
        pet.vertical_velocity = 0.0;
        pet.on_ground = true;
        pet.stuck_timer = 0.0;

        if pet.horizontal_velocity.length_squared() > 0.0025 {
            pet.yaw = pet.horizontal_velocity.x.atan2(pet.horizontal_velocity.z);
        }

        pet.last_goal_distance = horizontal_distance(pet.feet_position, pet.wander_target);
    }

    fn hosted_wild_pet_needs_reset(&self, pet: &HostedWildPetState) -> bool {
        pet.feet_position.y < PET_FALL_RESET_Y || pet.stuck_timer >= PET_STUCK_TIMEOUT_SECS
    }

    fn reset_hosted_wild_pet(&mut self, pet: &mut HostedWildPetState) {
        let safe_position = if pet.feet_position.y < PET_FALL_RESET_Y {
            self.find_safe_pet_position(pet.spawn_position, pet.spawn_position)
        } else {
            pet.feet_position
        };
        let wander_target = self.choose_wild_pet_wander_target(safe_position);
        *pet = HostedWildPetState::new(
            pet.pet_id,
            pet.spawn_position,
            safe_position,
            wander_target,
            pet.yaw,
        );
    }

    fn choose_wild_pet_wander_target(&mut self, current_position: Vec3) -> Vec3 {
        let angle = (js_sys::Math::random() as f32) * std::f32::consts::TAU;
        let radius = WILD_PET_MIN_WANDER_DISTANCE
            + (js_sys::Math::random() as f32)
                * (WILD_PET_MAX_WANDER_DISTANCE - WILD_PET_MIN_WANDER_DISTANCE);
        current_position + Vec3::new(angle.cos() * radius, 0.0, angle.sin() * radius)
    }

    fn update_pet_follower(
        &mut self,
        pet: &mut PetFollowerState,
        slot_target: Vec3,
        player_feet: Vec3,
        dt_secs: f32,
    ) {
        if self.pet_needs_reset(pet, slot_target) {
            self.reset_pet_follower(pet, slot_target, player_feet);
            return;
        }

        let to_target = Vec3::new(
            slot_target.x - pet.feet_position.x,
            0.0,
            slot_target.z - pet.feet_position.z,
        );
        let slot_distance = to_target.length();
        let desired_velocity = if slot_distance <= PET_STOP_DISTANCE {
            Vec3::ZERO
        } else {
            let slow_factor = if slot_distance < PET_SLOW_RADIUS {
                ((slot_distance - PET_STOP_DISTANCE) / (PET_SLOW_RADIUS - PET_STOP_DISTANCE))
                    .clamp(0.0, 1.0)
            } else {
                1.0
            };
            to_target.normalize_or_zero() * (PET_FOLLOW_SPEED * slow_factor)
        };
        let acceleration = if pet.on_ground {
            PET_FOLLOW_ACCELERATION
        } else {
            PET_AIR_ACCELERATION
        };
        pet.horizontal_velocity = move_towards_vec3(
            pet.horizontal_velocity,
            desired_velocity,
            acceleration * dt_secs,
        );
        pet.horizontal_velocity.y = 0.0;

        pet.vertical_velocity -= PET_GRAVITY * dt_secs;
        let previous_feet = pet.feet_position;
        let horizontal_delta = pet.horizontal_velocity * dt_secs;
        self.sweep_collider_axis(
            &mut pet.feet_position,
            horizontal_delta.x,
            Axis::X,
            pet.on_ground,
            PET_COLLIDER,
        );
        self.sweep_collider_axis(
            &mut pet.feet_position,
            horizontal_delta.z,
            Axis::Z,
            pet.on_ground,
            PET_COLLIDER,
        );

        let requested_horizontal = Vec3::new(horizontal_delta.x, 0.0, horizontal_delta.z).length();
        let moved_horizontal = horizontal_distance(previous_feet, pet.feet_position);
        if slot_distance > PET_STUCK_DISTANCE
            && requested_horizontal > 0.01
            && moved_horizontal + 0.02 < requested_horizontal
        {
            pet.vertical_velocity = pet.vertical_velocity.max(PET_CLIMB_BOOST_SPEED);
            pet.on_ground = false;
        }

        let moved_vertically = self.sweep_collider_axis(
            &mut pet.feet_position,
            pet.vertical_velocity * dt_secs,
            Axis::Y,
            false,
            PET_COLLIDER,
        );
        if moved_vertically {
            pet.on_ground = false;
        } else {
            if pet.vertical_velocity < 0.0 {
                pet.on_ground = true;
            }
            pet.vertical_velocity = 0.0;
        }

        if pet.horizontal_velocity.length_squared() > 0.0025 {
            pet.yaw = pet.horizontal_velocity.x.atan2(pet.horizontal_velocity.z);
        }

        let next_distance = horizontal_distance(pet.feet_position, slot_target);
        let progress = pet.last_slot_distance - next_distance;
        if next_distance > PET_STUCK_DISTANCE {
            if progress < PET_STUCK_PROGRESS_EPSILON {
                pet.stuck_timer += dt_secs;
            } else {
                pet.stuck_timer = 0.0;
            }
        } else {
            pet.stuck_timer = 0.0;
        }
        pet.last_slot_distance = next_distance;

        if self.pet_needs_reset(pet, slot_target) {
            self.reset_pet_follower(pet, slot_target, player_feet);
        }
    }

    fn pet_needs_reset(&self, pet: &PetFollowerState, slot_target: Vec3) -> bool {
        let slot_distance = horizontal_distance(pet.feet_position, slot_target);
        slot_distance > PET_TELEPORT_DISTANCE
            || pet.feet_position.y < PET_FALL_RESET_Y
            || (slot_distance > PET_STUCK_DISTANCE && pet.stuck_timer >= PET_STUCK_TIMEOUT_SECS)
    }

    fn reset_pet_follower(
        &mut self,
        pet: &mut PetFollowerState,
        slot_target: Vec3,
        player_feet: Vec3,
    ) {
        let safe_position = self.find_safe_pet_position(slot_target, player_feet);
        let slot_distance = horizontal_distance(safe_position, slot_target);
        *pet = PetFollowerState::new(safe_position, self.camera.yaw, slot_distance);
    }

    fn find_safe_pet_position(&mut self, slot_target: Vec3, player_feet: Vec3) -> Vec3 {
        const SEARCH_OFFSETS: [(f32, f32); 9] = [
            (0.0, 0.0),
            (0.35, 0.0),
            (-0.35, 0.0),
            (0.0, 0.35),
            (0.0, -0.35),
            (0.35, 0.35),
            (0.35, -0.35),
            (-0.35, 0.35),
            (-0.35, -0.35),
        ];

        for lift in 0..=6 {
            let base_y = player_feet.y + lift as f32 * 0.45;
            for (dx, dz) in SEARCH_OFFSETS {
                let candidate = Vec3::new(slot_target.x + dx, base_y, slot_target.z + dz);
                if !self.collider_collides(candidate, PET_COLLIDER, None) {
                    return candidate;
                }
            }
        }

        player_feet + Vec3::new(0.0, 1.0, 0.0)
    }

    fn camera_matrix(&self) -> Mat4 {
        let aspect = self.size.width as f32 / self.size.height.max(1) as f32;
        self.camera.matrix(aspect, self.render_camera_eye())
    }

    fn world_to_screen_point(&self, position: Vec3) -> Option<(f32, f32)> {
        let clip = self.camera_matrix() * Vec4::new(position.x, position.y, position.z, 1.0);
        if clip.w <= 0.0 {
            return None;
        }

        let ndc = clip.truncate() / clip.w;
        if ndc.z < -1.0 || ndc.z > 1.0 || ndc.x.abs() > 1.05 || ndc.y.abs() > 1.05 {
            return None;
        }

        let (viewport_width, viewport_height) =
            css_viewport_size().unwrap_or((self.size.width as f32, self.size.height as f32));
        let x = (ndc.x * 0.5 + 0.5) * viewport_width;
        let y = (1.0 - (ndc.y * 0.5 + 0.5)) * viewport_height;
        Some((x, y))
    }

    fn update_wild_pet_health_overlay(&self) {
        let mut markup = String::new();
        for pet in self.wild_pets.values() {
            if !self.pet_should_render_impostor(pet.position) {
                continue;
            }
            let Some((screen_x, screen_y)) =
                self.world_to_screen_point(pet.position + Vec3::new(0.0, 1.9, 0.0))
            else {
                continue;
            };

            markup.push_str(&format!(
                "<div style=\"position:fixed;left:{screen_x:.1}px;top:{screen_y:.1}px;transform:translate(-50%,-100%);padding:4px 8px;border-radius:999px;border:1px solid rgba(255,255,255,0.18);background:rgba(11,16,24,0.84);color:#f8df95;box-shadow:0 10px 22px rgba(0,0,0,0.28);font:700 11px/1 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.06em;text-transform:uppercase;white-space:nowrap;\">{}/{} HP</div>",
                pet.health,
                pet.max_health,
            ));
        }

        self.wild_pet_health_overlay.set_inner_html(&markup);
    }

    fn current_target(&mut self) -> Option<RaycastHit> {
        self.raycast_world(6.0)
    }

    fn current_interaction_target(&mut self) -> Option<InteractionTarget> {
        let block_hit = self.current_target();
        let link_hit = self.current_link_target();
        let wild_pet_hit = self.current_wild_pet_target();
        let world_weapon_hit = self.current_world_weapon_target();

        let mut best_target = block_hit.map(InteractionTarget::Block);
        let mut best_distance = block_hit.map(|hit| hit.distance).unwrap_or(f32::INFINITY);

        if let Some(link) = link_hit {
            if link.distance < best_distance {
                best_distance = link.distance;
                best_target = Some(InteractionTarget::Link);
            }
        }

        if let Some(wild_pet) = wild_pet_hit {
            if wild_pet.distance < best_distance {
                best_distance = wild_pet.distance;
                best_target = Some(InteractionTarget::WildPet(wild_pet));
            }
        }

        if let Some(world_weapon) = world_weapon_hit {
            if world_weapon.distance < best_distance {
                best_target = Some(InteractionTarget::WorldWeapon(world_weapon));
            }
        }

        best_target
    }

    fn current_link_target(&self) -> Option<LinkHit> {
        let (origin, direction) = self.interaction_ray()?;
        raycast_link_panel(origin, direction, self.link_panel)
    }

    fn current_wild_pet_target(&self) -> Option<WildPetHit> {
        let (origin, direction) = self.interaction_ray()?;
        let range_origin = self.camera.position;

        let mut best_hit = None;
        for (&pet_id, pet) in &self.wild_pets {
            let to_pet = pet.position - range_origin;
            if to_pet.length_squared()
                > WILD_PET_CAPTURE_TARGET_DISTANCE * WILD_PET_CAPTURE_TARGET_DISTANCE
            {
                continue;
            }
            let min = Vec3::new(
                pet.position.x - WILD_PET_CAPTURE_BOX_RADIUS,
                pet.position.y - WILD_PET_CAPTURE_BOX_FOOT_PADDING,
                pet.position.z - WILD_PET_CAPTURE_BOX_RADIUS,
            );
            let max = Vec3::new(
                pet.position.x + WILD_PET_CAPTURE_BOX_RADIUS,
                pet.position.y + WILD_PET_CAPTURE_BOX_HEIGHT,
                pet.position.z + WILD_PET_CAPTURE_BOX_RADIUS,
            );
            let Some(distance) = ray_aabb_distance(origin, direction, min, max) else {
                continue;
            };
            if best_hit
                .map(|hit: WildPetHit| distance < hit.distance)
                .unwrap_or(true)
            {
                best_hit = Some(WildPetHit { pet_id, distance });
            }
        }

        best_hit
    }

    fn current_world_weapon_target(&self) -> Option<WorldWeaponHit> {
        let (origin, direction) = self.interaction_ray()?;
        let range_origin = self.camera.position;

        let mut best_hit = None;
        for (&weapon_id, weapon) in &self.world_weapons {
            let to_weapon = weapon.position - range_origin;
            if to_weapon.length_squared()
                > WORLD_WEAPON_PICKUP_TARGET_DISTANCE * WORLD_WEAPON_PICKUP_TARGET_DISTANCE
            {
                continue;
            }
            let min = Vec3::new(
                weapon.position.x - WORLD_WEAPON_PICKUP_BOX_RADIUS,
                weapon.position.y - WORLD_WEAPON_PICKUP_BOX_FOOT_PADDING,
                weapon.position.z - WORLD_WEAPON_PICKUP_BOX_RADIUS,
            );
            let max = Vec3::new(
                weapon.position.x + WORLD_WEAPON_PICKUP_BOX_RADIUS,
                weapon.position.y + WORLD_WEAPON_PICKUP_BOX_HEIGHT,
                weapon.position.z + WORLD_WEAPON_PICKUP_BOX_RADIUS,
            );
            let Some(distance) = ray_aabb_distance(origin, direction, min, max) else {
                continue;
            };
            if best_hit
                .map(|hit: WorldWeaponHit| distance < hit.distance)
                .unwrap_or(true)
            {
                best_hit = Some(WorldWeaponHit {
                    weapon_id,
                    distance,
                });
            }
        }

        best_hit
    }

    fn build_crosshair_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        if !self.mouse_captured {
            return None;
        }

        let forward = self.camera.forward();
        let right = Vec3::new(-forward.z, 0.0, forward.x).normalize_or_zero();
        let up = right.cross(forward).normalize_or_zero();
        let scale = if self.is_third_person_active() {
            0.7
        } else {
            1.0
        };
        let center = self.render_camera_eye() + forward * CROSSHAIR_DISTANCE;

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_box_oriented(
            &mut vertices,
            &mut indices,
            center,
            right * (CROSSHAIR_LENGTH * scale),
            up * (CROSSHAIR_THICKNESS * scale),
            forward * (CROSSHAIR_THICKNESS * scale),
            [1.0, 1.0, 1.0],
            (3, 1),
        );
        add_box_oriented(
            &mut vertices,
            &mut indices,
            center,
            right * (CROSSHAIR_THICKNESS * scale),
            up * (CROSSHAIR_LENGTH * scale),
            forward * (CROSSHAIR_THICKNESS * scale),
            [1.0, 1.0, 1.0],
            (3, 1),
        );

        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn build_target_highlight_mesh(&mut self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        match self.current_interaction_target()? {
            InteractionTarget::Block(target) => {
                let face = target.face?;
                add_face_highlight(
                    &mut vertices,
                    &mut indices,
                    target.block,
                    face,
                    TARGET_OUTLINE_THICKNESS,
                    [1.0, 0.95, 0.45],
                    (3, 1),
                );
            }
            InteractionTarget::WildPet(hit) => {
                let pet = self.wild_pets.get(&hit.pet_id)?;
                let min = Vec3::new(
                    pet.position.x - WILD_PET_CAPTURE_BOX_RADIUS,
                    pet.position.y - WILD_PET_CAPTURE_BOX_FOOT_PADDING,
                    pet.position.z - WILD_PET_CAPTURE_BOX_RADIUS,
                ) - Vec3::splat(WILD_PET_TARGET_OUTLINE_PADDING);
                let max = Vec3::new(
                    pet.position.x + WILD_PET_CAPTURE_BOX_RADIUS,
                    pet.position.y + WILD_PET_CAPTURE_BOX_HEIGHT,
                    pet.position.z + WILD_PET_CAPTURE_BOX_RADIUS,
                ) + Vec3::splat(WILD_PET_TARGET_OUTLINE_PADDING);
                add_aabb_outline(
                    &mut vertices,
                    &mut indices,
                    min,
                    max,
                    WILD_PET_TARGET_OUTLINE_THICKNESS,
                    [1.0, 0.58, 0.24],
                    (3, 1),
                );
            }
            InteractionTarget::WorldWeapon(hit) => {
                let weapon = self.world_weapons.get(&hit.weapon_id)?;
                let min = Vec3::new(
                    weapon.position.x - WORLD_WEAPON_PICKUP_BOX_RADIUS,
                    weapon.position.y - WORLD_WEAPON_PICKUP_BOX_FOOT_PADDING,
                    weapon.position.z - WORLD_WEAPON_PICKUP_BOX_RADIUS,
                ) - Vec3::splat(WORLD_WEAPON_TARGET_OUTLINE_PADDING);
                let max = Vec3::new(
                    weapon.position.x + WORLD_WEAPON_PICKUP_BOX_RADIUS,
                    weapon.position.y + WORLD_WEAPON_PICKUP_BOX_HEIGHT,
                    weapon.position.z + WORLD_WEAPON_PICKUP_BOX_RADIUS,
                ) + Vec3::splat(WORLD_WEAPON_TARGET_OUTLINE_PADDING);
                add_aabb_outline(
                    &mut vertices,
                    &mut indices,
                    min,
                    max,
                    WORLD_WEAPON_TARGET_OUTLINE_THICKNESS,
                    [1.0, 0.84, 0.32],
                    (3, 1),
                );
            }
            InteractionTarget::Link => return None,
        }

        (!vertices.is_empty()).then(|| renderer.create_mesh(&vertices, &indices))
    }

    fn build_pet_weapon_shot_mesh(&mut self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let now = Instant::now();
        self.pet_weapon_shots.retain(|shot| shot.expires_at > now);
        if self.pet_weapon_shots.is_empty() {
            return None;
        }

        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        for shot in &self.pet_weapon_shots {
            let progress = normalized_lifetime_progress(shot.started_at, shot.expires_at, now);
            match shot.kind {
                PetWeaponEffectKind::Laser => {
                    add_pet_weapon_laser(&mut vertices, &mut indices, shot.origin, shot.target)
                }
                PetWeaponEffectKind::Gun => add_pet_weapon_projectile(
                    &mut vertices,
                    &mut indices,
                    shot.origin,
                    shot.target,
                    progress,
                ),
                PetWeaponEffectKind::Flamethrower => add_pet_weapon_flame_burst(
                    &mut vertices,
                    &mut indices,
                    shot.origin,
                    shot.target,
                    progress,
                ),
                PetWeaponEffectKind::Sword => add_pet_weapon_sword_throw(
                    &mut vertices,
                    &mut indices,
                    shot.origin,
                    shot.target,
                    progress,
                ),
            }
        }

        (!vertices.is_empty()).then(|| renderer.create_mesh(&vertices, &indices))
    }

    fn build_link_panel_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_link_panel_mesh(
            &mut vertices,
            &mut indices,
            self.link_panel,
            [1.0, 1.0, 1.0],
            LINK_PANEL_TILE,
        );
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
                .current_remote_avatar_url(player_id)
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
            let Some(url) = self.current_remote_avatar_url(player_id) else {
                continue;
            };
            let Some(asset) = self.remote_avatar_assets.get(url) else {
                continue;
            };
            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            let yaw = *self.remote_player_yaws.get(&player_id).unwrap_or(&0.0);
            let playback_time = self
                .remote_player_avatar_states
                .get(&player_id)
                .map(|state| state.playback_time)
                .unwrap_or(0.0);
            let model = Mat4::from_translation(anchor.body)
                * Mat4::from_rotation_y(yaw)
                * asset.model_normalization;
            let joints = evaluate_avatar_skin_matrices(asset, playback_time);
            meshes.push(renderer.create_animated_draw(&asset.mesh, model, &joints));
        }
        meshes
    }

    fn build_local_avatar_mesh_draw(
        &self,
        renderer: &Renderer<'_>,
    ) -> Option<AnimatedMeshDraw<'_>> {
        if !self.should_render_local_avatar() {
            return None;
        }

        let url = self
            .local_avatar_active_url()
            .or_else(|| self.local_avatar_selection()?.first_available_url())?;
        let asset = self.remote_avatar_assets.get(url)?;
        let anchor = player_anchor_from_eye(self.camera.position);
        let model = Mat4::from_translation(anchor.body)
            * Mat4::from_rotation_y(self.avatar_yaw)
            * asset.model_normalization;
        let joints = evaluate_avatar_skin_matrices(asset, self.local_avatar_state.playback_time);
        Some(renderer.create_animated_draw(&asset.mesh, model, &joints))
    }

    fn build_local_avatar_placeholder_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        if !self.should_render_local_avatar() || self.local_avatar_asset_ready() {
            return None;
        }

        let anchor = player_anchor_from_eye(self.camera.position);
        let center = (anchor.body + anchor.head) * 0.5;
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        add_box_oriented(
            &mut vertices,
            &mut indices,
            center,
            Vec3::new(REMOTE_PLAYER_HALF_WIDTH, 0.0, 0.0),
            Vec3::new(0.0, REMOTE_PLAYER_HALF_HEIGHT, 0.0),
            Vec3::new(0.0, 0.0, REMOTE_PLAYER_HALF_WIDTH),
            LOCAL_AVATAR_PLACEHOLDER_TINT,
            (2, 0),
        );
        Some(renderer.create_mesh(&vertices, &indices))
    }

    fn current_remote_avatar_url(&self, player_id: u64) -> Option<&str> {
        let selection = self.remote_player_avatar_selections.get(&player_id)?;
        let animation = self
            .remote_player_avatar_states
            .get(&player_id)
            .map(|state| state.animation)
            .unwrap_or(RemoteAvatarAnimation::Idle);
        selection
            .url_for_animation(animation)
            .or_else(|| selection.first_available_url())
    }

    fn update_remote_avatar_playback(&mut self, dt_secs: f32) {
        let remote_player_ids = self.remote_players.keys().copied().collect::<Vec<_>>();
        for player_id in remote_player_ids {
            let selection = self
                .remote_player_avatar_selections
                .get(&player_id)
                .cloned()
                .unwrap_or_default();
            let speed = self
                .remote_player_velocities
                .get(&player_id)
                .map(|velocity| velocity[0].hypot(velocity[2]))
                .unwrap_or(0.0);
            let moving = speed > REMOTE_AVATAR_RUN_SPEED_THRESHOLD;

            let state = self
                .remote_player_avatar_states
                .entry(player_id)
                .or_default();
            if moving {
                state.time_since_motion = 0.0;
            } else {
                state.time_since_motion += dt_secs;
            }

            let desired_animation = if moving {
                RemoteAvatarAnimation::Run
            } else if state.time_since_motion >= REMOTE_AVATAR_DANCE_DELAY_SECS {
                RemoteAvatarAnimation::Dance
            } else if state.time_since_motion >= REMOTE_AVATAR_IDLE_DELAY_SECS
                || state.animation != RemoteAvatarAnimation::Run
            {
                RemoteAvatarAnimation::Idle
            } else {
                RemoteAvatarAnimation::Run
            };

            let active_url = selection
                .url_for_animation(desired_animation)
                .or_else(|| selection.first_available_url())
                .map(str::to_owned);
            if state.animation != desired_animation || state.active_url != active_url {
                state.animation = desired_animation;
                state.active_url = active_url.clone();
                state.playback_time = 0.0;
            }

            let maybe_duration = active_url
                .as_deref()
                .and_then(|url| self.remote_avatar_assets.get(url))
                .map(|asset| asset.animation.duration_seconds);
            if let Some(duration) = maybe_duration.filter(|duration| *duration > 0.0) {
                state.playback_time = (state.playback_time + dt_secs) % duration;
            }
        }

        self.remote_player_avatar_states
            .retain(|player_id, _| self.remote_players.contains_key(player_id));
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

    fn ensure_remote_avatar_selection_requested(&mut self, selection: &PlayerAvatarSelection) {
        self.ensure_remote_avatar_requested(selection.idle_url());
        self.ensure_remote_avatar_requested(selection.run_url());
        self.ensure_remote_avatar_requested(selection.dance_url());
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
                self.render_camera_eye(),
                [0.14, 0.14, 0.16],
                atlas_quad_raw(REMOTE_MEDIA_PLACEHOLDER_TILE),
            );
        }

        (!vertices.is_empty()).then(|| renderer.create_mesh(&vertices, &indices))
    }

    fn build_remote_media_meshes(&self, renderer: &Renderer<'_>) -> Vec<TexturedMesh> {
        let mut meshes = Vec::new();
        for (&player_id, position) in &self.remote_players {
            let Some(texture) = self
                .remote_media
                .get(&player_id)
                .and_then(|media| media.texture.as_ref())
            else {
                continue;
            };
            let anchor = player_anchor_from_eye(Vec3::new(position[0], position[1], position[2]));
            let mut vertices = Vec::new();
            let mut indices = Vec::new();
            add_media_panel_billboard(
                &mut vertices,
                &mut indices,
                anchor.media,
                self.render_camera_eye(),
                [1.0, 1.0, 1.0],
                full_uv_quad(),
            );
            meshes.push(renderer.create_textured_mesh(&vertices, &indices, texture));
        }
        meshes
    }

    fn ensure_pet_asset_loaded(&mut self, renderer: &Renderer<'_>) {
        let has_local_pets = !self.active_captured_pet_identities().is_empty();
        let has_remote_pets = self
            .remote_pet_states
            .values()
            .any(|pet_states| !pet_states.is_empty());
        let has_wild_pets = !self.wild_pets.is_empty();
        if (!has_local_pets && !has_remote_pets && !has_wild_pets)
            || self.pet_asset.is_some()
            || self.pet_asset_attempted
        {
            return;
        }

        self.pet_asset_attempted = true;
        match load_pet_model_mesh(renderer) {
            Ok(mesh) => {
                self.pet_asset = Some(mesh);
            }
            Err(error) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "failed to load pet model: {error:?}"
                )));
            }
        }
    }

    fn prepare_pet_assets(&mut self, renderer: &Renderer<'_>) {
        self.ensure_pet_asset_loaded(renderer);
        let local_pet_identities = self.active_captured_pet_identities();
        let mut requested_pet_identities = local_pet_identities.clone();
        for identities in self.remote_player_pet_identities.values() {
            requested_pet_identities.extend(identities.iter().cloned());
        }
        requested_pet_identities
            .extend(self.wild_pets.values().map(|pet| pet.pet_identity.clone()));
        self.ensure_pet_identities_requested(&requested_pet_identities);
    }

    fn ensure_pet_identities_requested(&mut self, pet_identities: &[PetIdentity]) {
        for identity in pet_identities {
            let Some(url) = identity.model_url.as_deref() else {
                continue;
            };
            if self.remote_pet_assets.contains_key(url)
                || self.pending_remote_pet_urls.contains(url)
            {
                continue;
            }
            self.pending_remote_pet_urls.insert(url.to_string());
            request_remote_pet_model(url.to_string(), self.remote_pet_tx.clone());
        }
    }

    fn prepare_weapon_assets(&mut self, renderer: &Renderer<'_>) {
        let _ = renderer;
        let mut requested_weapon_identities = self
            .world_weapons
            .values()
            .map(|weapon| weapon.weapon_identity.clone())
            .collect::<Vec<_>>();
        requested_weapon_identities.extend(
            self.active_captured_pet_identities()
                .into_iter()
                .filter_map(|pet| pet.equipped_weapon),
        );
        for identities in self.remote_player_pet_identities.values() {
            requested_weapon_identities.extend(
                identities
                    .iter()
                    .filter_map(|pet| pet.equipped_weapon.clone()),
            );
        }
        self.ensure_weapon_identities_requested(&requested_weapon_identities);
    }

    fn pet_mesh_for_identity<'a>(
        &'a self,
        identity: Option<&PetIdentity>,
    ) -> Option<&'a TexturedMesh> {
        if let Some(url) = identity.and_then(|pet| pet.model_url.as_deref()) {
            if let Some(mesh) = self.remote_pet_assets.get(url) {
                return Some(mesh);
            }
        }

        self.pet_asset.as_ref()
    }

    fn ensure_weapon_identities_requested(&mut self, weapon_identities: &[WeaponIdentity]) {
        for identity in weapon_identities {
            let Some(url) = identity.model_url.as_deref() else {
                continue;
            };
            if self.remote_weapon_assets.contains_key(url)
                || self.pending_remote_weapon_urls.contains(url)
            {
                continue;
            }
            self.pending_remote_weapon_urls.insert(url.to_string());
            request_remote_weapon_model(url.to_string(), self.remote_weapon_tx.clone());
        }
    }

    fn weapon_mesh_for_identity<'a>(
        &'a self,
        identity: Option<&WeaponIdentity>,
    ) -> Option<&'a TexturedMesh> {
        let url = identity?.model_url.as_deref()?;
        self.remote_weapon_assets.get(url)
    }

    fn build_pet_mesh_draws(&self, renderer: &Renderer<'_>) -> Vec<TexturedMeshDraw<'_>> {
        let local_pet_identities = self.active_captured_pet_identities();
        let now = Instant::now();

        let mut draws = Vec::with_capacity(
            self.pet_followers.len()
                + self.remote_pet_states.values().map(Vec::len).sum::<usize>()
                + self.wild_pets.len(),
        );

        for (pet, identity) in self.pet_followers.iter().zip(local_pet_identities.iter()) {
            let Some(pet_asset) = self.pet_mesh_for_identity(Some(identity)) else {
                continue;
            };
            let attack_visual =
                pet_attack_visual(pet.feet_position, pet.yaw, &self.pet_weapon_shots, now);
            let model = pet_model_matrix(pet.feet_position, pet.yaw, attack_visual);
            draws.push(renderer.create_textured_draw(pet_asset, model));
        }

        for (&player_id, pet_states) in &self.remote_pet_states {
            let identities = self.remote_player_pet_identities.get(&player_id);
            for (index, pet) in pet_states.iter().enumerate() {
                let pet_position = Vec3::from_array(pet.position);
                if !self.pet_should_render_model(pet_position) {
                    continue;
                }
                let Some(pet_asset) =
                    self.pet_mesh_for_identity(identities.and_then(|items| items.get(index)))
                else {
                    continue;
                };
                let attack_visual =
                    pet_attack_visual(pet_position, pet.yaw, &self.pet_weapon_shots, now);
                let model = pet_model_matrix(pet_position, pet.yaw, attack_visual);
                draws.push(renderer.create_textured_draw(pet_asset, model));
            }
        }

        for pet in self.wild_pets.values() {
            if !self.pet_should_render_model(pet.position) {
                continue;
            }
            let Some(pet_asset) = self.pet_mesh_for_identity(Some(&pet.pet_identity)) else {
                continue;
            };
            let attack_visual =
                pet_attack_visual(pet.position, pet.yaw, &self.pet_weapon_shots, now);
            let model = pet_model_matrix(pet.position, pet.yaw, attack_visual);
            draws.push(renderer.create_textured_draw(pet_asset, model));
        }

        draws
    }

    fn build_weapon_mesh_draws(&self, renderer: &Renderer<'_>) -> Vec<TexturedMeshDraw<'_>> {
        let local_pet_identities = self.active_captured_pet_identities();
        let now = Instant::now();
        let mut draws = Vec::with_capacity(
            self.world_weapons.len()
                + self.pet_followers.len()
                + self.remote_pet_states.values().map(Vec::len).sum::<usize>(),
        );
        for weapon in self.world_weapons.values() {
            let Some(weapon_asset) = self.weapon_mesh_for_identity(Some(&weapon.weapon_identity))
            else {
                continue;
            };
            let model = Mat4::from_translation(weapon.position);
            draws.push(renderer.create_textured_draw(weapon_asset, model));
        }

        for (index, (pet, identity)) in self
            .pet_followers
            .iter()
            .zip(local_pet_identities.iter())
            .enumerate()
        {
            let Some(weapon_identity) = identity.equipped_weapon.as_ref() else {
                continue;
            };
            let Some(weapon_asset) = self.weapon_mesh_for_identity(Some(weapon_identity)) else {
                continue;
            };
            let attack_visual =
                pet_attack_visual(pet.feet_position, pet.yaw, &self.pet_weapon_shots, now);
            let model = equipped_pet_weapon_model_matrix(
                pet.feet_position,
                pet.yaw,
                index as f32 * 0.61,
                attack_visual,
            );
            draws.push(renderer.create_textured_draw(weapon_asset, model));
        }

        for (&player_id, pet_states) in &self.remote_pet_states {
            let identities = self.remote_player_pet_identities.get(&player_id);
            for (index, pet) in pet_states.iter().enumerate() {
                let pet_position = Vec3::from_array(pet.position);
                if !self.pet_should_render_model(pet_position) {
                    continue;
                }
                let Some(weapon_identity) = identities
                    .and_then(|items| items.get(index))
                    .and_then(|pet| pet.equipped_weapon.as_ref())
                else {
                    continue;
                };
                let Some(weapon_asset) = self.weapon_mesh_for_identity(Some(weapon_identity))
                else {
                    continue;
                };
                let attack_visual =
                    pet_attack_visual(pet_position, pet.yaw, &self.pet_weapon_shots, now);
                let model = equipped_pet_weapon_model_matrix(
                    pet_position,
                    pet.yaw,
                    player_id as f32 * 0.17 + index as f32 * 0.61,
                    attack_visual,
                );
                draws.push(renderer.create_textured_draw(weapon_asset, model));
            }
        }

        draws
    }

    fn refresh_pet_impostor_mesh(&mut self, renderer: &Renderer<'_>) {
        let now = Instant::now();
        if self.pet_impostor_mesh.is_some()
            && now.duration_since(self.last_pet_impostor_update) < PET_IMPOSTOR_REFRESH_INTERVAL
        {
            return;
        }

        self.last_pet_impostor_update = now;
        self.pet_impostor_mesh = self.rebuild_pet_impostor_mesh(renderer);
    }

    fn rebuild_pet_impostor_mesh(&self, renderer: &Renderer<'_>) -> Option<Mesh> {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();

        for pet_states in self.remote_pet_states.values() {
            for pet in pet_states {
                let position = Vec3::from_array(pet.position);
                if !self.pet_should_render_impostor(position)
                    || self.pet_should_render_model(position)
                {
                    continue;
                }
                add_pet_impostor(&mut vertices, &mut indices, position, [0.82, 0.78, 0.68]);
            }
        }

        for pet in self.wild_pets.values() {
            if !self.pet_should_render_impostor(pet.position)
                || self.pet_should_render_model(pet.position)
            {
                continue;
            }
            add_pet_impostor(
                &mut vertices,
                &mut indices,
                pet.position,
                pet_impostor_color(&pet.pet_identity.id),
            );
        }

        (!vertices.is_empty()).then(|| renderer.create_mesh(&vertices, &indices))
    }

    fn chunk_is_visible(&self, position: ChunkPos) -> bool {
        let render_eye = self.render_camera_eye();
        let center = chunk_center(position, render_eye.y);
        let to_chunk = center - render_eye;
        let horizontal = Vec3::new(to_chunk.x, 0.0, to_chunk.z);
        let distance = horizontal.length();
        let max_distance = self.chunk_quality_preset.draw_distance_chunks() * CHUNK_WIDTH as f32;

        if distance > max_distance {
            return false;
        }

        if distance <= CHUNK_NEAR_VISIBILITY_DISTANCE {
            return true;
        }

        let forward = horizontal_forward(self.camera.forward());
        if forward == Vec3::ZERO {
            return true;
        }

        let direction = horizontal / distance.max(0.001);
        let aspect = self.size.width.max(1) as f32 / self.size.height.max(1) as f32;
        let half_vertical_fov = CAMERA_VERTICAL_FOV_RADIANS * 0.5;
        let half_horizontal_fov = (half_vertical_fov.tan() * aspect).atan();
        let chunk_radius = (CHUNK_WIDTH as f32 * 0.5).hypot(CHUNK_DEPTH as f32 * 0.5);
        let angular_padding = (chunk_radius / distance.max(chunk_radius + 0.001)).asin();
        let threshold =
            (half_horizontal_fov + CHUNK_HORIZONTAL_CULL_MARGIN_RADIANS + angular_padding)
                .min(std::f32::consts::PI - 0.01)
                .cos();
        forward.dot(direction) >= threshold
    }

    fn collect_visible_chunk_meshes<'a>(
        &self,
        chunk_meshes: &'a HashMap<ChunkPos, Mesh>,
    ) -> Vec<&'a Mesh> {
        let render_eye = self.render_camera_eye();
        let forward = horizontal_forward(self.camera.forward());
        let mut visible = chunk_meshes
            .iter()
            .filter(|(position, _)| self.chunk_is_visible(**position))
            .collect::<Vec<_>>();

        visible.sort_by(|(a, _), (b, _)| {
            chunk_render_priority(**a, render_eye, forward)
                .total_cmp(&chunk_render_priority(**b, render_eye, forward))
        });

        let max_visible = self.chunk_quality_preset.max_visible_chunk_meshes();
        if visible.len() > max_visible {
            visible.truncate(max_visible);
        }

        visible.into_iter().map(|(_, mesh)| mesh).collect()
    }

    fn world_position_is_visible(&self, position: Vec3, max_distance: f32) -> bool {
        let render_eye = self.render_camera_eye();
        let to_target = position - render_eye;
        let distance = to_target.length();
        if distance > max_distance {
            return false;
        }

        if distance <= CHUNK_WIDTH as f32 {
            return true;
        }

        let direction = to_target / distance.max(0.001);
        let threshold = 0.05 - (16.0 / distance).min(0.2);
        self.camera.forward().dot(direction) >= threshold
    }

    fn pet_should_render_model(&self, position: Vec3) -> bool {
        self.world_position_is_visible(position, PET_MODEL_RENDER_DISTANCE)
    }

    fn pet_should_render_impostor(&self, position: Vec3) -> bool {
        self.world_position_is_visible(position, PET_IMPOSTOR_RENDER_DISTANCE)
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
        let now = Instant::now();
        for media in self.remote_media.values_mut() {
            let (Some(video), Some(canvas), Some(context)) = (
                media.video.as_ref(),
                media.canvas.as_ref(),
                media.context.as_ref(),
            ) else {
                continue;
            };
            if media
                .last_texture_upload_at
                .map(|last| now.duration_since(last) < REMOTE_MEDIA_UPLOAD_INTERVAL)
                .unwrap_or(false)
            {
                continue;
            }
            if video.video_width() == 0 || video.video_height() == 0 {
                continue;
            }
            if media.texture.is_none() {
                media.texture =
                    Some(renderer.create_dynamic_texture(
                        WEBCAM_SOURCE_SIZE as u32,
                        WEBCAM_SOURCE_SIZE as u32,
                    ));
            }

            let width = canvas.width() as f64;
            let height = canvas.height() as f64;
            let _ = context
                .draw_image_with_html_video_element_and_dw_and_dh(video, 0.0, 0.0, width, height);
            let Ok(image_data) = context.get_image_data(0.0, 0.0, width, height) else {
                continue;
            };
            if let Some(texture) = &media.texture {
                renderer.update_dynamic_texture_rgba(texture, &image_data.data().0);
                media.last_texture_upload_at = Some(now);
            }
        }
    }

    fn maybe_enable_peer_media(&mut self, remote_player_id: u64) {
        let Some(local_player_id) = self.player_id else {
            return;
        };
        let Some(remote) = self.remote_media.get_mut(&remote_player_id) else {
            return;
        };
        let mut should_send_offer = false;

        if local_player_id < remote_player_id && remote.data_channel.is_none() {
            let channel = remote.connection.create_data_channel("player-realtime");
            let bindings =
                bind_peer_realtime_channel(remote_player_id, &channel, &self.peer_realtime_tx);
            remote.data_channel = Some(channel);
            remote.data_channel_bindings = Some(bindings);
            if !remote.offer_started {
                remote.offer_started = true;
                should_send_offer = true;
            }
        }

        if let Some(webcam) = self
            .webcam
            .as_ref()
            .filter(|_| !remote.local_tracks_attached)
        {
            let tracks = webcam.stream.get_tracks();
            for index in 0..tracks.length() {
                if let Ok(track) = tracks.get(index).dyn_into::<web_sys::MediaStreamTrack>() {
                    let args = js_sys::Array::new();
                    args.push(&track);
                    args.push(&webcam.stream);
                    if let Ok(add_track) = js_sys::Reflect::get(
                        remote.connection.as_ref(),
                        &JsValue::from_str("addTrack"),
                    ) {
                        if let Ok(add_track) = add_track.dyn_into::<js_sys::Function>() {
                            let _ = add_track.apply(remote.connection.as_ref(), &args);
                        }
                    }
                }
            }
            remote.local_tracks_attached = true;
            remote.needs_media_renegotiation = true;
        }

        if remote.data_channel_open && remote.needs_media_renegotiation {
            remote.needs_media_renegotiation = false;
            should_send_offer = true;
        }

        if should_send_offer {
            spawn_webrtc_offer(
                self.websocket.clone(),
                remote.connection.clone(),
                remote_player_id,
            );
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
            let _ = js_sys::Reflect::set(
                &options,
                &JsValue::from_str("willReadFrequently"),
                &JsValue::TRUE,
            );
            let Ok(Some(context_value)) = canvas.get_context_with_context_options("2d", &options)
            else {
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

        let peer_realtime_tx = self.peer_realtime_tx.clone();
        let ondatachannel = Closure::wrap(Box::new(move |event: RtcDataChannelEvent| {
            let channel = event.channel();
            let bindings =
                bind_peer_realtime_channel(remote_player_id, &channel, &peer_realtime_tx);
            REMOTE_DATA_CHANNEL_REGISTRY.with(|registry| {
                registry.borrow_mut().insert(
                    remote_player_id,
                    RemoteDataChannelRegistration { channel, bindings },
                );
            });
        }) as Box<dyn FnMut(RtcDataChannelEvent)>);
        connection.set_ondatachannel(Some(ondatachannel.as_ref().unchecked_ref()));

        let remote = RemotePeerMedia {
            connection: connection.clone(),
            video: None,
            canvas: None,
            context: None,
            texture: None,
            last_texture_upload_at: None,
            data_channel: None,
            data_channel_open: false,
            data_channel_bindings: None,
            local_tracks_attached: false,
            needs_media_renegotiation: false,
            offer_started: false,
            _onicecandidate: onicecandidate,
            _ontrack: ontrack,
            _ondatachannel: ondatachannel,
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
                    if JsFuture::from(connection.set_remote_description(&description))
                        .await
                        .is_err()
                    {
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
                    if JsFuture::from(connection.set_local_description(&answer_description))
                        .await
                        .is_ok()
                    {
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
                    if JsFuture::from(connection.set_remote_description(&description))
                        .await
                        .is_ok()
                    {
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

        if !self.pending_network_events.is_empty() {
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

        if !self.pending_network_events.is_empty() {
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
        let target_radius = self.chunk_quality_preset.chunk_radius();
        if next_chunk == self.current_chunk && target_radius == self.applied_chunk_radius {
            return;
        }

        let previous_desired = self.desired_chunks.clone();
        self.current_chunk = next_chunk;
        self.applied_chunk_radius = target_radius;
        self.desired_chunks = desired_chunk_set(self.current_chunk, target_radius);
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
        for position in ordered_desired_chunk_positions(self.current_chunk, target_radius) {
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
        let render_eye = self.render_camera_eye();
        pending.sort_by(|a, b| {
            chunk_priority(*a, self.current_chunk, render_eye, forward).total_cmp(&chunk_priority(
                *b,
                self.current_chunk,
                render_eye,
                forward,
            ))
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

    fn update_camera_physics(
        &mut self,
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

        let forward =
            Vec3::new(self.camera.yaw.sin(), 0.0, self.camera.yaw.cos()).normalize_or_zero();
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
        let mut feet_position = self.player_feet_position();
        let previous_feet = feet_position;
        self.sweep_collider_axis(
            &mut feet_position,
            horizontal_delta.x,
            Axis::X,
            self.camera.on_ground,
            PLAYER_COLLIDER,
        );
        self.sweep_collider_axis(
            &mut feet_position,
            horizontal_delta.z,
            Axis::Z,
            self.camera.on_ground,
            PLAYER_COLLIDER,
        );

        let requested_horizontal = Vec3::new(horizontal_delta.x, 0.0, horizontal_delta.z).length();
        let moved_horizontal = horizontal_distance(previous_feet, feet_position);
        if horizontal != Vec3::ZERO
            && requested_horizontal > 0.01
            && moved_horizontal + 0.02 < requested_horizontal
        {
            self.camera.vertical_velocity =
                self.camera.vertical_velocity.max(PLAYER_CLIMB_BOOST_SPEED);
            self.camera.on_ground = false;
        }

        let moved_vertically = self.sweep_collider_axis(
            &mut feet_position,
            self.camera.vertical_velocity * dt_secs,
            Axis::Y,
            false,
            PLAYER_COLLIDER,
        );
        if moved_vertically {
            self.camera.on_ground = false;
        } else {
            if self.camera.vertical_velocity < 0.0 {
                self.camera.on_ground = true;
            }
            self.camera.vertical_velocity = 0.0;
        }

        self.camera.position = feet_position + Vec3::Y * PLAYER_EYE_HEIGHT;
    }

    fn sweep_collider_axis(
        &mut self,
        position: &mut Vec3,
        delta: f32,
        axis: Axis,
        allow_step: bool,
        collider: ColliderSpec,
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

            if self.collider_collides(candidate, collider, None) {
                if allow_step && matches!(axis, Axis::X | Axis::Z) {
                    let mut stepped = candidate;
                    stepped.y += collider.step_height;
                    if !self.collider_collides(stepped, collider, None) {
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
        self.collider_collides(
            eye_position - Vec3::Y * PLAYER_EYE_HEIGHT,
            PLAYER_COLLIDER,
            None,
        )
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

    fn collider_collides(
        &mut self,
        feet_position: Vec3,
        collider: ColliderSpec,
        replace_block: Option<(WorldPos, BlockId)>,
    ) -> bool {
        let min = Vec3::new(
            feet_position.x - collider.radius,
            feet_position.y,
            feet_position.z - collider.radius,
        );
        let max = Vec3::new(
            feet_position.x + collider.radius,
            feet_position.y + collider.height,
            feet_position.z + collider.radius,
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
                    if let Some((position, block)) = replace_block {
                        if i64::from(x) == position.x
                            && y == position.y
                            && i64::from(z) == position.z
                        {
                            if block_is_solid(block) {
                                return true;
                            }
                            continue;
                        }
                    }

                    if self.world_block_is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn player_collides_with_world_pos(
        &mut self,
        eye_position: Vec3,
        position: WorldPos,
        block: BlockId,
    ) -> bool {
        self.collider_collides(
            eye_position - Vec3::Y * PLAYER_EYE_HEIGHT,
            PLAYER_COLLIDER,
            Some((position, block)),
        )
    }

    fn point_collides_with_world(&mut self, position: Vec3, clearance: f32) -> bool {
        let min = Vec3::splat(-clearance) + position;
        let max = Vec3::splat(clearance) + position;

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

    fn raycast_world(&mut self, max_distance: f32) -> Option<RaycastHit> {
        let (origin, direction) = self.interaction_ray()?;
        let player_reach_offset = (self.camera.position - origin).dot(direction).max(0.0);
        let max_ray_distance = max_distance + player_reach_offset;

        let step = 0.1;
        let steps = (max_ray_distance / step).ceil() as usize;
        let mut previous_empty = None;

        for index in 1..=steps {
            let sample = origin + direction * (index as f32 * step);
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
            BlockId::Air => {
                self.send_client_message(&ClientMessage::BreakBlockRequest(BreakBlockRequest {
                    position,
                }))
            }
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

fn load_pet_model_mesh(renderer: &Renderer<'_>) -> Result<TexturedMesh> {
    build_pet_mesh(renderer, PET_MODEL_BYTES)
}

fn build_pet_mesh(renderer: &Renderer<'_>, bytes: &[u8]) -> Result<TexturedMesh> {
    build_static_textured_model_mesh(renderer, bytes, PET_MODEL_DESIRED_HEIGHT, true)
}

fn build_weapon_mesh(renderer: &Renderer<'_>, bytes: &[u8]) -> Result<TexturedMesh> {
    build_static_textured_model_mesh(renderer, bytes, WEAPON_MODEL_DESIRED_SIZE, false)
}

fn pet_model_matrix(pet_feet_position: Vec3, pet_yaw: f32, attack_visual: PetAttackVisual) -> Mat4 {
    Mat4::from_translation(pet_feet_position + attack_visual.offset)
        * Mat4::from_rotation_y(pet_yaw)
        * Mat4::from_rotation_z(attack_visual.roll)
        * Mat4::from_rotation_x(attack_visual.pitch)
}

fn equipped_pet_weapon_model_matrix(
    pet_feet_position: Vec3,
    pet_yaw: f32,
    phase: f32,
    attack_visual: PetAttackVisual,
) -> Mat4 {
    let bob_phase = phase + (js_sys::Date::now() as f32 * 0.004);
    let bob = bob_phase.sin() * 0.08;
    let translation = pet_feet_position + attack_visual.offset + Vec3::new(0.0, 1.55 + bob, 0.0);
    Mat4::from_translation(translation)
        * Mat4::from_rotation_y(pet_yaw + std::f32::consts::FRAC_PI_2)
        * Mat4::from_rotation_z(0.16 + attack_visual.roll * 0.85)
        * Mat4::from_rotation_x(attack_visual.pitch * 0.65)
        * Mat4::from_scale(Vec3::splat(0.5))
}

fn pet_attack_origin(pet_feet_position: Vec3, pet_yaw: f32) -> Vec3 {
    let (forward, _) = horizontal_basis_from_yaw(pet_yaw);
    pet_feet_position
        + Vec3::Y * PET_ATTACK_REACTION_ORIGIN_HEIGHT
        + forward * PET_ATTACK_REACTION_FORWARD_OFFSET
}

fn pet_attack_visual(
    pet_feet_position: Vec3,
    pet_yaw: f32,
    shots: &[PetWeaponShotClientState],
    now: Instant,
) -> PetAttackVisual {
    let expected_origin = pet_attack_origin(pet_feet_position, pet_yaw);
    let max_distance_sq = PET_ATTACK_REACTION_MATCH_RADIUS * PET_ATTACK_REACTION_MATCH_RADIUS;

    let mut best_match = None;
    for shot in shots {
        let elapsed = now.saturating_duration_since(shot.started_at);
        if elapsed >= PET_ATTACK_REACTION_DURATION {
            continue;
        }

        let distance_sq = expected_origin.distance_squared(shot.origin);
        if distance_sq > max_distance_sq {
            continue;
        }

        let score = elapsed.as_secs_f32() * 0.7 + distance_sq;
        if best_match
            .map(|(best_score, _): (f32, &PetWeaponShotClientState)| score < best_score)
            .unwrap_or(true)
        {
            best_match = Some((score, shot));
        }
    }

    let Some((_, shot)) = best_match else {
        return PetAttackVisual::default();
    };

    let progress = (now.saturating_duration_since(shot.started_at).as_secs_f32()
        / PET_ATTACK_REACTION_DURATION.as_secs_f32())
    .clamp(0.0, 1.0);
    let decay = (1.0 - progress).clamp(0.0, 1.0);
    let pulse = (progress * std::f32::consts::PI).sin();
    let horizontal_delta = Vec3::new(
        shot.target.x - shot.origin.x,
        0.0,
        shot.target.z - shot.origin.z,
    );
    let (forward, right) = if horizontal_delta.length_squared() > f32::EPSILON {
        let forward = horizontal_delta.normalize();
        (forward, Vec3::new(-forward.z, 0.0, forward.x))
    } else {
        horizontal_basis_from_yaw(pet_yaw)
    };

    match shot.kind {
        PetWeaponEffectKind::Laser => {
            let jitter = ((progress * std::f32::consts::TAU * 6.0) + 0.35).sin() * decay;
            PetAttackVisual {
                offset: -forward * (0.04 * decay)
                    + right * (0.022 * jitter)
                    + Vec3::Y * (0.024 * pulse),
                pitch: -0.08 * decay,
                roll: 0.05 * jitter,
            }
        }
        PetWeaponEffectKind::Gun => {
            let jitter = ((progress * std::f32::consts::TAU * 8.0) + 0.45).sin() * decay;
            PetAttackVisual {
                offset: -forward * (0.07 * decay)
                    + right * (0.03 * jitter)
                    + Vec3::Y * (0.03 * pulse),
                pitch: -0.12 * decay,
                roll: 0.08 * jitter,
            }
        }
        PetWeaponEffectKind::Flamethrower => {
            let sway = (progress * std::f32::consts::TAU * 4.0).sin() * decay;
            PetAttackVisual {
                offset: forward * (0.045 * pulse)
                    + right * (0.028 * sway)
                    + Vec3::Y * (0.038 * pulse),
                pitch: 0.08 * pulse,
                roll: 0.05 * sway,
            }
        }
        PetWeaponEffectKind::Sword => {
            let lunge = if progress <= 0.45 {
                progress / 0.45
            } else {
                (1.0 - progress) / 0.55
            }
            .clamp(0.0, 1.0);
            let sway = (progress * std::f32::consts::TAU * 3.0).sin() * decay;
            PetAttackVisual {
                offset: forward * (0.14 * lunge) - forward * (0.035 * (1.0 - lunge) * decay)
                    + right * (0.02 * sway)
                    + Vec3::Y * (0.032 * pulse),
                pitch: 0.12 * lunge - 0.04 * (1.0 - lunge) * decay,
                roll: 0.1 * sway,
            }
        }
    }
}

fn build_static_textured_model_mesh(
    renderer: &Renderer<'_>,
    bytes: &[u8],
    desired_size: f32,
    normalize_by_height: bool,
) -> Result<TexturedMesh> {
    let (mut vertices, indices, image) = load_glb_model(bytes)?;
    let (min, max) =
        model_bounds(&vertices).ok_or_else(|| anyhow::anyhow!("model has no vertices"))?;
    let extents = max - min;
    let basis = if normalize_by_height {
        extents.y.max(0.001)
    } else {
        extents.max_element().max(0.001)
    };
    let scale = desired_size / basis;
    let center_x = (min.x + max.x) * 0.5;
    let center_z = (min.z + max.z) * 0.5;

    let vertices = vertices
        .drain(..)
        .map(|vertex| {
            let normalized = Vec3::new(
                vertex.position.x - center_x,
                vertex.position.y - min.y,
                vertex.position.z - center_z,
            );
            Vertex {
                position: (normalized * scale).to_array(),
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

fn load_glb_model(
    bytes: &[u8],
) -> Result<(Vec<ImportedVertex>, Vec<u32>, Option<gltf::image::Data>)> {
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
            let normals = reader
                .read_normals()
                .map(|values| values.collect::<Vec<_>>());
            let texcoords = reader
                .read_tex_coords(0)
                .map(|coords| coords.into_f32().collect::<Vec<_>>());
            let base_vertex = vertices.len() as u32;

            for (index, position) in primitive_positions.iter().enumerate() {
                let world_position = transform.transform_point3(Vec3::from_array(*position));
                let normal = normals
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map(|value| {
                        transform
                            .transform_vector3(Vec3::from_array(*value))
                            .normalize_or_zero()
                    })
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
                indices
                    .extend((0..primitive_positions.len() as u32).map(|index| base_vertex + index));
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
    last_texture_upload_at: Option<Instant>,
    data_channel: Option<RtcDataChannel>,
    data_channel_open: bool,
    data_channel_bindings: Option<PeerRealtimeChannelBindings>,
    local_tracks_attached: bool,
    needs_media_renegotiation: bool,
    offer_started: bool,
    _onicecandidate: Closure<dyn FnMut(RtcPeerConnectionIceEvent)>,
    _ontrack: Closure<dyn FnMut(RtcTrackEvent)>,
    _ondatachannel: Closure<dyn FnMut(RtcDataChannelEvent)>,
}

struct RemoteMediaRegistration {
    video: HtmlVideoElement,
    canvas: HtmlCanvasElement,
    context: CanvasRenderingContext2d,
}

struct PeerRealtimeChannelBindings {
    _onopen: Closure<dyn FnMut(WebEvent)>,
    _onclose: Closure<dyn FnMut(WebEvent)>,
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
}

struct RemoteDataChannelRegistration {
    channel: RtcDataChannel,
    bindings: PeerRealtimeChannelBindings,
}

#[derive(Clone)]
struct PendingIceCandidate {
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
}

thread_local! {
    static REMOTE_MEDIA_REGISTRY: RefCell<HashMap<u64, RemoteMediaRegistration>> = RefCell::new(HashMap::new());
    static REMOTE_DATA_CHANNEL_REGISTRY: RefCell<HashMap<u64, RemoteDataChannelRegistration>> = RefCell::new(HashMap::new());
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
    fn matrix(&self, aspect: f32, eye_position: Vec3) -> Mat4 {
        let look = self.forward();
        let view = Mat4::look_at_rh(eye_position, eye_position + look, Vec3::Y);
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

#[derive(Clone, Copy)]
struct ColliderSpec {
    radius: f32,
    height: f32,
    step_height: f32,
}

const PLAYER_COLLIDER: ColliderSpec = ColliderSpec {
    radius: PLAYER_RADIUS,
    height: PLAYER_HEIGHT,
    step_height: STEP_HEIGHT,
};

const PET_COLLIDER: ColliderSpec = ColliderSpec {
    radius: 0.26,
    height: 1.05,
    step_height: STEP_HEIGHT,
};

#[derive(Clone, Copy)]
struct PetFollowerState {
    feet_position: Vec3,
    horizontal_velocity: Vec3,
    vertical_velocity: f32,
    yaw: f32,
    on_ground: bool,
    last_slot_distance: f32,
    stuck_timer: f32,
}

impl PetFollowerState {
    fn new(feet_position: Vec3, yaw: f32, initial_distance: f32) -> Self {
        Self {
            feet_position,
            horizontal_velocity: Vec3::ZERO,
            vertical_velocity: 0.0,
            yaw,
            on_ground: false,
            last_slot_distance: initial_distance,
            stuck_timer: 0.0,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PetAttackVisual {
    offset: Vec3,
    pitch: f32,
    roll: f32,
}

#[derive(Clone)]
struct WildPetClientState {
    pet_id: u64,
    pet_identity: PetIdentity,
    spawn_position: Vec3,
    position: Vec3,
    velocity: Vec3,
    yaw: f32,
    health: u8,
    max_health: u8,
    host_player_id: Option<u64>,
    latest_tick: u64,
}

impl WildPetClientState {
    fn from_snapshot(snapshot: &WildPetSnapshot) -> Self {
        Self {
            pet_id: snapshot.pet_id,
            pet_identity: snapshot.pet_identity.clone(),
            spawn_position: Vec3::from_array(snapshot.spawn_position),
            position: Vec3::from_array(snapshot.position),
            velocity: Vec3::from_array(snapshot.velocity),
            yaw: snapshot.yaw,
            health: snapshot.health,
            max_health: snapshot.max_health,
            host_player_id: snapshot.host_player_id,
            latest_tick: snapshot.tick,
        }
    }
}

#[derive(Clone)]
struct WorldWeaponClientState {
    weapon_id: u64,
    weapon_identity: WeaponIdentity,
    position: Vec3,
    latest_tick: u64,
}

impl WorldWeaponClientState {
    fn from_snapshot(snapshot: &WorldWeaponSnapshot) -> Self {
        Self {
            weapon_id: snapshot.weapon_id,
            weapon_identity: snapshot.weapon_identity.clone(),
            position: Vec3::from_array(snapshot.position),
            latest_tick: snapshot.tick,
        }
    }
}

#[derive(Clone)]
struct PetWeaponShotClientState {
    kind: PetWeaponEffectKind,
    origin: Vec3,
    target: Vec3,
    started_at: Instant,
    expires_at: Instant,
}

#[derive(Clone, Copy)]
enum PetWeaponEffectKind {
    Laser,
    Gun,
    Flamethrower,
    Sword,
}

impl PetWeaponEffectKind {
    fn from_weapon_kind(kind: &str) -> Self {
        match kind.trim().to_ascii_lowercase().as_str() {
            "gun" => Self::Gun,
            "flamethrower" => Self::Flamethrower,
            "sword" => Self::Sword,
            _ => Self::Laser,
        }
    }

    fn lifetime(self) -> Duration {
        match self {
            Self::Laser => PET_WEAPON_LASER_LIFETIME,
            Self::Gun => PET_WEAPON_GUN_LIFETIME,
            Self::Flamethrower => PET_WEAPON_FLAME_LIFETIME,
            Self::Sword => PET_WEAPON_SWORD_LIFETIME,
        }
    }
}

#[derive(Clone, Copy)]
struct HostedWildPetState {
    pet_id: u64,
    spawn_position: Vec3,
    feet_position: Vec3,
    horizontal_velocity: Vec3,
    vertical_velocity: f32,
    yaw: f32,
    on_ground: bool,
    last_goal_distance: f32,
    stuck_timer: f32,
    wander_target: Vec3,
    idle_timer: f32,
}

impl HostedWildPetState {
    fn new(
        pet_id: u64,
        spawn_position: Vec3,
        feet_position: Vec3,
        wander_target: Vec3,
        yaw: f32,
    ) -> Self {
        Self {
            pet_id,
            spawn_position,
            feet_position,
            horizontal_velocity: Vec3::ZERO,
            vertical_velocity: 0.0,
            yaw,
            on_ground: false,
            last_goal_distance: horizontal_distance(feet_position, wander_target),
            stuck_timer: 0.0,
            wander_target,
            idle_timer: 0.0,
        }
    }

    fn from_client_state(state: &WildPetClientState, wander_target: Vec3) -> Self {
        Self {
            pet_id: state.pet_id,
            spawn_position: state.spawn_position,
            feet_position: state.position,
            horizontal_velocity: Vec3::new(state.velocity.x, 0.0, state.velocity.z),
            vertical_velocity: state.velocity.y,
            yaw: state.yaw,
            on_ground: false,
            last_goal_distance: horizontal_distance(state.position, wander_target),
            stuck_timer: 0.0,
            wander_target,
            idle_timer: 0.0,
        }
    }
}

fn horizontal_basis_from_yaw(yaw: f32) -> (Vec3, Vec3) {
    let forward = Vec3::new(yaw.sin(), 0.0, yaw.cos()).normalize_or_zero();
    let right = Vec3::new(-forward.z, 0.0, forward.x);
    (forward, right)
}

fn horizontal_distance(a: Vec3, b: Vec3) -> f32 {
    Vec3::new(a.x - b.x, 0.0, a.z - b.z).length()
}

fn move_towards_vec3(current: Vec3, target: Vec3, max_delta: f32) -> Vec3 {
    let delta = target - current;
    let distance = delta.length();
    if distance <= max_delta || distance <= f32::EPSILON {
        target
    } else {
        current + delta / distance * max_delta
    }
}

fn move_towards_scalar(current: f32, target: f32, max_delta: f32) -> f32 {
    let delta = target - current;
    if delta.abs() <= max_delta || max_delta <= f32::EPSILON {
        target
    } else {
        current + delta.signum() * max_delta
    }
}

fn angle_delta(target: f32, current: f32) -> f32 {
    let mut delta = target - current;
    while delta > std::f32::consts::PI {
        delta -= std::f32::consts::TAU;
    }
    while delta < -std::f32::consts::PI {
        delta += std::f32::consts::TAU;
    }
    delta
}

fn rotate_angle_towards(current: f32, target: f32, max_delta: f32) -> f32 {
    current + angle_delta(target, current).clamp(-max_delta, max_delta)
}

fn normalize_scroll_delta(delta: MouseScrollDelta) -> f32 {
    match delta {
        MouseScrollDelta::LineDelta(_, y) => y,
        MouseScrollDelta::PixelDelta(position) => (position.y as f32 / 40.0).clamp(-4.0, 4.0),
    }
}

fn wild_pet_idle_duration() -> f32 {
    WILD_PET_IDLE_MIN_SECS
        + (js_sys::Math::random() as f32) * (WILD_PET_IDLE_MAX_SECS - WILD_PET_IDLE_MIN_SECS)
}

fn attach_canvas(canvas: HtmlCanvasElement) {
    let window = web_sys::window().expect("window");
    let document = window.document().expect("document");
    let body = document.body().expect("body");
    let _ = body.append_child(&canvas);
}

fn css_viewport_size() -> Option<(f32, f32)> {
    let window = web_sys::window()?;
    let width = window.inner_width().ok()?.as_f64()? as f32;
    let height = window.inner_height().ok()?.as_f64()? as f32;
    Some((width.max(1.0), height.max(1.0)))
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
    ensure_mouse_lock_prompt_styles(&document);
    let body = document.body().expect("body");
    let prompt = document
        .create_element("button")
        .expect("mouse lock prompt");
    let _ = prompt.set_attribute("type", "button");
    let _ = prompt.set_attribute("aria-live", "polite");
    sync_mouse_lock_prompt(&prompt, false, false);
    let canvas = canvas.clone();
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        canvas.request_pointer_lock();
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = prompt.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    let _ = body.append_child(&prompt);
    (prompt, onclick)
}

fn ensure_mouse_lock_prompt_styles(document: &Document) {
    if document
        .get_element_by_id("augmego-mouse-lock-prompt-style")
        .is_some()
    {
        return;
    }

    let style = document
        .create_element("style")
        .expect("mouse lock prompt style");
    let _ = style.set_attribute("id", "augmego-mouse-lock-prompt-style");
    style.set_text_content(Some(
        "@keyframes augmego-mouse-lock-spin{from{transform:rotate(0deg);}to{transform:rotate(360deg);}}",
    ));

    if let Some(body) = document.body() {
        let _ = body.append_child(&style);
    }
}

fn sync_mouse_lock_prompt(prompt: &Element, spawn_settled: bool, mouse_captured: bool) {
    if mouse_captured {
        let _ = prompt.set_attribute("style", "display:none;");
        return;
    }

    if spawn_settled {
        prompt.set_text_content(Some("Click to lock mouse"));
        let _ = prompt.remove_attribute("disabled");
        let _ = prompt.remove_attribute("aria-busy");
        let _ = prompt.set_attribute("style", mouse_lock_prompt_style(true));
    } else {
        prompt.set_inner_html(mouse_lock_loading_markup());
        let _ = prompt.set_attribute("disabled", "true");
        let _ = prompt.set_attribute("aria-busy", "true");
        let _ = prompt.set_attribute("style", mouse_lock_prompt_style(false));
    }
}

fn mouse_lock_prompt_style(interactive: bool) -> &'static str {
    if interactive {
        "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);display:flex;align-items:center;justify-content:center;gap:12px;min-width:220px;padding:18px 28px;border-radius:18px;border:1px solid rgba(255,255,255,0.28);background:rgba(18,24,32,0.88);color:#f6f8fb;font:600 18px/1.2 ui-sans-serif,system-ui,sans-serif;box-shadow:0 20px 60px rgba(0,0,0,0.35);cursor:pointer;z-index:40;backdrop-filter:blur(10px);"
    } else {
        "position:fixed;left:50%;top:50%;transform:translate(-50%,-50%);display:flex;align-items:center;justify-content:center;gap:12px;min-width:220px;padding:18px 28px;border-radius:18px;border:1px solid rgba(255,255,255,0.28);background:rgba(18,24,32,0.88);color:#f6f8fb;font:600 18px/1.2 ui-sans-serif,system-ui,sans-serif;box-shadow:0 20px 60px rgba(0,0,0,0.35);cursor:progress;z-index:40;backdrop-filter:blur(10px);"
    }
}

fn mouse_lock_loading_markup() -> &'static str {
    "<span aria-hidden=\"true\" style=\"width:18px;height:18px;border:2px solid rgba(246,248,251,0.24);border-top-color:#f6f8fb;border-radius:999px;display:inline-block;animation:augmego-mouse-lock-spin 0.75s linear infinite;\"></span><span>loading</span>"
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

fn create_perf_panel_toggle_button() -> (Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };
    let Some(body) = document.body() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };

    let button = document
        .create_element("button")
        .expect("perf panel toggle button");
    button.set_text_content(Some("FPS"));
    let _ = button.set_attribute("type", "button");
    let _ = button.set_attribute("style", perf_panel_toggle_button_style(false));
    let _ = button.set_attribute("title", "Show performance stats");
    let _ = button.set_attribute("aria-label", "Show performance stats");
    let _ = button.set_attribute("aria-pressed", "false");
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        PERF_PANEL_TOGGLE_QUEUE.with(|queue| {
            let mut pending = queue.borrow_mut();
            *pending = pending.saturating_add(1);
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    let _ = body.append_child(&button);
    (button, onclick)
}

fn create_captured_pets_panel() -> Element {
    let Some(document) = document() else {
        return fallback_element();
    };
    let Some(body) = document.body() else {
        return fallback_element();
    };

    let panel = document
        .create_element("button")
        .expect("captured pets panel");
    let _ = panel.set_attribute("type", "button");
    let _ = panel.set_attribute(
        "style",
        "position:fixed;left:16px;bottom:108px;min-width:136px;padding:12px 18px;border-radius:999px;border:1px solid rgba(255,255,255,0.16);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:30;font:700 13px/1.2 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.12em;text-transform:uppercase;text-align:center;cursor:pointer;",
    );
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        PET_PARTY_MODAL_OPEN_QUEUE.with(|queue| {
            *queue.borrow_mut() = true;
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = panel.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    onclick.forget();
    let _ = body.append_child(&panel);
    panel
}

fn create_weapon_collection_panel() -> Element {
    let Some(document) = document() else {
        return fallback_element();
    };
    let Some(body) = document.body() else {
        return fallback_element();
    };

    let panel = document
        .create_element("div")
        .expect("weapon collection panel");
    let _ = panel.set_attribute(
        "style",
        "position:fixed;left:16px;bottom:168px;width:min(300px,calc(100vw - 32px));max-height:220px;overflow:auto;padding:14px 16px;border-radius:18px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:30;font:600 12px/1.45 ui-sans-serif,system-ui,sans-serif;white-space:pre-line;",
    );
    let _ = body.append_child(&panel);
    panel
}

fn create_wild_pet_health_overlay() -> Element {
    let Some(document) = document() else {
        return fallback_element();
    };
    let Some(body) = document.body() else {
        return fallback_element();
    };

    let overlay = document
        .create_element("div")
        .expect("wild pet health overlay");
    let _ = overlay.set_attribute(
        "style",
        "position:fixed;inset:0;pointer-events:none;z-index:34;",
    );
    let _ = body.append_child(&overlay);
    overlay
}

fn create_pet_party_modal() -> (Element, Element, Element, Element, Element, Element) {
    let Some(document) = document() else {
        return (
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
        );
    };
    let Some(body) = document.body() else {
        return (
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
            fallback_element(),
        );
    };

    let modal = document.create_element("div").expect("pet party modal");
    let _ = modal.set_attribute("style", player_avatar_modal_style());

    let card = document
        .create_element("div")
        .expect("pet party modal card");
    let _ = card.set_attribute(
        "style",
        "position:relative;width:min(440px,calc(100vw - 24px));max-height:min(82vh,760px);overflow:auto;padding:18px;border-radius:20px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.96),rgba(7,11,18,0.96));color:#e6edf3;box-shadow:0 24px 60px rgba(0,0,0,0.38);",
    );

    let close_button = document
        .create_element("button")
        .expect("pet party modal close button");
    close_button.set_text_content(Some("Close"));
    let _ = close_button.set_attribute(
        "style",
        "position:absolute;top:14px;right:14px;padding:8px 12px;border-radius:12px;border:1px solid rgba(255,255,255,0.14);background:rgba(255,255,255,0.08);color:#f2f6fb;font:600 12px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = close_button.set_attribute("type", "button");
    let _ = card.append_child(&close_button);

    let title = document
        .create_element("h2")
        .expect("pet party modal title");
    let _ = title.set_attribute(
        "style",
        "margin:0 48px 6px 0;font:700 18px/1.2 ui-sans-serif,system-ui,sans-serif;",
    );
    title.set_text_content(Some("Pet Party"));
    let _ = card.append_child(&title);

    let copy = document.create_element("p").expect("pet party modal copy");
    let _ = copy.set_attribute(
        "style",
        "margin:0 0 12px 0;color:rgba(230,237,243,0.72);font-size:13px;line-height:1.45;",
    );
    let _ = card.append_child(&copy);

    let count = document
        .create_element("div")
        .expect("pet party selected count");
    let _ = count.set_attribute(
        "style",
        "margin:0 0 12px 0;color:rgba(183,230,255,0.72);font-size:11px;letter-spacing:0.12em;text-transform:uppercase;",
    );
    let _ = card.append_child(&count);

    let actions = document
        .create_element("div")
        .expect("pet party modal actions");
    let _ = actions.set_attribute(
        "style",
        "position:sticky;top:-18px;z-index:2;margin:0 0 14px 0;padding:12px 0 14px 0;background:linear-gradient(180deg,rgba(10,16,24,0.99) 0%,rgba(10,16,24,0.96) 78%,rgba(10,16,24,0.00) 100%);",
    );
    let _ = card.append_child(&actions);

    let save_button = document
        .create_element("button")
        .expect("pet party save button");
    save_button.set_text_content(Some("Save Party"));
    let _ = save_button.set_attribute("type", "button");
    let _ = actions.append_child(&save_button);

    let status = document
        .create_element("p")
        .expect("pet party modal status");
    let _ = status.set_attribute(
        "style",
        "margin:8px 0 0 0;color:rgba(230,237,243,0.72);font-size:12px;line-height:1.45;",
    );
    let _ = actions.append_child(&status);

    let list = document.create_element("div").expect("pet party pet list");
    let _ = list.set_attribute("style", "display:grid;gap:10px;");
    let _ = card.append_child(&list);

    let close_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        PET_PARTY_MODAL_CLOSE_QUEUE.with(|queue| {
            *queue.borrow_mut() = true;
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = close_button
        .add_event_listener_with_callback("click", close_onclick.as_ref().unchecked_ref());
    close_onclick.forget();

    let save_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        PET_PARTY_SAVE_QUEUE.with(|queue| {
            *queue.borrow_mut() = true;
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = save_button
        .add_event_listener_with_callback("click", save_onclick.as_ref().unchecked_ref());
    save_onclick.forget();

    let list_onclick = Closure::wrap(Box::new(move |event: WebEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(target) = target.dyn_into::<Element>() else {
            return;
        };
        let Ok(Some(button)) = target.closest("[data-pet-toggle-id]") else {
            return;
        };
        let Some(pet_id) = button.get_attribute("data-pet-toggle-id") else {
            return;
        };
        PET_PARTY_TOGGLE_QUEUE.with(|queue| {
            queue.borrow_mut().push(pet_id);
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = list.add_event_listener_with_callback("click", list_onclick.as_ref().unchecked_ref());
    list_onclick.forget();

    let list_onchange = Closure::wrap(Box::new(move |event: WebEvent| {
        let Some(target) = event.target() else {
            return;
        };
        let Ok(select) = target.dyn_into::<Element>() else {
            return;
        };
        let Some(pet_id) = select.get_attribute("data-pet-weapon-id") else {
            return;
        };
        let weapon_id = js_sys::Reflect::get(select.as_ref(), &JsValue::from_str("value"))
            .ok()
            .and_then(|value| value.as_string())
            .unwrap_or_default();
        PET_PARTY_WEAPON_ASSIGN_QUEUE.with(|queue| {
            queue
                .borrow_mut()
                .push((pet_id, (!weapon_id.trim().is_empty()).then_some(weapon_id)));
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = list.add_event_listener_with_callback("change", list_onchange.as_ref().unchecked_ref());
    list_onchange.forget();

    let _ = modal.append_child(&card);
    let _ = body.append_child(&modal);
    (modal, copy, count, list, status, save_button)
}

async fn create_pet_party_thumbnail_renderer() -> Result<PetPartyThumbnailRendererState> {
    let Some(document) = document() else {
        anyhow::bail!("document unavailable");
    };
    let Some(body) = document.body() else {
        anyhow::bail!("document body unavailable");
    };
    let canvas = document
        .create_element("canvas")
        .map_err(|error| anyhow::anyhow!("create thumbnail canvas: {error:?}"))?
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| anyhow::anyhow!("cast thumbnail canvas"))?;
    let _ = canvas.set_attribute(
        "style",
        "position:fixed;left:-9999px;top:-9999px;width:88px;height:88px;opacity:0;pointer-events:none;z-index:-1;",
    );
    canvas.set_width(PET_PARTY_THUMBNAIL_SIZE);
    canvas.set_height(PET_PARTY_THUMBNAIL_SIZE);
    let _ = body.append_child(&canvas);
    let renderer = Renderer::new_with_canvas(
        canvas.clone(),
        PhysicalSize::new(PET_PARTY_THUMBNAIL_SIZE, PET_PARTY_THUMBNAIL_SIZE),
    )
    .await?;

    Ok(PetPartyThumbnailRendererState {
        canvas,
        renderer,
        meshes: HashMap::new(),
        image_urls: HashMap::new(),
        failed_keys: HashSet::new(),
    })
}

fn pet_party_thumbnail_camera_matrix() -> Mat4 {
    let eye = Vec3::new(0.0, 1.25, 2.9);
    let target = Vec3::new(0.0, 0.62, 0.0);
    let view = Mat4::look_at_rh(eye, target, Vec3::Y);
    let proj = Mat4::perspective_rh_gl(34.0_f32.to_radians(), 1.0, 0.1, 12.0);
    proj * view
}

fn pet_party_thumbnail_model_matrix() -> Mat4 {
    Mat4::from_rotation_y(0.72) * Mat4::from_rotation_x(-0.1)
}

fn create_perf_panel() -> Element {
    let Some(document) = document() else {
        panic!("document");
    };
    let Some(body) = document.body() else {
        panic!("body");
    };

    let panel = document.create_element("div").expect("perf panel");
    let _ = panel.set_attribute("style", perf_panel_style(false));
    let _ = body.append_child(&panel);
    panel
}

fn perf_panel_style(visible: bool) -> &'static str {
    if visible {
        "position:fixed;top:72px;right:220px;z-index:45;padding:10px 12px;border-radius:12px;background:rgba(7,10,14,0.74);border:1px solid rgba(255,255,255,0.10);color:rgba(233,239,245,0.92);font:12px/1.4 ui-monospace,SFMono-Regular,Menlo,Monaco,Consolas,monospace;white-space:pre-line;pointer-events:none;backdrop-filter:blur(8px);min-width:170px;"
    } else {
        "display:none;"
    }
}

fn perf_panel_toggle_button_style(active: bool) -> &'static str {
    if active {
        "position:fixed;top:16px;right:220px;width:52px;height:52px;border-radius:16px;border:1px solid rgba(183,230,255,0.34);background:linear-gradient(180deg,rgba(28,54,72,0.94),rgba(18,32,44,0.94));color:#f6f8fb;font:700 13px/1 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.08em;box-shadow:0 12px 28px rgba(0,0,0,0.35);cursor:pointer;z-index:46;backdrop-filter:blur(10px);"
    } else {
        "position:fixed;top:16px;right:220px;width:52px;height:52px;border-radius:16px;border:1px solid rgba(255,255,255,0.20);background:rgba(18,24,32,0.88);color:#f6f8fb;font:700 13px/1 ui-sans-serif,system-ui,sans-serif;letter-spacing:0.08em;box-shadow:0 12px 28px rgba(0,0,0,0.35);cursor:pointer;z-index:46;backdrop-filter:blur(10px);"
    }
}

fn create_chunk_quality_button(
    preset: ChunkQualityPreset,
) -> (Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };
    let Some(body) = document.body() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };

    let button = document
        .create_element("button")
        .expect("chunk quality button");
    button.set_text_content(Some(&format!("Chunk Quality: {}", preset.label())));
    let _ = button.set_attribute("type", "button");
    let _ = button.set_attribute(
        "style",
        "position:fixed;left:16px;top:74px;padding:10px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:45;cursor:pointer;font:700 13px/1.2 ui-sans-serif,system-ui,sans-serif;",
    );
    let _ = button.set_attribute("title", "Click to cycle Low, Balanced, High");
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        CHUNK_QUALITY_CYCLE_QUEUE.with(|queue| {
            let mut pending = queue.borrow_mut();
            *pending = pending.saturating_add(1);
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());
    let _ = body.append_child(&button);
    (button, onclick)
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
        "width:min(92vw,460px);padding:28px;border-radius:24px;background:linear-gradient(180deg,rgba(18,24,32,0.92),rgba(8,12,18,0.96));border:1px solid rgba(255,255,255,0.12);box-shadow:0 30px 90px rgba(0,0,0,0.45);color:#f6f8fb;font-family:ui-sans-serif,system-ui,sans-serif;text-align:center;",
    );

    let eyebrow = document.create_element("div").expect("auth eyebrow");
    let _ = eyebrow.set_attribute(
        "style",
        "font-size:11px;letter-spacing:0.22em;text-transform:uppercase;color:rgba(183,230,255,0.72);margin:0 0 10px 0;text-align:center;",
    );
    eyebrow.set_text_content(Some("Augmego Login"));
    let _ = card.append_child(&eyebrow);

    let title = document.create_element("h1").expect("auth title");
    let _ = title.set_attribute(
        "style",
        "margin:0 0 10px 0;font:700 34px/1.05 Georgia,'Times New Roman',serif;text-align:center;",
    );
    title.set_text_content(Some("Enter the shared world"));
    let _ = card.append_child(&title);

    let body_copy = document.create_element("p").expect("auth body");
    let _ = body_copy.set_attribute(
        "style",
        "margin:0 0 18px 0;color:rgba(230,237,243,0.78);font-size:15px;line-height:1.5;text-align:center;",
    );
    body_copy.set_text_content(Some(
        "Sign in with Google, Apple, or Microsoft for saved pets and avatars, or continue as a guest to explore and try temporary pet captures.",
    ));
    let _ = card.append_child(&body_copy);

    let status = document.create_element("p").expect("auth status");
    let _ = status.set_attribute("style", auth_overlay_status_style());
    status.set_text_content(Some(AUTH_STATUS_CHECKING));
    let _ = card.append_child(&status);

    let buttons = document.create_element("div").expect("auth buttons");
    let _ = buttons.set_attribute("style", "display:grid;gap:10px;");

    let mut onclicks = Vec::new();
    for (provider, label) in [
        ("google", "Continue With Google"),
        ("apple", "Continue With Apple"),
        ("microsoft", "Continue With Microsoft"),
    ] {
        let button = document
            .create_element("button")
            .expect("auth provider button");
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

    let guest_button = document
        .create_element("button")
        .expect("auth guest button");
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

    let _ = root.append_child(&card);
    let _ = body.append_child(&root);
    (root, status, onclicks)
}

fn auth_overlay_style() -> &'static str {
    "position:fixed;inset:0;display:grid;place-items:center;padding:24px;background:radial-gradient(circle at top,rgba(62,118,158,0.24),transparent 45%),rgba(5,8,12,0.72);backdrop-filter:blur(10px);z-index:60;"
}

fn auth_overlay_status_style() -> &'static str {
    "margin:0 0 18px 0;color:#f7d794;font-size:14px;line-height:1.4;text-align:center;"
}

fn player_avatar_launcher_style() -> &'static str {
    "position:fixed;left:16px;top:16px;padding:12px 16px;border-radius:16px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:45;cursor:pointer;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;"
}

fn player_avatar_launcher_row_style() -> &'static str {
    "position:fixed;left:16px;top:16px;display:flex;gap:10px;align-items:center;z-index:45;"
}

fn player_avatar_primary_button_style() -> &'static str {
    "padding:12px 16px;border-radius:16px;border:1px solid rgba(12,36,40,0.14);background:linear-gradient(180deg,#f4d58a,#efb96a);color:#13212a;box-shadow:0 18px 44px rgba(0,0,0,0.24);backdrop-filter:blur(10px);cursor:pointer;font:700 14px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;"
}

fn player_avatar_secondary_button_style() -> &'static str {
    "padding:12px 16px;border-radius:16px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.92),rgba(7,11,18,0.92));color:#e6edf3;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);cursor:pointer;font:700 14px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;"
}

fn player_avatar_small_primary_button_style() -> &'static str {
    "width:100%;padding:10px 12px;border-radius:14px;border:1px solid rgba(12,36,40,0.12);background:linear-gradient(180deg,#f4d58a,#efb96a);color:#13212a;font:700 13px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;cursor:pointer;"
}

fn player_avatar_small_secondary_button_style() -> &'static str {
    "width:100%;padding:10px 12px;border-radius:14px;border:1px solid rgba(19,33,42,0.12);background:rgba(19,33,42,0.08);color:#13212a;font:700 13px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;cursor:pointer;"
}

fn logout_button_style() -> &'static str {
    "position:fixed;right:16px;top:16px;padding:12px 16px;border-radius:16px;border:1px solid rgba(247,215,148,0.28);background:linear-gradient(180deg,rgba(77,27,19,0.94),rgba(45,15,11,0.94));color:#f9d9b6;box-shadow:0 18px 44px rgba(0,0,0,0.32);backdrop-filter:blur(10px);z-index:45;cursor:pointer;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;"
}

fn player_avatar_modal_style() -> &'static str {
    "position:fixed;inset:0;display:none;align-items:center;justify-content:center;padding:20px;background:rgba(5,8,12,0.72);backdrop-filter:blur(10px);z-index:65;"
}

fn player_avatar_modal_card_style() -> &'static str {
    "position:relative;width:min(420px,calc(100vw - 24px));max-height:min(82vh,760px);overflow:auto;padding:18px;border-radius:20px;border:1px solid rgba(255,255,255,0.14);background:linear-gradient(180deg,rgba(10,16,24,0.96),rgba(7,11,18,0.96));color:#e6edf3;box-shadow:0 24px 60px rgba(0,0,0,0.38);"
}

fn player_avatar_generation_card_style() -> &'static str {
    "position:relative;width:min(560px,calc(100vw - 24px));max-height:min(88vh,860px);overflow:auto;padding:20px;border-radius:24px;border:1px solid rgba(19,33,42,0.10);background:linear-gradient(180deg,#faf4ea,#f2e4d2);color:#13212a;box-shadow:0 28px 70px rgba(20,12,6,0.24);"
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

fn request_auth_session() -> (Sender<AuthEvent>, Receiver<AuthEvent>) {
    let (tx, rx) = mpsc::channel();
    request_auth_session_refresh(tx.clone());
    (tx, rx)
}

fn request_auth_session_refresh(tx: Sender<AuthEvent>) {
    spawn_local(async move {
        let result = fetch_auth_user()
            .await
            .map_err(|error| format!("Unable to load login session: {error}"));
        let _ = tx.send(AuthEvent::Resolved(result));
    });
}

fn create_logout_button() -> (Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };
    let Some(body) = document.body() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (fallback_element(), closure);
    };

    let button = document.create_element("button").expect("logout button");
    button.set_text_content(Some("Log Out"));
    let _ = button.set_attribute("style", "display:none;");
    let _ = button.set_attribute("type", "button");

    let button_for_click = button.clone();
    let onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        let button = button_for_click.clone();
        spawn_local(async move {
            let is_guest_session =
                button.get_attribute("data-session-kind").as_deref() == Some("guest");
            let _ = button.set_attribute("disabled", "true");
            if is_guest_session {
                button.set_text_content(Some("Exiting guest session..."));
                reload_current_tab();
                return;
            }

            button.set_text_content(Some("Logging out..."));
            match logout_auth_session().await {
                Ok(()) => reload_current_tab(),
                Err(error) => {
                    button.set_text_content(Some("Log Out"));
                    let _ = button.remove_attribute("disabled");
                    if let Some(window) = web_sys::window() {
                        let _ = window.alert_with_message(&format!("Unable to log out: {error}"));
                    }
                }
            }
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());

    let _ = body.append_child(&button);
    (button, onclick)
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

fn load_chunk_quality_preset() -> ChunkQualityPreset {
    browser_local_storage()
        .and_then(|storage| browser_storage_get(&storage, CHUNK_QUALITY_STORAGE_KEY))
        .and_then(|value| ChunkQualityPreset::from_storage_value(&value))
        .unwrap_or(ChunkQualityPreset::Low)
}

fn save_chunk_quality_preset(preset: ChunkQualityPreset) {
    if let Some(storage) = browser_local_storage() {
        let _ = browser_storage_set(&storage, CHUNK_QUALITY_STORAGE_KEY, preset.storage_value());
    }
}

fn browser_local_storage() -> Option<js_sys::Object> {
    let window = web_sys::window()?;
    let storage = js_sys::Reflect::get(window.as_ref(), &JsValue::from_str("localStorage")).ok()?;
    if storage.is_null() || storage.is_undefined() {
        None
    } else {
        storage.dyn_into::<js_sys::Object>().ok()
    }
}

fn browser_storage_get(storage: &js_sys::Object, key: &str) -> Option<String> {
    let get_item = js_sys::Reflect::get(storage.as_ref(), &JsValue::from_str("getItem"))
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    let value = get_item
        .call1(storage.as_ref(), &JsValue::from_str(key))
        .ok()?;
    if value.is_null() || value.is_undefined() {
        None
    } else {
        value.as_string()
    }
}

fn browser_storage_set(storage: &js_sys::Object, key: &str, value: &str) -> Result<()> {
    let set_item = js_sys::Reflect::get(storage.as_ref(), &JsValue::from_str("setItem"))
        .map_err(|error| anyhow::anyhow!("resolve setItem: {error:?}"))?
        .dyn_into::<js_sys::Function>()
        .map_err(|error| anyhow::anyhow!("cast setItem: {error:?}"))?;
    set_item
        .call2(
            storage.as_ref(),
            &JsValue::from_str(key),
            &JsValue::from_str(value),
        )
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("store browser setting: {error:?}"))
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

fn reload_current_tab() {
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
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
        return Err(anyhow::anyhow!(
            "auth endpoint returned HTTP {}",
            response.status()
        ));
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

async fn logout_auth_session() -> Result<()> {
    let init = RequestInit::new();
    init.set_method("POST");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);

    let request =
        Request::new_with_str_and_init(&format!("{}/auth/logout", api_base_url()?), &init)
            .map_err(|error| anyhow::anyhow!("build logout request: {error:?}"))?;
    request
        .headers()
        .set("Accept", "application/json")
        .map_err(|error| anyhow::anyhow!("set logout headers: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("fetch logout session: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert logout response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!(
            "logout endpoint returned HTTP {}",
            response.status()
        ));
    }

    Ok(())
}

fn parse_auth_user(body: &JsValue) -> Option<AuthUser> {
    let user = js_get(body, "user")?;
    Some(AuthUser {
        id: js_get_string(&user, "id")?,
        name: js_get_string(&user, "name"),
        email: js_get_string(&user, "email"),
        avatar_selection: parse_avatar_selection(&user),
        game_auth_token: js_get_string(&user, "gameAuthToken"),
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

fn js_get_i32(value: &JsValue, key: &str) -> Option<i32> {
    js_get(value, key)?
        .as_f64()
        .map(|value| value.round() as i32)
}

fn queue_auth_refresh() {
    AUTH_REFRESH_QUEUE.with(|queue| {
        *queue.borrow_mut() = true;
    });
}

fn parse_avatar_generation_task_response(body: &JsValue) -> Option<AvatarGenerationTaskClient> {
    let task = js_get(body, "task")?;
    parse_avatar_generation_task(&task)
}

fn parse_avatar_generation_task(task: &JsValue) -> Option<AvatarGenerationTaskClient> {
    Some(AvatarGenerationTaskClient {
        id: js_get_string(task, "id")?,
        status: js_get_string(task, "status")?,
        phase: js_get_string(task, "phase")?,
        progress_percent: js_get_i32(task, "progressPercent").unwrap_or_default(),
        message: js_get_string(task, "message").unwrap_or_else(|| "Processing avatar...".into()),
        selfie_url: js_get_string(task, "selfieUrl"),
        portrait_url: js_get_string(task, "portraitUrl"),
        failure_reason: js_get_string(task, "failureReason"),
        avatar_selection: parse_avatar_selection(task),
    })
}

fn avatar_generation_task_is_active(task: &AvatarGenerationTaskClient) -> bool {
    matches!(task.status.as_str(), "QUEUED" | "PROCESSING")
}

fn avatar_generation_step_index(phase: &str) -> usize {
    match phase {
        "UPLOADED" => 0,
        "PORTRAIT_GENERATING" => 1,
        "MESH_GENERATING" => 2,
        "RIGGING_GENERATING" => 3,
        "IDLE_ANIMATING" => 4,
        "RUN_PREPARING" => 5,
        "DANCE_ANIMATING" => 6,
        "FINALIZING" | "READY" => 7,
        "FAILED" => 0,
        _ => 0,
    }
}

fn avatar_generation_step_lines(task: Option<&AvatarGenerationTaskClient>) -> String {
    let steps = [
        "Upload selfie",
        "Generate portrait",
        "Build 3D mesh",
        "Rig character",
        "Create idle animation",
        "Prepare run animation",
        "Create dance animation",
        "Finalize and activate",
    ];
    let current_step = task
        .map(|task| avatar_generation_step_index(&task.phase))
        .unwrap_or(0);
    let is_ready = task.is_some_and(|task| task.status == "READY");
    let is_failed = task.is_some_and(|task| task.status == "FAILED");

    steps
        .iter()
        .enumerate()
        .map(|(index, label)| {
            let marker = if is_ready || index < current_step {
                "[x]"
            } else if is_failed && index == current_step {
                "[!]"
            } else if index == current_step && task.is_some() {
                "[>]"
            } else {
                "[ ]"
            };
            format!("{marker} {label}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn set_element_image_src(image: &Element, url: Option<&str>) {
    match url {
        Some(url) if !url.trim().is_empty() => {
            let _ = image.set_attribute("src", url);
            let _ = image.set_attribute(
                "style",
                "display:block;width:100%;max-height:240px;object-fit:contain;border-radius:16px;border:1px solid rgba(255,255,255,0.12);background:rgba(5,8,12,0.45);",
            );
        }
        _ => {
            let _ = image.remove_attribute("src");
            let _ = image.set_attribute("style", "display:none;");
        }
    }
}

fn set_button_disabled(button: &Element, disabled: bool) {
    if disabled {
        let _ = button.set_attribute("disabled", "true");
    } else {
        let _ = button.remove_attribute("disabled");
    }
}

fn reset_avatar_generation_selfie_state(state: &Rc<RefCell<AvatarGenerationModalState>>) {
    let mut state = state.borrow_mut();
    if let Some(object_url) = state.selfie_object_url.take() {
        let _ = Url::revoke_object_url(&object_url);
    }
    state.selfie_preview_url = None;
    state.selected_selfie_file = None;
}

fn stop_video_stream(video: &HtmlVideoElement) {
    if let Some(stream) = video.src_object() {
        let tracks = stream.get_tracks();
        for index in 0..tracks.length() {
            if let Ok(track) = tracks.get(index).dyn_into::<MediaStreamTrack>() {
                track.stop();
            }
        }
    }
    video.set_src_object(None);
}

async fn start_avatar_generation_camera(video: HtmlVideoElement, status: Element) -> Result<()> {
    stop_video_stream(&video);
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let media_devices = window
        .navigator()
        .media_devices()
        .map_err(|error| anyhow::anyhow!("media devices unavailable: {error:?}"))?;
    let constraints = MediaStreamConstraints::new();
    constraints.set_video(&JsValue::TRUE);
    let stream_value = JsFuture::from(
        media_devices
            .get_user_media_with_constraints(&constraints)
            .map_err(|error| anyhow::anyhow!("getUserMedia failed: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("Camera access was denied: {error:?}"))?;
    let stream: MediaStream = stream_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("camera stream conversion failed"))?;
    video.set_muted(true);
    video.set_autoplay(true);
    let _ = video.set_attribute("playsinline", "true");
    video.set_src_object(Some(&stream));
    if let Ok(play_promise) = video.play() {
        let _ = JsFuture::from(play_promise).await;
    }
    status.set_text_content(Some(
        "Live camera ready. Capture a selfie or choose an image file.",
    ));
    Ok(())
}

fn capture_video_frame_to_data_url(
    video: &HtmlVideoElement,
    canvas: &HtmlCanvasElement,
) -> Result<String> {
    let width = video.video_width().max(1);
    let height = video.video_height().max(1);
    canvas.set_width(width);
    canvas.set_height(height);
    let context = canvas
        .get_context("2d")
        .map_err(|error| anyhow::anyhow!("load capture canvas context: {error:?}"))?
        .ok_or_else(|| anyhow::anyhow!("2d capture context unavailable"))?
        .dyn_into::<CanvasRenderingContext2d>()
        .map_err(|_| anyhow::anyhow!("capture canvas context conversion failed"))?;
    context
        .draw_image_with_html_video_element(video, 0.0, 0.0)
        .map_err(|error| anyhow::anyhow!("draw selfie frame failed: {error:?}"))?;
    canvas
        .to_data_url_with_type("image/png")
        .map_err(|error| anyhow::anyhow!("encode captured selfie: {error:?}"))
}

async fn blob_from_data_url(data_url: &str) -> Result<Blob> {
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_str(data_url))
        .await
        .map_err(|error| anyhow::anyhow!("load selfie preview data: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert selfie preview response"))?;
    let blob_value = JsFuture::from(
        response
            .blob()
            .map_err(|error| anyhow::anyhow!("read selfie preview blob: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("resolve selfie preview blob: {error:?}"))?;
    blob_value
        .dyn_into::<Blob>()
        .map_err(|_| anyhow::anyhow!("convert selfie preview blob"))
}

async fn sleep_ms(ms: i32) -> Result<()> {
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let Some(window) = web_sys::window() else {
            let _ = reject.call1(
                &JsValue::UNDEFINED,
                &JsValue::from_str("window unavailable for timeout"),
            );
            return;
        };
        let resolve = resolve.clone();
        let callback = Closure::once(move || {
            let _ = resolve.call0(&JsValue::UNDEFINED);
        });
        if window
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                ms,
            )
            .is_err()
        {
            let _ = reject.call1(&JsValue::UNDEFINED, &JsValue::from_str("setTimeout failed"));
            return;
        }
        callback.forget();
    });
    JsFuture::from(promise)
        .await
        .map_err(|error| anyhow::anyhow!("sleep failed: {error:?}"))?;
    Ok(())
}

async fn fetch_player_avatar_generation_task() -> Result<Option<AvatarGenerationTaskClient>> {
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);

    let request = Request::new_with_str_and_init(
        &format!("{}/auth/player-avatar/generation", api_base_url()?),
        &init,
    )
    .map_err(|error| anyhow::anyhow!("build avatar generation task request: {error:?}"))?;
    request
        .headers()
        .set("Accept", "application/json")
        .map_err(|error| anyhow::anyhow!("set avatar generation headers: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("load avatar generation task: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert avatar generation task response"))?;
    if !response.ok() {
        return Err(anyhow::anyhow!(
            "avatar generation status request failed with HTTP {}",
            response.status()
        ));
    }
    let body = JsFuture::from(
        response
            .json()
            .map_err(|error| anyhow::anyhow!("read avatar generation body: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse avatar generation body: {error:?}"))?;
    Ok(parse_avatar_generation_task_response(&body))
}

async fn create_player_avatar_generation_task(
    selfie_blob: &Blob,
    file_name: &str,
) -> Result<AvatarGenerationTaskClient> {
    let form_data = FormData::new()
        .map_err(|error| anyhow::anyhow!("create generation form data: {error:?}"))?;
    form_data
        .append_with_blob_and_filename("selfie", selfie_blob, file_name)
        .map_err(|error| anyhow::anyhow!("append selfie to generation form: {error:?}"))?;

    let init = RequestInit::new();
    init.set_method("POST");
    init.set_mode(RequestMode::Cors);
    init.set_credentials(RequestCredentials::Include);
    init.set_body(&JsValue::from(form_data));

    let request = Request::new_with_str_and_init(
        &format!("{}/auth/player-avatar/generation", api_base_url()?),
        &init,
    )
    .map_err(|error| anyhow::anyhow!("build avatar generation create request: {error:?}"))?;

    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("submit avatar generation request: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert avatar generation create response"))?;
    if !response.ok() {
        return Err(anyhow::anyhow!(
            "avatar generation request failed with HTTP {}",
            response.status()
        ));
    }
    let body = JsFuture::from(
        response
            .json()
            .map_err(|error| anyhow::anyhow!("read avatar generation create body: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse avatar generation create body: {error:?}"))?;
    parse_avatar_generation_task_response(&body)
        .ok_or_else(|| anyhow::anyhow!("avatar generation response was missing task details"))
}

fn render_avatar_generation_modal(
    task: Option<&AvatarGenerationTaskClient>,
    state: &Rc<RefCell<AvatarGenerationModalState>>,
    video: &HtmlVideoElement,
    selfie_preview: &Element,
    portrait_preview: &Element,
    progress_fill: &Element,
    progress_label: &Element,
    step_list: &Element,
    status: &Element,
    capture_button: &Element,
    retake_button: &Element,
    submit_button: &Element,
) {
    let (local_selfie_url, has_selected_selfie) = {
        let state = state.borrow();
        (
            state.selfie_preview_url.clone(),
            state.selfie_preview_url.is_some() || state.selected_selfie_file.is_some(),
        )
    };
    let active_task = task.is_some_and(avatar_generation_task_is_active);
    let progress = task
        .map(|task| task.progress_percent.clamp(0, 100))
        .unwrap_or(0);
    let selfie_url = local_selfie_url
        .as_deref()
        .or_else(|| task.and_then(|task| task.selfie_url.as_deref()));
    let portrait_url = task.and_then(|task| task.portrait_url.as_deref());
    let status_text = task
        .and_then(|task| {
            task.failure_reason
                .as_deref()
                .filter(|reason| !reason.trim().is_empty())
                .or(Some(task.message.as_str()))
        })
        .unwrap_or("Capture a selfie, then we’ll generate and activate your avatar set.");

    set_element_image_src(selfie_preview, selfie_url);
    set_element_image_src(portrait_preview, portrait_url);
    let _ = progress_fill.set_attribute(
        "style",
        &format!(
            "height:100%;width:{progress}%;border-radius:999px;background:linear-gradient(90deg,#f5d26b,#ff9c55);transition:width 180ms ease;"
        ),
    );
    progress_label.set_text_content(Some(&format!("{progress}%")));
    step_list.set_text_content(Some(&avatar_generation_step_lines(task)));
    status.set_text_content(Some(status_text));

    if active_task || has_selected_selfie {
        let _ = video.set_attribute("style", "display:none;");
    } else {
        let _ = video.set_attribute(
            "style",
            "display:block;width:100%;min-height:240px;max-height:240px;object-fit:cover;border-radius:16px;border:1px solid rgba(255,255,255,0.12);background:rgba(5,8,12,0.45);",
        );
    }

    set_button_disabled(capture_button, active_task || video.src_object().is_none());
    set_button_disabled(retake_button, active_task || !has_selected_selfie);
    set_button_disabled(submit_button, active_task || !has_selected_selfie);
}

fn start_avatar_generation_polling(
    state: Rc<RefCell<AvatarGenerationModalState>>,
    video: HtmlVideoElement,
    selfie_preview: Element,
    portrait_preview: Element,
    progress_fill: Element,
    progress_label: Element,
    step_list: Element,
    status: Element,
    capture_button: Element,
    retake_button: Element,
    submit_button: Element,
) {
    if state.borrow().polling {
        return;
    }
    state.borrow_mut().polling = true;

    spawn_local(async move {
        loop {
            match fetch_player_avatar_generation_task().await {
                Ok(task) => {
                    if let Some(task) = task.as_ref() {
                        state.borrow_mut().active_task_id = Some(task.id.clone());
                        render_avatar_generation_modal(
                            Some(task),
                            &state,
                            &video,
                            &selfie_preview,
                            &portrait_preview,
                            &progress_fill,
                            &progress_label,
                            &step_list,
                            &status,
                            &capture_button,
                            &retake_button,
                            &submit_button,
                        );
                        if task.status == "READY" && task.avatar_selection.is_some() {
                            queue_auth_refresh();
                        }
                        if !avatar_generation_task_is_active(task) {
                            break;
                        }
                    } else {
                        render_avatar_generation_modal(
                            None,
                            &state,
                            &video,
                            &selfie_preview,
                            &portrait_preview,
                            &progress_fill,
                            &progress_label,
                            &step_list,
                            &status,
                            &capture_button,
                            &retake_button,
                            &submit_button,
                        );
                        break;
                    }
                }
                Err(error) => {
                    status.set_text_content(Some(&format!(
                        "Unable to refresh avatar progress: {error}"
                    )));
                    break;
                }
            }

            if let Err(error) = sleep_ms(2000).await {
                status.set_text_content(Some(&format!("Avatar polling stopped: {error}")));
                break;
            }
        }

        state.borrow_mut().polling = false;
    });
}

fn create_player_avatar_panel() -> (Element, Element, Element, Closure<dyn FnMut(WebEvent)>) {
    let Some(document) = document() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (
            fallback_element(),
            fallback_element(),
            fallback_element(),
            closure,
        );
    };
    let Some(body) = document.body() else {
        let closure =
            Closure::wrap(Box::new(move |_event: WebEvent| {}) as Box<dyn FnMut(WebEvent)>);
        return (
            fallback_element(),
            fallback_element(),
            fallback_element(),
            closure,
        );
    };

    let root = document
        .create_element("div")
        .expect("player avatar launcher row");
    let _ = root.set_attribute("style", "display:none;");
    let generate_launcher = document
        .create_element("button")
        .expect("player avatar generator launcher");
    generate_launcher.set_text_content(Some("Generate Avatar"));
    let _ = generate_launcher.set_attribute("type", "button");
    let _ = generate_launcher.set_attribute("style", player_avatar_primary_button_style());
    let _ = root.append_child(&generate_launcher);

    let manual_launcher = document
        .create_element("button")
        .expect("player avatar manual launcher");
    manual_launcher.set_text_content(Some("Manual Upload"));
    let _ = manual_launcher.set_attribute("type", "button");
    let _ = manual_launcher.set_attribute("style", player_avatar_secondary_button_style());
    let _ = root.append_child(&manual_launcher);

    let modal_root = document
        .create_element("div")
        .expect("player avatar modal root");
    let _ = modal_root.set_attribute("style", "display:block;");

    let generation_modal = document
        .create_element("div")
        .expect("player avatar generation modal");
    let _ = generation_modal.set_attribute("style", player_avatar_modal_style());

    let generation_card = document
        .create_element("div")
        .expect("player avatar generation card");
    let _ = generation_card.set_attribute("style", player_avatar_generation_card_style());

    let generation_close_button = document
        .create_element("button")
        .expect("player avatar generation modal close button");
    generation_close_button.set_text_content(Some("Close"));
    let _ = generation_close_button.set_attribute(
        "style",
        "position:absolute;top:16px;right:16px;padding:8px 12px;border-radius:12px;border:1px solid rgba(16,32,42,0.12);background:rgba(16,32,42,0.08);color:#10202a;font:700 12px/1.2 Georgia,\"Times New Roman\",serif;cursor:pointer;",
    );
    let _ = generation_close_button.set_attribute("type", "button");
    let _ = generation_card.append_child(&generation_close_button);

    let generation_title = document
        .create_element("h2")
        .expect("player avatar generation title");
    generation_title.set_text_content(Some("Camera-First Avatar Generator"));
    let _ = generation_title.set_attribute(
        "style",
        "margin:0 56px 6px 0;color:#13212a;font:700 22px/1.1 Georgia,\"Times New Roman\",serif;",
    );
    let _ = generation_card.append_child(&generation_title);

    let generation_copy = document
        .create_element("p")
        .expect("player avatar generation copy");
    generation_copy.set_text_content(Some(
        "Take a selfie or upload one image. We’ll turn it into a full-body portrait, build the mesh, animate it, optimize the GLBs, and activate the result on your player.",
    ));
    let _ = generation_copy.set_attribute(
        "style",
        "margin:0 0 16px 0;color:rgba(19,33,42,0.78);font:500 13px/1.5 \"Avenir Next\",\"Segoe UI\",sans-serif;",
    );
    let _ = generation_card.append_child(&generation_copy);

    let live_label = document
        .create_element("p")
        .expect("player avatar generation live label");
    live_label.set_text_content(Some("Live Selfie"));
    let _ = live_label.set_attribute(
        "style",
        "margin:0 0 8px 0;color:#10202a;font:700 12px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;letter-spacing:0.04em;text-transform:uppercase;",
    );
    let _ = generation_card.append_child(&live_label);

    let live_video = document
        .create_element("video")
        .expect("player avatar generation video")
        .dyn_into::<HtmlVideoElement>()
        .expect("player avatar generation video cast");
    let _ = live_video.set_attribute(
        "style",
        "display:block;width:100%;min-height:240px;max-height:240px;object-fit:cover;border-radius:16px;border:1px solid rgba(19,33,42,0.14);background:rgba(5,8,12,0.45);",
    );
    let _ = generation_card.append_child(&live_video);

    let capture_canvas = document
        .create_element("canvas")
        .expect("player avatar generation canvas")
        .dyn_into::<HtmlCanvasElement>()
        .expect("player avatar generation canvas cast");
    let _ = capture_canvas.set_attribute("style", "display:none;");
    let _ = generation_card.append_child(&capture_canvas);

    let controls_row = document
        .create_element("div")
        .expect("player avatar generation controls row");
    let _ = controls_row.set_attribute(
        "style",
        "display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:10px;margin-top:12px;",
    );

    let capture_button = document
        .create_element("button")
        .expect("player avatar capture button");
    capture_button.set_text_content(Some("Capture"));
    let _ = capture_button.set_attribute("type", "button");
    let _ = capture_button.set_attribute("style", player_avatar_small_primary_button_style());
    let _ = controls_row.append_child(&capture_button);

    let retake_button = document
        .create_element("button")
        .expect("player avatar retake button");
    retake_button.set_text_content(Some("Retake"));
    let _ = retake_button.set_attribute("type", "button");
    let _ = retake_button.set_attribute("style", player_avatar_small_secondary_button_style());
    let _ = controls_row.append_child(&retake_button);

    let submit_button = document
        .create_element("button")
        .expect("player avatar submit button");
    submit_button.set_text_content(Some("Use Photo"));
    let _ = submit_button.set_attribute("type", "button");
    let _ = submit_button.set_attribute("style", player_avatar_small_primary_button_style());
    let _ = controls_row.append_child(&submit_button);
    let _ = generation_card.append_child(&controls_row);

    let upload_wrapper = document
        .create_element("label")
        .expect("player avatar generation upload wrapper");
    upload_wrapper.set_text_content(Some("Upload Fallback"));
    let _ = upload_wrapper.set_attribute(
        "style",
        "display:grid;gap:6px;margin-top:14px;color:#10202a;font:700 12px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;letter-spacing:0.04em;text-transform:uppercase;",
    );
    let selfie_input = document
        .create_element("input")
        .expect("player avatar selfie input")
        .dyn_into::<HtmlInputElement>()
        .expect("player avatar selfie input cast");
    selfie_input.set_type("file");
    selfie_input.set_accept("image/png,image/jpeg,image/webp");
    let _ = selfie_input.set_attribute(
        "style",
        "display:block;width:100%;padding:10px;border-radius:14px;border:1px solid rgba(19,33,42,0.14);background:rgba(255,255,255,0.7);color:#10202a;font:500 12px/1.3 \"Avenir Next\",\"Segoe UI\",sans-serif;",
    );
    let _ = upload_wrapper.append_child(&selfie_input);
    let _ = generation_card.append_child(&upload_wrapper);

    let preview_grid = document
        .create_element("div")
        .expect("player avatar preview grid");
    let _ = preview_grid.set_attribute(
        "style",
        "display:grid;grid-template-columns:repeat(auto-fit,minmax(180px,1fr));gap:12px;margin-top:16px;",
    );

    let selfie_column = document
        .create_element("div")
        .expect("player avatar selfie preview column");
    let _ = selfie_column.set_attribute("style", "display:grid;gap:8px;");
    let selfie_heading = document
        .create_element("p")
        .expect("player avatar selfie preview heading");
    selfie_heading.set_text_content(Some("Selfie"));
    let _ = selfie_heading.set_attribute(
        "style",
        "margin:0;color:#10202a;font:700 12px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;letter-spacing:0.04em;text-transform:uppercase;",
    );
    let _ = selfie_column.append_child(&selfie_heading);
    let selfie_preview = document
        .create_element("img")
        .expect("player avatar selfie preview");
    let _ = selfie_preview.set_attribute("alt", "Selected selfie preview");
    let _ = selfie_preview.set_attribute("style", "display:none;");
    let _ = selfie_column.append_child(&selfie_preview);
    let _ = preview_grid.append_child(&selfie_column);

    let portrait_column = document
        .create_element("div")
        .expect("player avatar portrait preview column");
    let _ = portrait_column.set_attribute("style", "display:grid;gap:8px;");
    let portrait_heading = document
        .create_element("p")
        .expect("player avatar portrait preview heading");
    portrait_heading.set_text_content(Some("Generated Portrait"));
    let _ = portrait_heading.set_attribute(
        "style",
        "margin:0;color:#10202a;font:700 12px/1.2 \"Avenir Next\",\"Segoe UI\",sans-serif;letter-spacing:0.04em;text-transform:uppercase;",
    );
    let _ = portrait_column.append_child(&portrait_heading);
    let portrait_preview = document
        .create_element("img")
        .expect("player avatar portrait preview");
    let _ = portrait_preview.set_attribute("alt", "Generated portrait preview");
    let _ = portrait_preview.set_attribute("style", "display:none;");
    let _ = portrait_column.append_child(&portrait_preview);
    let _ = preview_grid.append_child(&portrait_column);
    let _ = generation_card.append_child(&preview_grid);

    let progress_row = document
        .create_element("div")
        .expect("player avatar progress row");
    let _ = progress_row.set_attribute(
        "style",
        "display:flex;align-items:center;gap:12px;margin-top:16px;",
    );
    let progress_bar = document
        .create_element("div")
        .expect("player avatar progress bar");
    let _ = progress_bar.set_attribute(
        "style",
        "flex:1;height:12px;border-radius:999px;border:1px solid rgba(19,33,42,0.10);background:rgba(19,33,42,0.08);overflow:hidden;",
    );
    let progress_fill = document
        .create_element("div")
        .expect("player avatar progress fill");
    let _ = progress_fill.set_attribute(
        "style",
        "height:100%;width:0%;border-radius:999px;background:linear-gradient(90deg,#f5d26b,#ff9c55);transition:width 180ms ease;",
    );
    let _ = progress_bar.append_child(&progress_fill);
    let _ = progress_row.append_child(&progress_bar);
    let progress_label = document
        .create_element("strong")
        .expect("player avatar progress label");
    progress_label.set_text_content(Some("0%"));
    let _ = progress_label.set_attribute(
        "style",
        "min-width:46px;color:#10202a;font:700 14px/1 \"Avenir Next\",\"Segoe UI\",sans-serif;text-align:right;",
    );
    let _ = progress_row.append_child(&progress_label);
    let _ = generation_card.append_child(&progress_row);

    let step_list = document
        .create_element("pre")
        .expect("player avatar step list");
    let _ = step_list.set_attribute(
        "style",
        "margin:14px 0 0 0;padding:14px 16px;border-radius:16px;border:1px solid rgba(19,33,42,0.10);background:rgba(255,255,255,0.72);color:#10202a;font:600 12px/1.7 \"Avenir Next\",\"Segoe UI\",sans-serif;white-space:pre-wrap;",
    );
    step_list.set_text_content(Some(&avatar_generation_step_lines(None)));
    let _ = generation_card.append_child(&step_list);

    let generation_status = document
        .create_element("p")
        .expect("player avatar generation status");
    generation_status.set_text_content(Some(
        "Capture a selfie, then we’ll generate and activate your avatar set.",
    ));
    let _ = generation_status.set_attribute(
        "style",
        "margin:14px 0 0 0;color:rgba(19,33,42,0.78);font:500 13px/1.5 \"Avenir Next\",\"Segoe UI\",sans-serif;",
    );
    let _ = generation_card.append_child(&generation_status);

    let _ = generation_modal.append_child(&generation_card);
    let _ = modal_root.append_child(&generation_modal);

    let modal = document.create_element("div").expect("player avatar modal");
    let _ = modal.set_attribute("style", player_avatar_modal_style());

    let card = document
        .create_element("div")
        .expect("player avatar modal card");
    let _ = card.set_attribute("style", player_avatar_modal_card_style());

    let close_button = document
        .create_element("button")
        .expect("player avatar modal close button");
    close_button.set_text_content(Some("Close"));
    let _ = close_button.set_attribute(
        "style",
        "position:absolute;top:14px;right:14px;padding:8px 12px;border-radius:12px;border:1px solid rgba(255,255,255,0.14);background:rgba(255,255,255,0.08);color:#f2f6fb;font:600 12px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = close_button.set_attribute("type", "button");
    let _ = card.append_child(&close_button);

    let title = document
        .create_element("h2")
        .expect("player avatar panel title");
    let _ = title.set_attribute(
        "style",
        "margin:0 48px 6px 0;font:700 18px/1.2 ui-sans-serif,system-ui,sans-serif;",
    );
    title.set_text_content(Some("Player Avatar Animations"));
    let _ = card.append_child(&title);

    let copy = document
        .create_element("p")
        .expect("player avatar panel copy");
    let _ = copy.set_attribute(
        "style",
        "margin:0 0 14px 0;color:rgba(230,237,243,0.72);font-size:13px;line-height:1.45;",
    );
    copy.set_text_content(Some(
        "Manual / advanced path. Upload one GLB each for idle, run, and dance, or paste existing public GLB URLs. Press Esc if the mouse is locked so you can use the form.",
    ));
    let _ = card.append_child(&copy);

    let idle_input =
        create_player_avatar_file_input(&document, &card, "Idle", "player-avatar-idle");
    let run_input = create_player_avatar_file_input(&document, &card, "Run", "player-avatar-run");
    let dance_input =
        create_player_avatar_file_input(&document, &card, "Dance", "player-avatar-dance");

    let divider = document
        .create_element("div")
        .expect("player avatar divider");
    let _ = divider.set_attribute(
        "style",
        "margin:14px 0 10px 0;height:1px;background:rgba(255,255,255,0.10);",
    );
    let _ = card.append_child(&divider);

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
    let _ = card.append_child(&url_copy);

    let idle_url_input =
        create_player_avatar_url_input(&document, &card, "Idle URL", "player-avatar-idle-url");
    let run_url_input =
        create_player_avatar_url_input(&document, &card, "Run URL", "player-avatar-run-url");
    let dance_url_input =
        create_player_avatar_url_input(&document, &card, "Dance URL", "player-avatar-dance-url");

    let upload_button = document
        .create_element("button")
        .expect("player avatar upload button");
    upload_button.set_text_content(Some("Upload Avatar Set"));
    let _ = upload_button.set_attribute(
        "style",
        "margin-top:14px;width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.18);background:linear-gradient(180deg,#f6c665,#e8a93c);color:#1b1206;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = upload_button.set_attribute("type", "button");
    let _ = card.append_child(&upload_button);

    let save_urls_button = document
        .create_element("button")
        .expect("player avatar url save button");
    save_urls_button.set_text_content(Some("Save Avatar URLs"));
    let _ = save_urls_button.set_attribute(
        "style",
        "margin-top:10px;width:100%;padding:12px 14px;border-radius:14px;border:1px solid rgba(255,255,255,0.12);background:rgba(255,255,255,0.08);color:#f2f6fb;font:700 14px/1.2 ui-sans-serif,system-ui,sans-serif;cursor:pointer;",
    );
    let _ = save_urls_button.set_attribute("type", "button");
    let _ = card.append_child(&save_urls_button);

    let status = document
        .create_element("p")
        .expect("player avatar panel status");
    let _ = status.set_attribute(
        "style",
        "margin:12px 0 0 0;color:rgba(230,237,243,0.72);font-size:12px;line-height:1.45;",
    );
    status.set_text_content(Some("Choose three GLBs for idle, run, and dance."));
    let _ = card.append_child(&status);

    let _ = modal.append_child(&card);

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
            if let Err(error) =
                upload_player_avatar_set(&idle_input, &run_input, &dance_input, &status).await
            {
                status.set_text_content(Some(&error.to_string()));
            }
            let _ = upload_button.remove_attribute("disabled");
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ =
        upload_button.add_event_listener_with_callback("click", onclick.as_ref().unchecked_ref());

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
            if let Err(error) =
                save_player_avatar_urls(&idle_url_input, &run_url_input, &dance_url_input, &status)
                    .await
            {
                status.set_text_content(Some(&error.to_string()));
            }
            let _ = save_urls_button.remove_attribute("disabled");
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = save_urls_button
        .add_event_listener_with_callback("click", save_urls_onclick.as_ref().unchecked_ref());
    save_urls_onclick.forget();

    let generation_state = Rc::new(RefCell::new(AvatarGenerationModalState::default()));

    let generation_modal_for_open = generation_modal.clone();
    let generation_state_for_open = generation_state.clone();
    let live_video_for_open = live_video.clone();
    let selfie_preview_for_open = selfie_preview.clone();
    let portrait_preview_for_open = portrait_preview.clone();
    let progress_fill_for_open = progress_fill.clone();
    let progress_label_for_open = progress_label.clone();
    let step_list_for_open = step_list.clone();
    let generation_status_for_open = generation_status.clone();
    let capture_button_for_open = capture_button.clone();
    let retake_button_for_open = retake_button.clone();
    let submit_button_for_open = submit_button.clone();
    let open_generate_modal = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = generation_modal_for_open.set_attribute(
            "style",
            "position:fixed;inset:0;display:flex;align-items:center;justify-content:center;padding:20px;background:rgba(245,239,225,0.72);backdrop-filter:blur(12px);z-index:65;",
        );
        render_avatar_generation_modal(
            None,
            &generation_state_for_open,
            &live_video_for_open,
            &selfie_preview_for_open,
            &portrait_preview_for_open,
            &progress_fill_for_open,
            &progress_label_for_open,
            &step_list_for_open,
            &generation_status_for_open,
            &capture_button_for_open,
            &retake_button_for_open,
            &submit_button_for_open,
        );

        let should_start_camera = {
            let state = generation_state_for_open.borrow();
            state.selfie_preview_url.is_none() && state.selected_selfie_file.is_none()
        };
        if should_start_camera {
            let video = live_video_for_open.clone();
            let status = generation_status_for_open.clone();
            spawn_local(async move {
                if let Err(error) = start_avatar_generation_camera(video, status.clone()).await {
                    status.set_text_content(Some(&format!(
                        "Camera unavailable. Upload a selfie file instead. {error}"
                    )));
                }
            });
        }

        let state = generation_state_for_open.clone();
        let video = live_video_for_open.clone();
        let selfie_preview = selfie_preview_for_open.clone();
        let portrait_preview = portrait_preview_for_open.clone();
        let progress_fill = progress_fill_for_open.clone();
        let progress_label = progress_label_for_open.clone();
        let step_list = step_list_for_open.clone();
        let status = generation_status_for_open.clone();
        let capture_button = capture_button_for_open.clone();
        let retake_button = retake_button_for_open.clone();
        let submit_button = submit_button_for_open.clone();
        spawn_local(async move {
            match fetch_player_avatar_generation_task().await {
                Ok(task) => {
                    if let Some(task) = task.as_ref() {
                        state.borrow_mut().active_task_id = Some(task.id.clone());
                        if avatar_generation_task_is_active(task) {
                            stop_video_stream(&video);
                        }
                        render_avatar_generation_modal(
                            Some(task),
                            &state,
                            &video,
                            &selfie_preview,
                            &portrait_preview,
                            &progress_fill,
                            &progress_label,
                            &step_list,
                            &status,
                            &capture_button,
                            &retake_button,
                            &submit_button,
                        );
                        if avatar_generation_task_is_active(task) {
                            start_avatar_generation_polling(
                                state,
                                video,
                                selfie_preview,
                                portrait_preview,
                                progress_fill,
                                progress_label,
                                step_list,
                                status,
                                capture_button,
                                retake_button,
                                submit_button,
                            );
                        }
                    } else {
                        render_avatar_generation_modal(
                            None,
                            &state,
                            &video,
                            &selfie_preview,
                            &portrait_preview,
                            &progress_fill,
                            &progress_label,
                            &step_list,
                            &status,
                            &capture_button,
                            &retake_button,
                            &submit_button,
                        );
                    }
                }
                Err(error) => {
                    status.set_text_content(Some(&format!(
                        "Unable to load avatar generation progress: {error}"
                    )));
                }
            }
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = generate_launcher
        .add_event_listener_with_callback("click", open_generate_modal.as_ref().unchecked_ref());
    open_generate_modal.forget();

    let modal_for_open = modal.clone();
    let open_modal = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = modal_for_open.set_attribute(
            "style",
            "position:fixed;inset:0;display:flex;align-items:center;justify-content:center;padding:20px;background:rgba(5,8,12,0.72);backdrop-filter:blur(10px);z-index:65;",
        );
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = manual_launcher
        .add_event_listener_with_callback("click", open_modal.as_ref().unchecked_ref());
    open_modal.forget();

    let modal_for_close = modal.clone();
    let close_modal = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = modal_for_close.set_attribute("style", player_avatar_modal_style());
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = close_button
        .add_event_listener_with_callback("click", close_modal.as_ref().unchecked_ref());
    close_modal.forget();

    let generation_modal_for_close = generation_modal.clone();
    let generation_video_for_close = live_video.clone();
    let close_generate_modal = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = generation_modal_for_close.set_attribute("style", player_avatar_modal_style());
        stop_video_stream(&generation_video_for_close);
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = generation_close_button
        .add_event_listener_with_callback("click", close_generate_modal.as_ref().unchecked_ref());
    close_generate_modal.forget();

    let capture_canvas_for_click = capture_canvas.clone();
    let generation_state_for_capture = generation_state.clone();
    let live_video_for_capture = live_video.clone();
    let selfie_preview_for_capture = selfie_preview.clone();
    let portrait_preview_for_capture = portrait_preview.clone();
    let progress_fill_for_capture = progress_fill.clone();
    let progress_label_for_capture = progress_label.clone();
    let step_list_for_capture = step_list.clone();
    let generation_status_for_capture = generation_status.clone();
    let capture_button_for_capture = capture_button.clone();
    let retake_button_for_capture = retake_button.clone();
    let submit_button_for_capture = submit_button.clone();
    let capture_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        match capture_video_frame_to_data_url(&live_video_for_capture, &capture_canvas_for_click) {
            Ok(data_url) => {
                reset_avatar_generation_selfie_state(&generation_state_for_capture);
                {
                    let mut state = generation_state_for_capture.borrow_mut();
                    state.active_task_id = None;
                    state.selfie_preview_url = Some(data_url);
                }
                stop_video_stream(&live_video_for_capture);
                render_avatar_generation_modal(
                    None,
                    &generation_state_for_capture,
                    &live_video_for_capture,
                    &selfie_preview_for_capture,
                    &portrait_preview_for_capture,
                    &progress_fill_for_capture,
                    &progress_label_for_capture,
                    &step_list_for_capture,
                    &generation_status_for_capture,
                    &capture_button_for_capture,
                    &retake_button_for_capture,
                    &submit_button_for_capture,
                );
                generation_status_for_capture
                    .set_text_content(Some("Photo captured. Use Photo to start generation."));
            }
            Err(error) => {
                generation_status_for_capture
                    .set_text_content(Some(&format!("Capture failed: {error}")));
            }
        }
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = capture_button
        .add_event_listener_with_callback("click", capture_onclick.as_ref().unchecked_ref());
    capture_onclick.forget();

    let generation_state_for_retake = generation_state.clone();
    let selfie_input_for_retake = selfie_input.clone();
    let live_video_for_retake = live_video.clone();
    let selfie_preview_for_retake = selfie_preview.clone();
    let portrait_preview_for_retake = portrait_preview.clone();
    let progress_fill_for_retake = progress_fill.clone();
    let progress_label_for_retake = progress_label.clone();
    let step_list_for_retake = step_list.clone();
    let generation_status_for_retake = generation_status.clone();
    let capture_button_for_retake = capture_button.clone();
    let retake_button_for_retake = retake_button.clone();
    let submit_button_for_retake = submit_button.clone();
    let retake_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        reset_avatar_generation_selfie_state(&generation_state_for_retake);
        generation_state_for_retake.borrow_mut().active_task_id = None;
        selfie_input_for_retake.set_value("");
        render_avatar_generation_modal(
            None,
            &generation_state_for_retake,
            &live_video_for_retake,
            &selfie_preview_for_retake,
            &portrait_preview_for_retake,
            &progress_fill_for_retake,
            &progress_label_for_retake,
            &step_list_for_retake,
            &generation_status_for_retake,
            &capture_button_for_retake,
            &retake_button_for_retake,
            &submit_button_for_retake,
        );
        let video = live_video_for_retake.clone();
        let status = generation_status_for_retake.clone();
        spawn_local(async move {
            if let Err(error) = start_avatar_generation_camera(video, status.clone()).await {
                status.set_text_content(Some(&format!(
                    "Camera unavailable. Upload a selfie file instead. {error}"
                )));
            }
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = retake_button
        .add_event_listener_with_callback("click", retake_onclick.as_ref().unchecked_ref());
    retake_onclick.forget();

    let generation_state_for_file = generation_state.clone();
    let live_video_for_file = live_video.clone();
    let selfie_preview_for_file = selfie_preview.clone();
    let portrait_preview_for_file = portrait_preview.clone();
    let progress_fill_for_file = progress_fill.clone();
    let progress_label_for_file = progress_label.clone();
    let step_list_for_file = step_list.clone();
    let generation_status_for_file = generation_status.clone();
    let capture_button_for_file = capture_button.clone();
    let retake_button_for_file = retake_button.clone();
    let submit_button_for_file = submit_button.clone();
    let selfie_input_for_change = selfie_input.clone();
    let file_onchange = Closure::wrap(Box::new(move |_event: WebEvent| {
        let Some(file) = input_selected_file(&selfie_input_for_change) else {
            return;
        };
        reset_avatar_generation_selfie_state(&generation_state_for_file);
        stop_video_stream(&live_video_for_file);
        match Url::create_object_url_with_blob(&file) {
            Ok(object_url) => {
                {
                    let mut state = generation_state_for_file.borrow_mut();
                    state.active_task_id = None;
                    state.selfie_object_url = Some(object_url.clone());
                    state.selfie_preview_url = Some(object_url);
                    state.selected_selfie_file = Some(file);
                }
                render_avatar_generation_modal(
                    None,
                    &generation_state_for_file,
                    &live_video_for_file,
                    &selfie_preview_for_file,
                    &portrait_preview_for_file,
                    &progress_fill_for_file,
                    &progress_label_for_file,
                    &step_list_for_file,
                    &generation_status_for_file,
                    &capture_button_for_file,
                    &retake_button_for_file,
                    &submit_button_for_file,
                );
                generation_status_for_file
                    .set_text_content(Some("Selfie selected. Use Photo to start generation."));
            }
            Err(error) => {
                generation_status_for_file
                    .set_text_content(Some(&format!("Preview failed: {error:?}")));
            }
        }
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = selfie_input
        .add_event_listener_with_callback("change", file_onchange.as_ref().unchecked_ref());
    file_onchange.forget();

    let generation_state_for_submit = generation_state.clone();
    let live_video_for_submit = live_video.clone();
    let selfie_preview_for_submit = selfie_preview.clone();
    let portrait_preview_for_submit = portrait_preview.clone();
    let progress_fill_for_submit = progress_fill.clone();
    let progress_label_for_submit = progress_label.clone();
    let step_list_for_submit = step_list.clone();
    let generation_status_for_submit = generation_status.clone();
    let capture_button_for_submit = capture_button.clone();
    let retake_button_for_submit = retake_button.clone();
    let submit_button_for_submit = submit_button.clone();
    let submit_onclick = Closure::wrap(Box::new(move |_event: WebEvent| {
        let state = generation_state_for_submit.clone();
        let video = live_video_for_submit.clone();
        let selfie_preview = selfie_preview_for_submit.clone();
        let portrait_preview = portrait_preview_for_submit.clone();
        let progress_fill = progress_fill_for_submit.clone();
        let progress_label = progress_label_for_submit.clone();
        let step_list = step_list_for_submit.clone();
        let status = generation_status_for_submit.clone();
        let capture_button = capture_button_for_submit.clone();
        let retake_button = retake_button_for_submit.clone();
        let submit_button = submit_button_for_submit.clone();
        spawn_local(async move {
            set_button_disabled(&submit_button, true);
            status.set_text_content(Some("Uploading selfie and starting avatar generation..."));

            let selected_file = state.borrow().selected_selfie_file.clone();
            let selected_preview_url = state.borrow().selfie_preview_url.clone();
            let generation_input = if let Some(file) = selected_file {
                let file_name = file.name();
                let blob: Blob = file.unchecked_into();
                Ok((blob, file_name))
            } else if let Some(data_url) = selected_preview_url {
                blob_from_data_url(&data_url)
                    .await
                    .map(|blob| (blob, "selfie.png".to_string()))
            } else {
                Err(anyhow::anyhow!("Capture or choose a selfie first."))
            };

            let task_result = match generation_input {
                Ok((blob, file_name)) => {
                    create_player_avatar_generation_task(&blob, &file_name).await
                }
                Err(error) => Err(error),
            };

            match task_result {
                Ok(task) => {
                    state.borrow_mut().active_task_id = Some(task.id.clone());
                    stop_video_stream(&video);
                    render_avatar_generation_modal(
                        Some(&task),
                        &state,
                        &video,
                        &selfie_preview,
                        &portrait_preview,
                        &progress_fill,
                        &progress_label,
                        &step_list,
                        &status,
                        &capture_button,
                        &retake_button,
                        &submit_button,
                    );
                    start_avatar_generation_polling(
                        state,
                        video,
                        selfie_preview,
                        portrait_preview,
                        progress_fill,
                        progress_label,
                        step_list,
                        status,
                        capture_button,
                        retake_button,
                        submit_button,
                    );
                }
                Err(error) => {
                    status.set_text_content(Some(&format!(
                        "Avatar generation failed to start: {error}"
                    )));
                    render_avatar_generation_modal(
                        None,
                        &state,
                        &video,
                        &selfie_preview,
                        &portrait_preview,
                        &progress_fill,
                        &progress_label,
                        &step_list,
                        &status,
                        &capture_button,
                        &retake_button,
                        &submit_button,
                    );
                }
            }
        });
    }) as Box<dyn FnMut(WebEvent)>);
    let _ = submit_button
        .add_event_listener_with_callback("click", submit_onclick.as_ref().unchecked_ref());
    submit_onclick.forget();

    let _ = body.append_child(&root);
    let _ = modal_root.append_child(&modal);
    let _ = body.append_child(&modal_root);
    (root, modal_root, status, onclick)
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
        return Err(anyhow::anyhow!("Choose at least one GLB before uploading."));
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
    queue_auth_refresh();
    status.set_text_content(Some("Avatar upload complete."));
    Ok(())
}

async fn upload_player_avatar_slot(slot: &str, file: &web_sys::File) -> Result<Option<String>> {
    if let Ok(public_url) = upload_player_avatar_slot_direct(slot, file).await {
        return Ok(Some(public_url));
    }

    let form_data =
        FormData::new().map_err(|error| anyhow::anyhow!("create form data: {error:?}"))?;
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
    js_sys::Reflect::set(
        &payload,
        &JsValue::from_str("slot"),
        &JsValue::from_str(slot),
    )
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
    queue_auth_refresh();
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

    let request =
        Request::new_with_str_and_init(&format!("{}/auth/player-avatar", api_base_url()?), &init)
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
        let result = fetch_remote_model_bytes(&url).await;
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

fn request_remote_pet_model(url: String, sender: Sender<RemotePetEvent>) {
    spawn_local(async move {
        let result = fetch_remote_model_bytes(&url).await;
        let event = match result {
            Ok(bytes) => RemotePetEvent::Loaded { url, bytes },
            Err(error) => RemotePetEvent::Failed {
                url,
                message: error.to_string(),
            },
        };
        let _ = sender.send(event);
    });
}

fn request_remote_weapon_model(url: String, sender: Sender<RemoteWeaponEvent>) {
    spawn_local(async move {
        let result = fetch_remote_model_bytes(&url).await;
        let event = match result {
            Ok(bytes) => RemoteWeaponEvent::Loaded { url, bytes },
            Err(error) => RemoteWeaponEvent::Failed {
                url,
                message: error.to_string(),
            },
        };
        let _ = sender.send(event);
    });
}

async fn fetch_remote_model_bytes(url: &str) -> Result<Vec<u8>> {
    let init = RequestInit::new();
    init.set_method("GET");
    init.set_mode(RequestMode::Cors);

    let request = Request::new_with_str_and_init(url, &init)
        .map_err(|error| anyhow::anyhow!("build remote model request: {error:?}"))?;
    let window = web_sys::window().ok_or_else(|| anyhow::anyhow!("window unavailable"))?;
    let response_value = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(|error| anyhow::anyhow!("fetch remote model bytes: {error:?}"))?;
    let response: Response = response_value
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("convert remote model response"))?;

    if !response.ok() {
        return Err(anyhow::anyhow!(
            "remote model request returned HTTP {}",
            response.status()
        ));
    }

    let buffer = JsFuture::from(
        response
            .array_buffer()
            .map_err(|error| anyhow::anyhow!("read remote model bytes: {error:?}"))?,
    )
    .await
    .map_err(|error| anyhow::anyhow!("parse remote model bytes: {error:?}"))?;

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
            let joint_nodes = node_skin
                .joints()
                .map(|joint| joint.index())
                .collect::<Vec<_>>();
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
            != node_skin
                .joints()
                .map(|joint| joint.index())
                .collect::<Vec<_>>()
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
            let normals = reader
                .read_normals()
                .map(|values| values.collect::<Vec<_>>());
            let texcoords = reader
                .read_tex_coords(0)
                .map(|coords| coords.into_f32().collect::<Vec<_>>());
            let base_vertex = vertices.len() as u32;

            for index in 0..primitive_positions.len() {
                let mut weight_values = primitive_weights
                    .get(index)
                    .copied()
                    .unwrap_or([1.0, 0.0, 0.0, 0.0]);
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
                indices
                    .extend((0..primitive_positions.len() as u32).map(|index| base_vertex + index));
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
    let normalized_corners = [
        Vec3::new(min.x, min.y, min.z),
        Vec3::new(min.x, min.y, max.z),
        Vec3::new(min.x, max.y, min.z),
        Vec3::new(min.x, max.y, max.z),
        Vec3::new(max.x, min.y, min.z),
        Vec3::new(max.x, min.y, max.z),
        Vec3::new(max.x, max.y, min.z),
        Vec3::new(max.x, max.y, max.z),
    ]
    .to_vec();
    let mut normalized_min = normalized_corners[0];
    let mut normalized_max = normalized_corners[0];
    for corner in &normalized_corners[1..] {
        normalized_min = normalized_min.min(*corner);
        normalized_max = normalized_max.max(*corner);
    }

    let scale = PLAYER_HEIGHT / (normalized_max.y - normalized_min.y).max(0.001);
    let center_x = (normalized_min.x + normalized_max.x) * 0.5;
    let center_z = (normalized_min.z + normalized_max.z) * 0.5;
    Mat4::from_translation(Vec3::new(
        -center_x * scale,
        -normalized_min.y * scale,
        -center_z * scale,
    )) * Mat4::from_scale(Vec3::splat(scale))
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
                node_local.translation =
                    sample_vec3_channel(&channel.keyframe_times, values, playback_time);
            }
            (AnimationProperty::Scale, AnimationOutputs::Vec3(values)) => {
                node_local.scale =
                    sample_vec3_channel(&channel.keyframe_times, values, playback_time);
            }
            (AnimationProperty::Rotation, AnimationOutputs::Quat(values)) => {
                node_local.rotation =
                    sample_quat_channel(&channel.keyframe_times, values, playback_time);
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
            Some(gltf::animation::util::ReadOutputs::Rotations(values)) => AnimationOutputs::Quat(
                values
                    .into_f32()
                    .map(|value| Quat::from_xyzw(value[0], value[1], value[2], value[3]))
                    .collect(),
            ),
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

fn bind_peer_realtime_channel(
    player_id: u64,
    channel: &RtcDataChannel,
    sender: &Sender<PeerRealtimeEvent>,
) -> PeerRealtimeChannelBindings {
    let _ = js_sys::Reflect::set(
        channel.as_ref(),
        &JsValue::from_str("binaryType"),
        &JsValue::from_str("arraybuffer"),
    );

    let open_tx = sender.clone();
    let onopen = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = open_tx.send(PeerRealtimeEvent::Opened { player_id });
    }) as Box<dyn FnMut(WebEvent)>);
    channel.set_onopen(Some(onopen.as_ref().unchecked_ref()));

    let close_tx = sender.clone();
    let onclose = Closure::wrap(Box::new(move |_event: WebEvent| {
        let _ = close_tx.send(PeerRealtimeEvent::Closed { player_id });
    }) as Box<dyn FnMut(WebEvent)>);
    channel.set_onclose(Some(onclose.as_ref().unchecked_ref()));

    let message_tx = sender.clone();
    let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
        let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
        let _ = message_tx.send(PeerRealtimeEvent::Message { player_id, bytes });
    }) as Box<dyn FnMut(MessageEvent)>);
    channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

    PeerRealtimeChannelBindings {
        _onopen: onopen,
        _onclose: onclose,
        _onmessage: onmessage,
    }
}

fn spawn_webrtc_offer(websocket: WebSocket, connection: RtcPeerConnection, remote_player_id: u64) {
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
        if JsFuture::from(connection.set_local_description(&description))
            .await
            .is_ok()
        {
            send_client_message_over_websocket(
                &websocket,
                &ClientMessage::WebRtcSignal(ClientWebRtcSignal {
                    target_player_id: remote_player_id,
                    payload: WebRtcSignalPayload::Offer { sdp },
                }),
            );
        }
    });
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

fn clear_peer_connection_registries() {
    REMOTE_MEDIA_REGISTRY.with(|registry| registry.borrow_mut().clear());
    REMOTE_DATA_CHANNEL_REGISTRY.with(|registry| registry.borrow_mut().clear());
    PENDING_ICE_REGISTRY.with(|registry| registry.borrow_mut().clear());
}

fn remove_peer_connection_registry_entry(player_id: u64) {
    REMOTE_MEDIA_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&player_id);
    });
    REMOTE_DATA_CHANNEL_REGISTRY.with(|registry| {
        if let Some(registration) = registry.borrow_mut().remove(&player_id) {
            registration.channel.close();
        }
    });
    PENDING_ICE_REGISTRY.with(|registry| {
        registry.borrow_mut().remove(&player_id);
    });
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

            let document = window
                .document()
                .ok_or_else(|| anyhow::anyhow!("document unavailable"))?;
            let video: HtmlVideoElement = document
                .create_element("video")
                .map_err(|error| anyhow::anyhow!("video element create failed: {error:?}"))?
                .dyn_into()
                .map_err(|_| anyhow::anyhow!("video element cast failed"))?;
            video.set_autoplay(true);
            video.set_muted(true);
            video
                .set_attribute("playsinline", "true")
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
                    let surface =
                        terrain.surface_height(sample_x.floor() as i64, sample_z.floor() as i64);

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

    Vec3::new(
        0.5,
        terrain.surface_height(0, 0) as f32 + 3.0 + PLAYER_EYE_HEIGHT,
        0.5,
    )
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
) -> Result<(
    Receiver<MeshBuildResult>,
    Vec<Worker>,
    Vec<Closure<dyn FnMut(MessageEvent)>>,
)> {
    let (tx, rx) = mpsc::channel::<MeshBuildResult>();
    let mut workers = Vec::with_capacity(worker_count);
    let mut onmessages = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let worker = Worker::new("/play/mesh-worker.js")
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

            let vertices_value =
                js_sys::Reflect::get(&object, &JsValue::from_str("vertices")).unwrap();
            let indices_value =
                js_sys::Reflect::get(&object, &JsValue::from_str("indices")).unwrap();
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
    let websocket =
        WebSocket::new(&url).map_err(|error| anyhow::anyhow!("create websocket: {error:?}"))?;
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
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("kind"),
        &JsValue::from_str("build"),
    );
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("x"),
        &JsValue::from_f64(f64::from(position.x)),
    );
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("z"),
        &JsValue::from_f64(f64::from(position.z)),
    );
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
    let _ = js_sys::Reflect::set(
        &job,
        &JsValue::from_str("kind"),
        &JsValue::from_str("mesh_chunk"),
    );
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
    let mut voxels =
        Vec::with_capacity(CHUNK_WIDTH as usize * CHUNK_HEIGHT as usize * CHUNK_DEPTH as usize);
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
    let protocol = location.protocol().unwrap_or_else(|_| "http:".to_string());
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
                if !within_chunk_radius(x, z, radius) {
                    continue;
                }

                positions.push(ChunkPos { x, z });
            }
        }
    }

    positions
}

fn within_chunk_radius(x: i32, z: i32, radius: i32) -> bool {
    let x = i64::from(x);
    let z = i64::from(z);
    let radius = i64::from(radius);
    x * x + z * z <= radius * radius
}

fn chunk_from_world_position(position: Vec3) -> ChunkPos {
    ChunkPos {
        x: (position.x / CHUNK_WIDTH as f32).floor() as i32,
        z: (position.z / CHUNK_DEPTH as f32).floor() as i32,
    }
}

fn chunk_center(position: ChunkPos, y: f32) -> Vec3 {
    Vec3::new(
        position.x as f32 * CHUNK_WIDTH as f32 + CHUNK_WORLD_RADIUS,
        y,
        position.z as f32 * CHUNK_DEPTH as f32 + CHUNK_WORLD_RADIUS,
    )
}

fn horizontal_forward(direction: Vec3) -> Vec3 {
    Vec3::new(direction.x, 0.0, direction.z).normalize_or_zero()
}

fn scaled_render_size(size: PhysicalSize<u32>, scale: f32) -> PhysicalSize<u32> {
    let width = ((size.width.max(1) as f32) * scale).round().max(1.0) as u32;
    let height = ((size.height.max(1) as f32) * scale).round().max(1.0) as u32;
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
        chunk_priority(*a, current_chunk, camera_position, forward).total_cmp(&chunk_priority(
            *b,
            current_chunk,
            camera_position,
            forward,
        ))
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

fn chunk_priority(
    position: ChunkPos,
    camera_chunk: ChunkPos,
    camera_position: Vec3,
    forward: Vec3,
) -> f32 {
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

fn chunk_render_priority(position: ChunkPos, camera_position: Vec3, forward: Vec3) -> f32 {
    let center = chunk_center(position, camera_position.y);
    let horizontal = Vec3::new(
        center.x - camera_position.x,
        0.0,
        center.z - camera_position.z,
    );
    let world_distance = horizontal.length();
    let distance = world_distance / CHUNK_WIDTH as f32;
    if world_distance <= f32::EPSILON || forward == Vec3::ZERO {
        return distance;
    }

    let view_dot = forward
        .dot(horizontal / world_distance.max(0.001))
        .clamp(-1.0, 1.0);
    let forward_penalty = (1.0 - view_dot) * 6.0;
    distance + forward_penalty
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
struct WildPetHit {
    pet_id: u64,
    distance: f32,
}

#[derive(Clone, Copy, Debug)]
struct WorldWeaponHit {
    weapon_id: u64,
    distance: f32,
}

#[derive(Clone, Copy, Debug)]
enum InteractionTarget {
    Block(RaycastHit),
    Link,
    WildPet(WildPetHit),
    WorldWeapon(WorldWeaponHit),
}

fn format_weapon_kind_label(kind: &str) -> String {
    kind.split(|ch: char| ch == '-' || ch == '_' || ch.is_whitespace())
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => format!(
                    "{}{}",
                    first.to_ascii_uppercase(),
                    chars.as_str().to_ascii_lowercase()
                ),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
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

fn ray_aabb_distance(origin: Vec3, direction: Vec3, min: Vec3, max: Vec3) -> Option<f32> {
    let mut t_min = 0.0f32;
    let mut t_max = f32::INFINITY;

    for axis in 0..3 {
        let origin_component = origin[axis];
        let direction_component = direction[axis];
        let min_component = min[axis];
        let max_component = max[axis];

        if direction_component.abs() <= f32::EPSILON {
            if origin_component < min_component || origin_component > max_component {
                return None;
            }
            continue;
        }

        let inv = 1.0 / direction_component;
        let mut t0 = (min_component - origin_component) * inv;
        let mut t1 = (max_component - origin_component) * inv;
        if t0 > t1 {
            std::mem::swap(&mut t0, &mut t1);
        }
        t_min = t_min.max(t0);
        t_max = t_max.min(t1);
        if t_min > t_max {
            return None;
        }
    }

    Some(if t_min >= 0.0 { t_min } else { t_max })
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

fn add_aabb_outline(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    min: Vec3,
    max: Vec3,
    thickness: f32,
    color: [f32; 3],
    tile: (u32, u32),
) {
    let half = thickness * 0.5;
    let center = (min + max) * 0.5;
    let extent = (max - min) * 0.5;

    for &y in &[-extent.y, extent.y] {
        for &z in &[-extent.z, extent.z] {
            add_box_oriented(
                vertices,
                indices,
                center + Vec3::new(0.0, y, z),
                Vec3::new(extent.x, 0.0, 0.0),
                Vec3::new(0.0, half, 0.0),
                Vec3::new(0.0, 0.0, half),
                color,
                tile,
            );
        }
    }

    for &x in &[-extent.x, extent.x] {
        for &z in &[-extent.z, extent.z] {
            add_box_oriented(
                vertices,
                indices,
                center + Vec3::new(x, 0.0, z),
                Vec3::new(half, 0.0, 0.0),
                Vec3::new(0.0, extent.y, 0.0),
                Vec3::new(0.0, 0.0, half),
                color,
                tile,
            );
        }
    }

    for &x in &[-extent.x, extent.x] {
        for &y in &[-extent.y, extent.y] {
            add_box_oriented(
                vertices,
                indices,
                center + Vec3::new(x, y, 0.0),
                Vec3::new(half, 0.0, 0.0),
                Vec3::new(0.0, half, 0.0),
                Vec3::new(0.0, 0.0, extent.z),
                color,
                tile,
            );
        }
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
    add_face_indices(
        vertices,
        indices,
        [corners[3], corners[2], corners[1], corners[0]],
        color,
        uvs,
    );
    add_face_indices(
        vertices,
        indices,
        [corners[6], corners[7], corners[4], corners[5]],
        color,
        uvs,
    );
    add_face_indices(
        vertices,
        indices,
        [corners[2], corners[6], corners[5], corners[1]],
        color,
        uvs,
    );
    add_face_indices(
        vertices,
        indices,
        [corners[7], corners[3], corners[0], corners[4]],
        color,
        uvs,
    );
    add_face_indices(
        vertices,
        indices,
        [corners[7], corners[6], corners[2], corners[3]],
        color,
        uvs,
    );
    add_face_indices(
        vertices,
        indices,
        [corners[0], corners[1], corners[5], corners[4]],
        color,
        uvs,
    );
}

fn normalized_lifetime_progress(started_at: Instant, expires_at: Instant, now: Instant) -> f32 {
    let total = expires_at
        .saturating_duration_since(started_at)
        .as_secs_f32()
        .max(0.001);
    let elapsed = now.saturating_duration_since(started_at).as_secs_f32();
    (elapsed / total).clamp(0.0, 1.0)
}

fn forward_basis(forward: Vec3) -> Option<(Vec3, Vec3)> {
    let reference_up = if forward.dot(Vec3::Y).abs() > 0.95 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let axis_x = forward.cross(reference_up).normalize_or_zero();
    let axis_y = axis_x.cross(forward).normalize_or_zero();
    (axis_x.length_squared() > f32::EPSILON && axis_y.length_squared() > f32::EPSILON)
        .then_some((axis_x, axis_y))
}

fn add_pet_weapon_laser(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    origin: Vec3,
    target: Vec3,
) {
    let delta = target - origin;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }

    let forward = delta / length;
    let Some((axis_x, axis_y)) = forward_basis(forward) else {
        return;
    };

    add_box_oriented(
        vertices,
        indices,
        (origin + target) * 0.5,
        axis_x * PET_WEAPON_SHOT_HALF_WIDTH,
        axis_y * PET_WEAPON_SHOT_HALF_DEPTH,
        forward * (length * 0.5),
        [1.0, 0.82, 0.32],
        (3, 1),
    );
}

fn add_pet_weapon_projectile(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    origin: Vec3,
    target: Vec3,
    progress: f32,
) {
    let delta = target - origin;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let forward = delta / length;
    let Some((axis_x, axis_y)) = forward_basis(forward) else {
        return;
    };

    let projectile_center = origin + delta * progress;
    add_box_oriented(
        vertices,
        indices,
        projectile_center,
        axis_x * PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT,
        axis_y * PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT,
        forward * PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT,
        [1.0, 0.92, 0.52],
        (3, 1),
    );

    let trail_length = PET_WEAPON_GUN_TRAIL_HALF_LENGTH * (1.0 - progress * 0.35);
    add_box_oriented(
        vertices,
        indices,
        projectile_center - forward * trail_length * 0.65,
        axis_x * (PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT * 0.38),
        axis_y * (PET_WEAPON_GUN_PROJECTILE_HALF_EXTENT * 0.38),
        forward * trail_length,
        [1.0, 0.72, 0.24],
        (3, 1),
    );
}

fn add_pet_weapon_flame_burst(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    origin: Vec3,
    target: Vec3,
    progress: f32,
) {
    let delta = target - origin;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let forward = delta / length;
    let Some((axis_x, axis_y)) = forward_basis(forward) else {
        return;
    };

    let flame_distance = PET_WEAPON_FLAME_BURST_RANGE.min(length * 0.45).max(0.8);
    let center = origin + forward * flame_distance * (0.35 + progress * 0.35);
    let stretch = 0.28 + progress * 0.4;
    add_box_oriented(
        vertices,
        indices,
        center,
        axis_x * (0.16 + progress * 0.08),
        axis_y * (0.12 + progress * 0.07),
        forward * stretch,
        [1.0, 0.58, 0.14],
        (3, 1),
    );
    add_box_oriented(
        vertices,
        indices,
        center + axis_x * 0.18 + axis_y * 0.05,
        axis_x * 0.11,
        axis_y * 0.08,
        forward * (stretch * 0.62),
        [1.0, 0.84, 0.34],
        (3, 1),
    );
    add_box_oriented(
        vertices,
        indices,
        center - axis_x * 0.15 - axis_y * 0.03,
        axis_x * 0.1,
        axis_y * 0.07,
        forward * (stretch * 0.54),
        [0.96, 0.34, 0.08],
        (3, 1),
    );
}

fn add_pet_weapon_sword_throw(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    origin: Vec3,
    target: Vec3,
    progress: f32,
) {
    let delta = target - origin;
    let length = delta.length();
    if length <= f32::EPSILON {
        return;
    }
    let forward = delta / length;
    let Some((axis_x, axis_y)) = forward_basis(forward) else {
        return;
    };

    let out_and_back = if progress <= 0.5 {
        progress * 2.0
    } else {
        (1.0 - progress) * 2.0
    };
    let center =
        origin + delta * out_and_back + axis_y * ((1.0 - (out_and_back * 2.0 - 1.0).abs()) * 0.42);
    add_box_oriented(
        vertices,
        indices,
        center,
        axis_x * PET_WEAPON_SWORD_HALF_WIDTH,
        axis_y * PET_WEAPON_SWORD_HALF_DEPTH,
        forward * PET_WEAPON_SWORD_HALF_LENGTH,
        [0.86, 0.88, 0.96],
        (3, 1),
    );
    add_box_oriented(
        vertices,
        indices,
        center - forward * (PET_WEAPON_SWORD_HALF_LENGTH + 0.08),
        axis_x * 0.03,
        axis_y * 0.03,
        forward * 0.09,
        [0.58, 0.42, 0.16],
        (3, 1),
    );
}

fn add_pet_impostor(
    vertices: &mut Vec<Vertex>,
    indices: &mut Vec<u32>,
    position: Vec3,
    color: [f32; 3],
) {
    let center = position + Vec3::Y * PET_IMPOSTOR_HALF_HEIGHT;
    add_box_oriented(
        vertices,
        indices,
        center,
        Vec3::new(PET_IMPOSTOR_HALF_WIDTH, 0.0, 0.0),
        Vec3::new(0.0, PET_IMPOSTOR_HALF_HEIGHT, 0.0),
        Vec3::new(0.0, 0.0, PET_IMPOSTOR_HALF_WIDTH),
        color,
        PET_IMPOSTOR_TILE,
    );
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

    [
        [min_u, min_v],
        [max_u, min_v],
        [max_u, max_v],
        [min_u, max_v],
    ]
}

fn remote_player_color(player_id: u64) -> [f32; 3] {
    let hue = (player_id as f32 * 0.173).fract();
    let r = 0.45 + 0.4 * (hue * std::f32::consts::TAU).sin().abs();
    let g = 0.45 + 0.4 * ((hue + 0.33) * std::f32::consts::TAU).sin().abs();
    let b = 0.45 + 0.4 * ((hue + 0.66) * std::f32::consts::TAU).sin().abs();
    [r, g, b]
}

fn pet_impostor_color(id: &str) -> [f32; 3] {
    let hash = id.bytes().fold(0_u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    let hue = ((hash % 360) as f32) / 360.0;
    let r = 0.56 + 0.2 * (hue * std::f32::consts::TAU).sin().abs();
    let g = 0.48 + 0.16 * ((hue + 0.2) * std::f32::consts::TAU).sin().abs();
    let b = 0.38 + 0.14 * ((hue + 0.47) * std::f32::consts::TAU).sin().abs();
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
