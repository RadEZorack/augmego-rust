use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use std::{mem, num::NonZeroU64};
use wgpu::util::DeviceExt;
#[cfg(target_arch = "wasm32")]
use wgpu::web_sys::HtmlCanvasElement;
use winit::{dpi::PhysicalSize, window::Window};

const TILE_SIZE: u32 = 16;
const ATLAS_TILES: u32 = 12;
const TEXTURED_BLOCK_TILE_COLUMNS: u32 = 6;
const TEXTURED_BLOCK_TILE_ROW_START: u32 = 8;
const MOTTLE_DENSE_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/mottle_dense_16x16.txt"
));
const MOTTLE_SOFT_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/mottle_soft_16x16.txt"
));
const WATER_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/water_16x16.txt"
));
const WOOD_BARK_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/wood_bark_16x16.txt"
));
const LEAF_CANOPY_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/leaf_canopy_16x16.txt"
));
const PLANKS_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/planks_16x16.txt"
));
const GLASS_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/glass_16x16.txt"
));
const LANTERN_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/lantern_16x16.txt"
));
const CRATE_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/crate_16x16.txt"
));
const ORE_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/ore_16x16.txt"
));
const SANDSTONE_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/sandstone_16x16.txt"
));
const COAL_ORE_TEXTURE_ART: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../assets/textures/coal_ore_16x16.txt"
));
type PaletteEntry = (char, [u8; 4]);

struct AsciiTextureDefinition {
    name: &'static str,
    block_id: u32,
    art: &'static str,
    palette: &'static [PaletteEntry],
}

const fn textured_block_tile(block_id: u32) -> (u32, u32) {
    let index = block_id - 1;
    (
        index % TEXTURED_BLOCK_TILE_COLUMNS,
        TEXTURED_BLOCK_TILE_ROW_START + index / TEXTURED_BLOCK_TILE_COLUMNS,
    )
}

