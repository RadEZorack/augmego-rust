use anyhow::{Context, Result, anyhow};
use bytemuck::{Pod, Zeroable};
use glam::Mat4;
use std::mem;
use wgpu::util::DeviceExt;
use winit::{dpi::PhysicalSize, window::Window};

const TILE_SIZE: u32 = 16;
const ATLAS_TILES: u32 = 12;

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
                fill_tile(&mut pixels, atlas_size, tile_x, tile_y, tile_color(tile_x, tile_y));
            }
        }

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
    overlay_pipeline: wgpu::RenderPipeline,
    camera_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    material: MaterialTarget,
    depth: DepthTarget,
}

impl<'a> Renderer<'a> {
    pub async fn new(window: &'a Window) -> Result<Self> {
        let size = window.inner_size();
        let instance = create_instance();
        let surface = instance.create_surface(window).context("create surface")?;
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
        let present_mode = if capabilities.present_modes.contains(&wgpu::PresentMode::Mailbox) {
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

        let material = MaterialTarget::new(&device, &queue);

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pipeline-layout"),
            bind_group_layouts: &[&camera_layout, &material.layout],
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

        let depth = DepthTarget::new(&device, size);

        Ok(Self {
            surface,
            device,
            queue,
            config,
            size,
            max_surface_extent,
            pipeline,
            overlay_pipeline,
            camera_buffer,
            camera_bind_group,
            material,
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
        let vertex_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("mesh-vertex-buffer"),
            contents: bytemuck::cast_slice(vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
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

    pub fn render(&mut self, meshes: &[&Mesh], textured_meshes: &[&TexturedMesh], overlays: &[&Mesh]) -> Result<()> {
        let frame = match self.surface.get_current_texture() {
            Ok(frame) => frame,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                self.surface.configure(&self.device, &self.config);
                self.surface.get_current_texture().context("acquire surface texture after reconfigure")?
            }
            Err(wgpu::SurfaceError::OutOfMemory) => return Err(anyhow!("surface out of memory")),
            Err(wgpu::SurfaceError::Timeout) => return Ok(()),
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
                pass.set_index_buffer(textured.mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..textured.mesh.index_count, 0, 0..1);
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
    PhysicalSize::new(size.width.clamp(1, max_extent), size.height.clamp(1, max_extent))
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

fn fill_checker_tile(pixels: &mut [u8], atlas_size: u32, start_x: u32, start_y: u32, base: [u8; 4]) {
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
