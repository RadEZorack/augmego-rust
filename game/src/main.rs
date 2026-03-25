use anyhow::{Context, Result};
use glam::{Mat4, Vec3};
use shared_math::{CHUNK_DEPTH, CHUNK_WIDTH, ChunkPos};
use shared_protocol::{
    ClientHello, ClientMessage, LoginRequest, PROTOCOL_VERSION, ServerMessage, SubscribeChunks,
    decode, frame,
};
use shared_world::{BlockId, ChunkData};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};
use wgpu_lite::{Mesh, Renderer, Vertex};
use winit::event::{DeviceEvent, ElementState, Event, KeyEvent, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::WindowBuilder;

fn main() -> Result<()> {
    let event_loop = EventLoop::new()?;
    let window: &'static winit::window::Window = Box::leak(Box::new(
        WindowBuilder::new()
            .with_title("Augmego Voxel Sandbox")
            .build(&event_loop)?,
    ));

    let renderer = pollster::block_on(Renderer::new(window))?;
    let (network_tx, network_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    start_network_thread(network_tx, command_rx);

    let mut app = GameApp::new(renderer.size(), network_rx, command_tx);
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
                    app.handle_key(event);
                }
                WindowEvent::RedrawRequested => {
                    app.drain_network();

                    if app.mesh_dirty {
                        chunk_meshes = rebuild_meshes(&renderer, &app.chunk_cache);
                        app.mesh_dirty = false;
                    }

                    app.tick();
                    renderer.update_camera(app.camera_matrix());
                    let visible_meshes = chunk_meshes.values().collect::<Vec<_>>();
                    if let Err(error) = renderer.render(&visible_meshes) {
                        eprintln!("render error: {error:?}");
                        target.exit();
                    }
                }
                _ => {}
            },
            Event::DeviceEvent { event: DeviceEvent::MouseMotion { delta }, .. } => {
                app.handle_mouse_motion(delta.0 as f32, delta.1 as f32);
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
    mesh_dirty: bool,
    pressed: HashSet<KeyCode>,
    camera: Camera,
    last_tick: Instant,
    width: u32,
    height: u32,
}

impl GameApp {
    fn new(size: winit::dpi::PhysicalSize<u32>, network_rx: Receiver<NetworkEvent>, command_tx: Sender<ClientCommand>) -> Self {
        Self {
            chunk_cache: HashMap::new(),
            network_rx,
            command_tx,
            mesh_dirty: false,
            pressed: HashSet::new(),
            camera: Camera::default(),
            last_tick: Instant::now(),
            width: size.width.max(1),
            height: size.height.max(1),
        }
    }