const GRASS_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [142, 184, 84, 255]),
    ('b', [112, 164, 69, 255]),
    ('c', [78, 132, 52, 255]),
    ('d', [154, 163, 80, 255]),
    ('e', [47, 96, 38, 255]),
];
const DIRT_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [157, 116, 81, 255]),
    ('b', [134, 93, 61, 255]),
    ('c', [112, 74, 47, 255]),
    ('d', [90, 58, 36, 255]),
    ('e', [67, 42, 27, 255]),
];
const STONE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [182, 186, 194, 255]),
    ('b', [152, 156, 165, 255]),
    ('c', [126, 130, 139, 255]),
    ('d', [97, 102, 112, 255]),
    ('e', [74, 79, 88, 255]),
];
const SAND_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [244, 229, 184, 255]),
    ('b', [230, 212, 154, 255]),
    ('c', [214, 192, 129, 255]),
    ('d', [189, 166, 107, 255]),
];
const WATER_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [197, 230, 247, 255]),
    ('b', [128, 188, 230, 255]),
    ('c', [90, 154, 214, 255]),
    ('d', [53, 112, 184, 255]),
    ('e', [33, 82, 154, 255]),
    ('f', [224, 245, 255, 255]),
];
const LOG_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [198, 154, 96, 255]),
    ('b', [182, 136, 84, 255]),
    ('c', [159, 117, 70, 255]),
    ('d', [132, 94, 56, 255]),
    ('e', [103, 71, 42, 255]),
    ('f', [78, 53, 31, 255]),
];
const LEAVES_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [140, 185, 84, 255]),
    ('b', [108, 163, 70, 255]),
    ('c', [79, 133, 54, 255]),
    ('d', [56, 103, 42, 255]),
    ('e', [158, 176, 76, 255]),
];
const PLANKS_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [210, 167, 109, 255]),
    ('b', [183, 140, 89, 255]),
    ('c', [155, 114, 68, 255]),
    ('d', [125, 87, 49, 255]),
    ('e', [95, 63, 35, 255]),
];
const GLASS_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [239, 250, 255, 255]),
    ('b', [213, 239, 249, 255]),
    ('c', [183, 223, 239, 255]),
    ('d', [141, 191, 215, 255]),
    ('e', [106, 161, 190, 255]),
];
const LANTERN_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [255, 243, 190, 255]),
    ('b', [247, 210, 119, 255]),
    ('c', [188, 142, 67, 255]),
    ('d', [131, 92, 39, 255]),
    ('e', [87, 60, 26, 255]),
    ('f', [255, 229, 151, 255]),
    ('g', [255, 248, 215, 255]),
];
const STORAGE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [191, 145, 90, 255]),
    ('b', [162, 117, 68, 255]),
    ('c', [131, 90, 51, 255]),
    ('d', [87, 96, 110, 255]),
    ('e', [103, 68, 40, 255]),
];
const GOLD_ORE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [182, 186, 194, 255]),
    ('b', [149, 154, 164, 255]),
    ('c', [119, 124, 133, 255]),
    ('d', [228, 187, 63, 255]),
    ('e', [186, 145, 39, 255]),
    ('f', [93, 98, 107, 255]),
];
const COAL_ORE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [149, 153, 161, 255]),
    ('b', [126, 130, 138, 255]),
    ('c', [99, 103, 111, 255]),
    ('d', [20, 21, 26, 255]),
    ('e', [44, 46, 53, 255]),
    ('f', [78, 81, 92, 255]),
];
const IRON_ORE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [180, 184, 191, 255]),
    ('b', [149, 153, 161, 255]),
    ('c', [119, 123, 131, 255]),
    ('d', [173, 111, 78, 255]),
    ('e', [136, 83, 55, 255]),
    ('f', [94, 98, 106, 255]),
];
const SANDSTONE_TEXTURE_PALETTE: &[PaletteEntry] = &[
    ('a', [236, 217, 173, 255]),
    ('b', [216, 195, 141, 255]),
    ('c', [194, 171, 118, 255]),
    ('d', [166, 145, 98, 255]),
    ('e', [129, 109, 71, 255]),
];
const VOXEL_TEXTURE_DEFINITIONS: &[AsciiTextureDefinition] = &[
    AsciiTextureDefinition {
        name: "Grass",
        block_id: 1,
        art: MOTTLE_DENSE_TEXTURE_ART,
        palette: GRASS_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Dirt",
        block_id: 2,
        art: MOTTLE_DENSE_TEXTURE_ART,
        palette: DIRT_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Stone",
        block_id: 3,
        art: MOTTLE_DENSE_TEXTURE_ART,
        palette: STONE_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Sand",
        block_id: 4,
        art: MOTTLE_SOFT_TEXTURE_ART,
        palette: SAND_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Water",
        block_id: 5,
        art: WATER_TEXTURE_ART,
        palette: WATER_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Log",
        block_id: 6,
        art: WOOD_BARK_TEXTURE_ART,
        palette: LOG_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Leaves",
        block_id: 7,
        art: LEAF_CANOPY_TEXTURE_ART,
        palette: LEAVES_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Planks",
        block_id: 8,
        art: PLANKS_TEXTURE_ART,
        palette: PLANKS_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Glass",
        block_id: 9,
        art: GLASS_TEXTURE_ART,
        palette: GLASS_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Lantern",
        block_id: 10,
        art: LANTERN_TEXTURE_ART,
        palette: LANTERN_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Storage",
        block_id: 11,
        art: CRATE_TEXTURE_ART,
        palette: STORAGE_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Gold Ore",
        block_id: 12,
        art: ORE_TEXTURE_ART,
        palette: GOLD_ORE_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Coal Ore",
        block_id: 13,
        art: COAL_ORE_TEXTURE_ART,
        palette: COAL_ORE_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Iron Ore",
        block_id: 14,
        art: ORE_TEXTURE_ART,
        palette: IRON_ORE_TEXTURE_PALETTE,
    },
    AsciiTextureDefinition {
        name: "Sandstone",
        block_id: 15,
        art: SANDSTONE_TEXTURE_ART,
        palette: SANDSTONE_TEXTURE_PALETTE,
    },
];
pub const MAX_SKIN_JOINTS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub color: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub material_id: f32,
}

impl Vertex {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<Vertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as u64,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 6]>() as u64,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 9]>() as u64,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 11]>() as u64,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32,
                },
            ],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct AnimatedVertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub joints: [f32; 4],
    pub weights: [f32; 4],
}

impl AnimatedVertex {
    pub fn layout() -> wgpu::VertexBufferLayout<'static> {
        wgpu::VertexBufferLayout {
            array_stride: mem::size_of::<AnimatedVertex>() as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &[
                wgpu::VertexAttribute {
                    offset: 0,
                    shader_location: 0,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 3]>() as u64,
                    shader_location: 1,
                    format: wgpu::VertexFormat::Float32x3,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 6]>() as u64,
                    shader_location: 2,
                    format: wgpu::VertexFormat::Float32x2,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 8]>() as u64,
                    shader_location: 3,
                    format: wgpu::VertexFormat::Float32x4,
                },
                wgpu::VertexAttribute {
                    offset: mem::size_of::<[f32; 12]>() as u64,
                    shader_location: 4,
                    format: wgpu::VertexFormat::Float32x4,
                },
            ],
        }
    }
}

pub struct Mesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
}

pub struct DynamicTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sampler: wgpu::Sampler,
    width: u32,
    height: u32,
}

pub struct TexturedMesh {
    mesh: Mesh,
    bind_group: wgpu::BindGroup,
}

pub struct TexturedMeshDraw<'a> {
    mesh: &'a TexturedMesh,
    model: Mat4,
}

pub struct AnimatedMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    texture_bind_group: wgpu::BindGroup,
}

pub struct AnimatedMeshDraw<'a> {
    mesh: &'a AnimatedMesh,
    uniform: SkinUniform,
}

struct DepthTarget {
    view: wgpu::TextureView,
}

impl DepthTarget {
    fn new(device: &wgpu::Device, size: PhysicalSize<u32>) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("depth-texture"),
            size: wgpu::Extent3d {
                width: size.width.max(1),
                height: size.height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth24Plus,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });

        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    view_proj: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ModelUniform {
    model: [[f32; 4]; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SkinUniform {
    model: [[f32; 4]; 4],
    joints: [[[f32; 4]; 4]; MAX_SKIN_JOINTS],
}

struct MaterialTarget {
    texture: wgpu::Texture,
    layout: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
}

impl MaterialTarget {
    fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let atlas_size = TILE_SIZE * ATLAS_TILES;
        let mut pixels = vec![0_u8; (atlas_size * atlas_size * 4) as usize];

        for tile_y in 0..ATLAS_TILES {
            for tile_x in 0..ATLAS_TILES {
                fill_tile(
                    &mut pixels,
                    atlas_size,
                    tile_x,
                    tile_y,
                    tile_color(tile_x, tile_y),
                );
            }
        }
        fill_voxel_texture_tiles(&mut pixels, atlas_size);

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("material-atlas"),
            size: wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * atlas_size),
                rows_per_image: Some(atlas_size),
            },
            wgpu::Extent3d {
                width: atlas_size,
                height: atlas_size,
                depth_or_array_layers: 1,
            },
        );

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("material-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("material-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled: false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("material-bind-group"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        Self {
            texture,
            layout,
            bind_group,
        }
    }
}

impl DynamicTexture {
    fn bind_group(&self, device: &wgpu::Device, layout: &wgpu::BindGroupLayout) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dynamic-texture-bind-group"),
            layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&self.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
}

pub struct Renderer<'a> {
    surface: wgpu::Surface<'a>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    size: PhysicalSize<u32>,
    max_surface_extent: u32,
    pipeline: wgpu::RenderPipeline,
    modeled_textured_pipeline: wgpu::RenderPipeline,
    animated_pipeline: wgpu::RenderPipeline,
    overlay_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    material: MaterialTarget,
    model_layout: wgpu::BindGroupLayout,
    model_buffer: wgpu::Buffer,
    model_bind_group: wgpu::BindGroup,
    model_stride: u64,
    model_capacity: usize,
    skin_layout: wgpu::BindGroupLayout,
    skin_buffer: wgpu::Buffer,
    skin_bind_group: wgpu::BindGroup,
    skin_stride: u64,
    skin_capacity: usize,
    depth: DepthTarget,
}

impl<'a> Renderer<'a> {
    pub async fn new(window: &'a Window) -> Result<Self> {
        Self::new_with_size(window, window.inner_size()).await
    }