    fn resize(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
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

    fn drain_network(&mut self) {
        while let Ok(event) = self.network_rx.try_recv() {
            match event {
                NetworkEvent::Chunk(chunk) => {
                    self.chunk_cache.insert(chunk.position, chunk);
                    self.mesh_dirty = true;
                }
                NetworkEvent::Welcome { message, .. } => {
                    println!("{message}");
                }
                NetworkEvent::PlayerState { position } => {
                    self.camera.position = Vec3::new(position[0], position[1] + 4.0, position[2] + 8.0);
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

        let _ = self.command_tx.send(ClientCommand::Input {
            movement: [movement.x, movement.y, movement.z],
        });
    }

    fn camera_matrix(&self) -> Mat4 {
        let aspect = self.width as f32 / self.height.max(1) as f32;
        self.camera.matrix(aspect)
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
        let look = Vec3::new(
            self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            self.yaw.cos() * self.pitch.cos(),
        );
        let view = Mat4::look_at_rh(self.position, self.position + look, Vec3::Y);
        let proj = Mat4::perspective_rh_gl(60.0_f32.to_radians(), aspect, 0.1, 1_500.0);
        proj * view
    }
}

#[derive(Debug)]
enum ClientCommand {
    Input { movement: [f32; 3] },
}

#[derive(Debug)]
enum NetworkEvent {
    Welcome { message: String },
    Chunk(ChunkData),
    PlayerState { position: [f32; 3] },
    BlockAction(String),
    Disconnected(String),
}

fn start_network_thread(events: Sender<NetworkEvent>, commands: Receiver<ClientCommand>) {
    thread::spawn(move || {
        if let Err(error) = network_main(events.clone(), commands) {
            let _ = events.send(NetworkEvent::Disconnected(error.to_string()));
        }
    });
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
        });
    }

    let _inventory = read_server_blocking(&mut stream)?;
    write_client(
        &mut stream,
        &ClientMessage::SubscribeChunks(SubscribeChunks {
            center: ChunkPos { x: 0, z: 0 },
            radius: 3,
        }),
    )?;

    let mut tick = 0_u64;
    loop {
        while let Ok(command) = commands.try_recv() {
            match command {
                ClientCommand::Input { movement } => {
                    tick += 1;
                    write_client(
                        &mut stream,
                        &ClientMessage::PlayerInputTick(shared_protocol::PlayerInputTick {
                            tick,
                            movement,
                            jump: false,
                        }),
                    )?;
                }
            }
        }

        match try_read_server(&mut stream) {
            Ok(Some(message)) => match message {
                ServerMessage::ChunkData(chunk) => {
                    let _ = events.send(NetworkEvent::Chunk(chunk));
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

fn rebuild_meshes(renderer: &Renderer<'_>, chunk_cache: &HashMap<ChunkPos, ChunkData>) -> HashMap<ChunkPos, Mesh> {
    chunk_cache
        .iter()
        .map(|(position, chunk)| {
            let (vertices, indices) = mesh_chunk(chunk);
            (*position, renderer.create_mesh(&vertices, &indices))
        })
        .collect()
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
    let neighbors = [
        ((0, 0, -1), face_vertices(world, Face::North, color_for_block(block))),
        ((0, 0, 1), face_vertices(world, Face::South, color_for_block(block))),
        ((-1, 0, 0), face_vertices(world, Face::West, color_for_block(block))),
        ((1, 0, 0), face_vertices(world, Face::East, color_for_block(block))),
        ((0, 1, 0), face_vertices(world, Face::Up, brighten(color_for_block(block), 0.15))),
        ((0, -1, 0), face_vertices(world, Face::Down, darken(color_for_block(block), 0.2))),
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

fn face_vertices(origin: [f32; 3], face: Face, color: [f32; 3]) -> [Vertex; 4] {
    let [x, y, z] = origin;
    match face {
        Face::North => [
            v(x, y + 1.0, z, color),
            v(x + 1.0, y + 1.0, z, color),
            v(x + 1.0, y, z, color),
            v(x, y, z, color),
        ],
        Face::South => [
            v(x + 1.0, y + 1.0, z + 1.0, color),
            v(x, y + 1.0, z + 1.0, color),
            v(x, y, z + 1.0, color),
            v(x + 1.0, y, z + 1.0, color),
        ],
        Face::East => [
            v(x + 1.0, y + 1.0, z, color),
            v(x + 1.0, y + 1.0, z + 1.0, color),
            v(x + 1.0, y, z + 1.0, color),
            v(x + 1.0, y, z, color),
        ],
        Face::West => [
            v(x, y + 1.0, z + 1.0, color),
            v(x, y + 1.0, z, color),
            v(x, y, z, color),
            v(x, y, z + 1.0, color),
        ],
        Face::Up => [
            v(x, y + 1.0, z, color),
            v(x, y + 1.0, z + 1.0, color),
            v(x + 1.0, y + 1.0, z + 1.0, color),
            v(x + 1.0, y + 1.0, z, color),
        ],
        Face::Down => [
            v(x, y, z, color),
            v(x + 1.0, y, z, color),
            v(x + 1.0, y, z + 1.0, color),
            v(x, y, z + 1.0, color),
        ],
    }
}

fn v(x: f32, y: f32, z: f32, color: [f32; 3]) -> Vertex {
    Vertex {
        position: [x, y, z],
        color,
    }
}

fn color_for_block(block: BlockId) -> [f32; 3] {
    match block {
        BlockId::Grass => [0.35, 0.75, 0.28],
        BlockId::Dirt => [0.48, 0.32, 0.18],
        BlockId::Stone => [0.52, 0.54, 0.58],
        BlockId::Sand => [0.86, 0.79, 0.51],
        BlockId::Water => [0.15, 0.42, 0.87],
        BlockId::Log => [0.55, 0.34, 0.16],
        BlockId::Leaves => [0.20, 0.52, 0.16],
        BlockId::Planks => [0.73, 0.58, 0.29],
        BlockId::Glass => [0.71, 0.88, 0.93],
        BlockId::Lantern => [0.95, 0.81, 0.29],
        BlockId::Storage => [0.58, 0.40, 0.18],
        BlockId::Air => [1.0, 1.0, 1.0],
    }
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