    pub async fn new_with_size(window: &'a Window, size: PhysicalSize<u32>) -> Result<Self> {
        let instance = create_instance();
        let surface = instance.create_surface(window).context("create surface")?;
        Self::from_surface(&instance, surface, size).await
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn new_with_canvas(
        canvas: HtmlCanvasElement,
        size: PhysicalSize<u32>,
    ) -> Result<Renderer<'static>> {
        let instance = create_instance();
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas))
            .context("create canvas surface")?;
        Renderer::from_surface(&instance, surface, size).await
    }

    async fn from_surface(
        instance: &wgpu::Instance,
        surface: wgpu::Surface<'a>,
        size: PhysicalSize<u32>,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
            })
            .await
            .ok_or_else(|| anyhow!("request adapter"))?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("wgpu-lite-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: device_limits(),
                },
                None,
            )
            .await
            .context("request device")?;

        let max_surface_extent = device.limits().max_texture_dimension_2d.max(1);
        let size = clamp_surface_size(size, max_surface_extent);

        let capabilities = surface.get_capabilities(&adapter);
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| format.is_srgb())
            .unwrap_or(capabilities.formats[0]);
        let present_mode = if capabilities
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        let camera_uniform = CameraUniform {
            view_proj: Mat4::IDENTITY.to_cols_array_2d(),
        };
        let camera_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("camera-buffer"),
            contents: bytemuck::bytes_of(&camera_uniform),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let camera_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("camera-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let camera_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("camera-bind-group"),
            layout: &camera_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: camera_buffer.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("voxel-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
        });
        let modeled_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("modeled-shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("modeled_shader.wgsl").into()),
        });

        let material = MaterialTarget::new(&device, &queue);

        let model_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("model-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: Some(model_uniform_size()),
                },
                count: None,
            }],
        });
        let model_stride = aligned_uniform_size(
            device.limits().min_uniform_buffer_offset_alignment,
            model_uniform_size().get(),
        );
        let (model_buffer, model_bind_group) = create_uniform_buffer_and_bind_group(
            &device,
            &model_layout,
            model_stride,
            1,
            model_uniform_size(),
        );

        let skin_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("skin-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: Some(skin_uniform_size()),
                },
                count: None,
            }],
        });
        let skin_stride = aligned_uniform_size(
            device.limits().min_uniform_buffer_offset_alignment,
            skin_uniform_size().get(),
        );
        let (skin_buffer, skin_bind_group) = create_uniform_buffer_and_bind_group(
            &device,
            &skin_layout,
            skin_stride,
            1,
            skin_uniform_size(),
        );

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline-layout"),
            bind_group_layouts: &[&camera_layout, &material.layout],
            push_constant_ranges: &[],
        });

        let modeled_textured_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("modeled-textured-pipeline-layout"),
                bind_group_layouts: &[&camera_layout, &material.layout, &model_layout],
                push_constant_ranges: &[],
            });

        let animated_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("animated-pipeline-layout"),
                bind_group_layouts: &[&camera_layout, &material.layout, &skin_layout],
                push_constant_ranges: &[],
            });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("voxel-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
        });

        let modeled_textured_pipeline =
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("modeled-textured-pipeline"),
                layout: Some(&modeled_textured_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &modeled_shader,
                    entry_point: "vs_modeled_main",
                    buffers: &[Vertex::layout()],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    strip_index_format: None,
                    front_face: wgpu::FrontFace::Ccw,
                    cull_mode: Some(wgpu::Face::Back),
                    unclipped_depth: false,
                    polygon_mode: wgpu::PolygonMode::Fill,
                    conservative: false,
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: wgpu::TextureFormat::Depth24Plus,
                    depth_write_enabled: true,
                    depth_compare: wgpu::CompareFunction::Less,
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &modeled_shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: config.format,
                        blend: Some(wgpu::BlendState::REPLACE),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
            });

        let overlay_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("overlay-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_main",
                buffers: &[Vertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
        });

        let animated_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("animated-pipeline"),
            layout: Some(&animated_pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: "vs_skinned_main",
                buffers: &[AnimatedVertex::layout()],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                unclipped_depth: false,
                polygon_mode: wgpu::PolygonMode::Fill,
                conservative: false,
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: wgpu::TextureFormat::Depth24Plus,
                depth_write_enabled: true,
                depth_compare: wgpu::CompareFunction::Less,
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: "fs_main",
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview: None,
        });

        let depth = DepthTarget::new(&device, size);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            max_surface_extent,
            pipeline,
            modeled_textured_pipeline,
            animated_pipeline,
            overlay_pipeline,
            camera_buffer,
            camera_bind_group,
            material,
            model_layout,
            model_buffer,
            model_bind_group,
            model_stride,
            model_capacity: 1,
            skin_layout,
            skin_buffer,
            skin_bind_group,
            skin_stride,
            skin_capacity: 1,
            depth,
        })
    }

    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }

        let size = clamp_surface_size(size, self.max_surface_extent);
        self.size = size;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
        self.depth = DepthTarget::new(&self.device, size);
    }

    pub fn create_mesh(&self, vertices: &[Vertex], indices: &[u32]) -> Mesh {
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-vertex-buffer"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-index-buffer"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        }
    }

    pub fn create_mesh_from_f32(&self, vertex_floats: &[f32], indices: &[u32]) -> Mesh {
        if !vertex_floats.len().is_multiple_of(12) {
            return self.create_mesh(&[], &[]);
        }

        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-vertex-buffer"),
                contents: bytemuck::cast_slice(vertex_floats),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mesh-index-buffer"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        Mesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
        }
    }

    pub fn create_dynamic_texture(&self, width: u32, height: u32) -> DynamicTexture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("dynamic-rgba-texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("dynamic-texture-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        DynamicTexture {
            texture,
            view,
            sampler,
            width: width.max(1),
            height: height.max(1),
        }
    }

    pub fn update_dynamic_texture_rgba(&self, texture: &DynamicTexture, pixels: &[u8]) {
        let expected_len = (texture.width * texture.height * 4) as usize;
        if pixels.len() != expected_len {
            return;
        }

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * texture.width),
                rows_per_image: Some(texture.height),
            },
            wgpu::Extent3d {
                width: texture.width,
                height: texture.height,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn create_textured_mesh(
        &self,
        vertices: &[Vertex],
        indices: &[u32],
        texture: &DynamicTexture,
    ) -> TexturedMesh {
        TexturedMesh {
            mesh: self.create_mesh(vertices, indices),
            bind_group: texture.bind_group(&self.device, &self.material.layout),
        }
    }

    pub fn create_textured_draw<'b>(
        &self,
        mesh: &'b TexturedMesh,
        model: Mat4,
    ) -> TexturedMeshDraw<'b> {
        TexturedMeshDraw { mesh, model }
    }

    pub fn create_animated_mesh(
        &self,
        vertices: &[AnimatedVertex],
        indices: &[u32],
        texture: &DynamicTexture,
    ) -> AnimatedMesh {
        let vertex_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("animated-mesh-vertex-buffer"),
                contents: bytemuck::cast_slice(vertices),
                usage: wgpu::BufferUsages::VERTEX,
            });
        let index_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("animated-mesh-index-buffer"),
                contents: bytemuck::cast_slice(indices),
                usage: wgpu::BufferUsages::INDEX,
            });

        AnimatedMesh {
            vertex_buffer,
            index_buffer,
            index_count: indices.len() as u32,
            texture_bind_group: texture.bind_group(&self.device, &self.material.layout),
        }
    }

    pub fn create_animated_draw<'b>(
        &self,
        mesh: &'b AnimatedMesh,
        model: Mat4,
        joints: &[Mat4],
    ) -> AnimatedMeshDraw<'b> {
        AnimatedMeshDraw {
            mesh,
            uniform: animated_uniform(model, joints),
        }
    }

    pub fn update_camera(&self, view_projection: Mat4) {
        let uniform = CameraUniform {
            view_proj: view_projection.to_cols_array_2d(),
        };
        self.queue
            .write_buffer(&self.camera_buffer, 0, bytemuck::bytes_of(&uniform));
    }

    pub fn update_atlas_tile_rgba(&self, tile: (u32, u32), pixels: &[u8]) {
        let expected_len = (TILE_SIZE * TILE_SIZE * 4) as usize;
        if pixels.len() != expected_len || tile.0 >= ATLAS_TILES || tile.1 >= ATLAS_TILES {
            return;
        }

        self.queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &self.material.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: tile.0 * TILE_SIZE,
                    y: tile.1 * TILE_SIZE,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            pixels,
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * TILE_SIZE),
                rows_per_image: Some(TILE_SIZE),
            },
            wgpu::Extent3d {
                width: TILE_SIZE,
                height: TILE_SIZE,
                depth_or_array_layers: 1,
            },
        );
    }

    pub fn render(
        &mut self,
        meshes: &[&Mesh],
        textured_meshes: &[&TexturedMesh],
        textured_draws: &[TexturedMeshDraw<'_>],
        animated_meshes: &[AnimatedMeshDraw<'_>],
        overlays: &[&Mesh],
    ) -> Result<()> {
        self.ensure_model_capacity(textured_draws.len());
        self.upload_model_uniforms(textured_draws);
        self.ensure_skin_capacity(animated_meshes.len());
        self.upload_skin_uniforms(animated_meshes);

        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                self.surface
                    .get_current_texture()
                    .context("acquire surface texture after reconfigure")?
            }
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(anyhow!("surface out of memory")),
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("main-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.53,
                            g: 0.81,
                            b: 0.92,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.depth.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.material.bind_group, &[]);
            for mesh in meshes {
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
            for textured in textured_meshes {
                pass.set_bind_group(1, &textured.bind_group, &[]);
                pass.set_vertex_buffer(0, textured.mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(
                    textured.mesh.index_buffer.slice(..),
                    wgpu::IndexFormat::Uint32,
                );
                pass.draw_indexed(0..textured.mesh.index_count, 0, 0..1);
            }
            if !textured_draws.is_empty() {
                pass.set_pipeline(&self.modeled_textured_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (index, textured) in textured_draws.iter().enumerate() {
                    pass.set_bind_group(1, &textured.mesh.bind_group, &[]);
                    pass.set_bind_group(2, &self.model_bind_group, &[self.model_offset(index)]);
                    pass.set_vertex_buffer(0, textured.mesh.mesh.vertex_buffer.slice(..));
                    pass.set_index_buffer(
                        textured.mesh.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..textured.mesh.mesh.index_count, 0, 0..1);
                }
            }
            if !animated_meshes.is_empty() {
                pass.set_pipeline(&self.animated_pipeline);
                pass.set_bind_group(0, &self.camera_bind_group, &[]);
                for (index, animated) in animated_meshes.iter().enumerate() {
                    pass.set_bind_group(1, &animated.mesh.texture_bind_group, &[]);
                    pass.set_bind_group(2, &self.skin_bind_group, &[self.skin_offset(index)]);
                    pass.set_vertex_buffer(0, animated.mesh.vertex_buffer.slice(..));
                    pass.set_index_buffer(
                        animated.mesh.index_buffer.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );
                    pass.draw_indexed(0..animated.mesh.index_count, 0, 0..1);
                }
            }
        }

        if !overlays.is_empty() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("overlay-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });

            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &self.camera_bind_group, &[]);
            pass.set_bind_group(1, &self.material.bind_group, &[]);
            for mesh in overlays {
                pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
                pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..mesh.index_count, 0, 0..1);
            }
        }

        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }

    pub fn size(&self) -> PhysicalSize<u32> {
        self.size
    }

    fn ensure_model_capacity(&mut self, draw_count: usize) {
        let required = draw_count.max(1);
        if required <= self.model_capacity {
            return;
        }

        self.model_capacity = required.next_power_of_two();
        let (model_buffer, model_bind_group) = create_uniform_buffer_and_bind_group(
            &self.device,
            &self.model_layout,
            self.model_stride,
            self.model_capacity,
            model_uniform_size(),
        );
        self.model_buffer = model_buffer;
        self.model_bind_group = model_bind_group;
    }

    fn ensure_skin_capacity(&mut self, draw_count: usize) {
        let required = draw_count.max(1);
        if required <= self.skin_capacity {
            return;
        }

        self.skin_capacity = required.next_power_of_two();
        let (skin_buffer, skin_bind_group) = create_uniform_buffer_and_bind_group(
            &self.device,
            &self.skin_layout,
            self.skin_stride,
            self.skin_capacity,
            skin_uniform_size(),
        );
        self.skin_buffer = skin_buffer;
        self.skin_bind_group = skin_bind_group;
    }

    fn upload_model_uniforms(&self, textured_draws: &[TexturedMeshDraw<'_>]) {
        if textured_draws.is_empty() {
            return;
        }

        let stride = self.model_stride as usize;
        let uniform_size = mem::size_of::<ModelUniform>();
        let mut bytes = vec![0_u8; stride * textured_draws.len()];

        for (index, draw) in textured_draws.iter().enumerate() {
            let uniform = model_uniform(draw.model);
            let offset = index * stride;
            bytes[offset..offset + uniform_size].copy_from_slice(bytemuck::bytes_of(&uniform));
        }

        self.queue.write_buffer(&self.model_buffer, 0, &bytes);
    }

    fn upload_skin_uniforms(&self, animated_meshes: &[AnimatedMeshDraw<'_>]) {
        if animated_meshes.is_empty() {
            return;
        }

        let stride = self.skin_stride as usize;
        let uniform_size = mem::size_of::<SkinUniform>();
        let mut bytes = vec![0_u8; stride * animated_meshes.len()];

        for (index, draw) in animated_meshes.iter().enumerate() {
            let offset = index * stride;
            bytes[offset..offset + uniform_size].copy_from_slice(bytemuck::bytes_of(&draw.uniform));
        }

        self.queue.write_buffer(&self.skin_buffer, 0, &bytes);
    }

    fn model_offset(&self, slot: usize) -> u32 {
        slot.checked_mul(self.model_stride as usize)
            .and_then(|value| u32::try_from(value).ok())
            .expect("model uniform offset fits in u32")
    }

    fn skin_offset(&self, slot: usize) -> u32 {
        slot.checked_mul(self.skin_stride as usize)
            .and_then(|value| u32::try_from(value).ok())
            .expect("skin uniform offset fits in u32")
    }
}

fn device_limits() -> wgpu::Limits {
    #[cfg(target_arch = "wasm32")]
    {
        wgpu::Limits::downlevel_webgl2_defaults()
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        wgpu::Limits::downlevel_defaults()
    }
}

fn model_uniform_size() -> NonZeroU64 {
    NonZeroU64::new(mem::size_of::<ModelUniform>() as u64).expect("model uniform size is non-zero")
}

fn skin_uniform_size() -> NonZeroU64 {
    NonZeroU64::new(mem::size_of::<SkinUniform>() as u64).expect("skin uniform size is non-zero")
}

fn aligned_uniform_size(alignment: u32, uniform_size: u64) -> u64 {
    align_to(uniform_size, u64::from(alignment.max(1)))
}

fn align_to(value: u64, alignment: u64) -> u64 {
    value.div_ceil(alignment) * alignment
}

fn create_uniform_buffer_and_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    stride: u64,
    capacity: usize,
    binding_size: NonZeroU64,
) -> (wgpu::Buffer, wgpu::BindGroup) {
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dynamic-uniform-buffer"),
        size: stride * capacity.max(1) as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("dynamic-uniform-bind-group"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &buffer,
                offset: 0,
                size: Some(binding_size),
            }),
        }],
    });

    (buffer, bind_group)
}

fn model_uniform(model: Mat4) -> ModelUniform {
    ModelUniform {
        model: model.to_cols_array_2d(),
    }
}

fn animated_uniform(model: Mat4, joints: &[Mat4]) -> SkinUniform {
    let mut uniform = SkinUniform {
        model: model.to_cols_array_2d(),
        joints: [Mat4::IDENTITY.to_cols_array_2d(); MAX_SKIN_JOINTS],
    };
    for (index, joint) in joints.iter().take(MAX_SKIN_JOINTS).enumerate() {
        uniform.joints[index] = joint.to_cols_array_2d();
    }
    uniform
}

fn create_instance() -> wgpu::Instance {
    #[cfg(target_arch = "wasm32")]
    {
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::GL,
            ..Default::default()
        })
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        wgpu::Instance::default()
    }
}

fn clamp_surface_size(size: PhysicalSize<u32>, max_extent: u32) -> PhysicalSize<u32> {
    PhysicalSize::new(
        size.width.clamp(1, max_extent),
        size.height.clamp(1, max_extent),
    )
}

fn fill_tile(pixels: &mut [u8], atlas_size: u32, tile_x: u32, tile_y: u32, base: [u8; 4]) {
    let start_x = tile_x * TILE_SIZE;
    let start_y = tile_y * TILE_SIZE;

    if (tile_x, tile_y) == (8, 4) {
        fill_link_tile(pixels, atlas_size, start_x, start_y);
        return;
    }

    if (8..=11).contains(&tile_x) && (0..=3).contains(&tile_y) {
        fill_webcam_tile(pixels, atlas_size, start_x, start_y);
        return;
    }

    if tile_x < 8 && tile_y < 8 {
        let base = tile_color(tile_x / 2, tile_y / 2);
        fill_checker_tile(pixels, atlas_size, start_x, start_y, base);
        return;
    }

    fill_checker_tile(pixels, atlas_size, start_x, start_y, base);
}

fn fill_voxel_texture_tiles(pixels: &mut [u8], atlas_size: u32) {
    for definition in VOXEL_TEXTURE_DEFINITIONS {
        let tile_pixels = parse_ascii_tile_rgba(definition.art, definition.palette)
            .unwrap_or_else(|error| panic!("invalid {} texture asset: {error}", definition.name));
        blit_tile_rgba(
            pixels,
            atlas_size,
            textured_block_tile(definition.block_id),
            &tile_pixels,
        );
    }
}

fn parse_ascii_tile_rgba(
    art: &str,
    palette: &[PaletteEntry],
) -> std::result::Result<Vec<u8>, String> {
    let rows = art
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    if rows.len() != TILE_SIZE as usize {
        return Err(format!("expected {} rows, got {}", TILE_SIZE, rows.len()));
    }

    let mut pixels = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
    for (y, row) in rows.iter().enumerate() {
        let chars = row.chars().collect::<Vec<_>>();
        if chars.len() != TILE_SIZE as usize {
            return Err(format!(
                "expected row {} to have {} columns, got {}",
                y,
                TILE_SIZE,
                chars.len()
            ));
        }

        for (x, ch) in chars.into_iter().enumerate() {
            let color = palette
                .iter()
                .find_map(|(key, color)| (*key == ch).then_some(*color))
                .ok_or_else(|| format!("unknown palette key '{ch}' at column {x}, row {y}"))?;
            pixels.extend_from_slice(&color);
        }
    }

    Ok(pixels)
}

fn blit_tile_rgba(pixels: &mut [u8], atlas_size: u32, tile: (u32, u32), tile_pixels: &[u8]) {
    let expected_len = (TILE_SIZE * TILE_SIZE * 4) as usize;
    if tile_pixels.len() != expected_len {
        return;
    }

    let start_x = tile.0 * TILE_SIZE;
    let start_y = tile.1 * TILE_SIZE;
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let source_offset = ((y * TILE_SIZE + x) * 4) as usize;
            let px = start_x + x;
            let py = start_y + y;
            let dest_offset = ((py * atlas_size + px) * 4) as usize;
            pixels[dest_offset..dest_offset + 4]
                .copy_from_slice(&tile_pixels[source_offset..source_offset + 4]);
        }
    }
}

fn fill_checker_tile(
    pixels: &mut [u8],
    atlas_size: u32,
    start_x: u32,
    start_y: u32,
    base: [u8; 4],
) {
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let px = start_x + x;
            let py = start_y + y;
            let offset = ((py * atlas_size + px) * 4) as usize;
            let checker = ((x / 4) + (y / 4)) % 2;
            let shade = if checker == 0 { 12_i16 } else { -10_i16 };
            pixels[offset..offset + 4].copy_from_slice(&shade_color(base, shade));
        }
    }
}

fn fill_link_tile(pixels: &mut [u8], atlas_size: u32, start_x: u32, start_y: u32) {
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let px = start_x + x;
            let py = start_y + y;
            let offset = ((py * atlas_size + px) * 4) as usize;
            let color = if y < 3 {
                [235, 235, 235, 255]
            } else if y < 6 {
                [66, 133, 244, 255]
            } else if x > 2 && x < 13 && y > 7 && y < 10 {
                [242, 242, 242, 255]
            } else if (x, y) == (5, 11) || (x, y) == (6, 11) {
                [66, 133, 244, 255]
            } else if (x, y) == (7, 11) || (x, y) == (8, 11) {
                [219, 68, 55, 255]
            } else if (x, y) == (9, 11) || (x, y) == (10, 11) {
                [244, 180, 0, 255]
            } else if (x, y) == (11, 11) || (x, y) == (12, 11) {
                [15, 157, 88, 255]
            } else {
                [250, 250, 250, 255]
            };
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn fill_webcam_tile(pixels: &mut [u8], atlas_size: u32, start_x: u32, start_y: u32) {
    for y in 0..TILE_SIZE {
        for x in 0..TILE_SIZE {
            let px = start_x + x;
            let py = start_y + y;
            let offset = ((py * atlas_size + px) * 4) as usize;
            let border = x == 0 || y == 0 || x == TILE_SIZE - 1 || y == TILE_SIZE - 1;
            let color = if border {
                [28, 28, 32, 255]
            } else {
                let u = x as f32 / (TILE_SIZE - 1) as f32;
                let v = y as f32 / (TILE_SIZE - 1) as f32;
                [
                    (40.0 + 130.0 * u) as u8,
                    (60.0 + 110.0 * (1.0 - v)) as u8,
                    (90.0 + 90.0 * v) as u8,
                    255,
                ]
            };
            pixels[offset..offset + 4].copy_from_slice(&color);
        }
    }
}

fn tile_color(tile_x: u32, tile_y: u32) -> [u8; 4] {
    match (tile_x, tile_y) {
        (0, 0) => [110, 76, 45, 255],
        (1, 0) => [104, 168, 72, 255],
        (2, 0) => [128, 132, 140, 255],
        (3, 0) => [219, 201, 132, 255],
        (0, 1) => [112, 78, 53, 255],
        (1, 1) => [68, 130, 58, 255],
        (2, 1) => [192, 228, 240, 255],
        (3, 1) => [228, 189, 90, 255],
        _ => [255, 0, 255, 255],
    }
}

fn shade_color(base: [u8; 4], delta: i16) -> [u8; 4] {
    [
        (i16::from(base[0]) + delta).clamp(0, 255) as u8,
        (i16::from(base[1]) + delta).clamp(0, 255) as u8,
        (i16::from(base[2]) + delta).clamp(0, 255) as u8,
        base[3],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voxel_texture_assets_parse_to_full_rgba_tiles() {
        for definition in VOXEL_TEXTURE_DEFINITIONS {
            let pixels = parse_ascii_tile_rgba(definition.art, definition.palette)
                .unwrap_or_else(|error| panic!("invalid {} texture: {error}", definition.name));
            let tile = textured_block_tile(definition.block_id);

            assert_eq!(
                pixels.len(),
                (TILE_SIZE * TILE_SIZE * 4) as usize,
                "{} texture should fill one 16x16 tile",
                definition.name
            );
            assert!(
                tile.0 < ATLAS_TILES && tile.1 < ATLAS_TILES,
                "{} texture tile should fit in atlas",
                definition.name
            );
            assert!(
                pixels
                    .chunks_exact(4)
                    .any(|pixel| pixel[0] != pixel[1] || pixel[1] != pixel[2]),
                "{} texture should include some color variation",
                definition.name
            );
        }
    }
}
