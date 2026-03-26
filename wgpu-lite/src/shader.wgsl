struct Camera {
    view_proj: mat4x4<f32>,
};

@group(0) @binding(0)
var<uniform> camera: Camera;

struct VertexInput {
    @location(0) position: vec3<f32>,
    @location(1) color: vec3<f32>,
    @location(2) normal: vec3<f32>,
    @location(3) uv: vec2<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) world_position: vec3<f32>,
};

@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;

@group(1) @binding(1)
var atlas_sampler: sampler;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.normal = normalize(input.normal);
    output.uv = input.uv;
    output.world_position = input.position;
    return output;
}

fn hash13(value: vec3<f32>) -> f32 {
    let q = vec3<f32>(
        dot(value, vec3<f32>(127.1, 311.7, 74.7)),
        dot(value, vec3<f32>(269.5, 183.3, 246.1)),
        dot(value, vec3<f32>(113.5, 271.9, 124.6)),
    );
    return fract(sin(dot(q, vec3<f32>(1.0, 1.0, 1.0))) * 43758.5453);
}

fn procedural_voxel_albedo(world_position: vec3<f32>, normal: vec3<f32>, uv: vec2<f32>) -> vec3<f32> {
    let block_origin = floor(world_position - normal * 0.5);
    let local_uv = uv - vec2<f32>(2.0, 2.0);
    let pixel = floor(local_uv * 16.0);
    let coarse_noise = hash13(block_origin * 0.73 + vec3<f32>(pixel, 0.0));
    let fine_noise = hash13(block_origin * 1.91 + vec3<f32>(pixel.yx, pixel.x + pixel.y));
    let grain = (coarse_noise - 0.5) * 0.16 + (fine_noise - 0.5) * 0.08;
    let face_bias = (hash13(block_origin + normal * 3.17) - 0.5) * 0.05;
    let tint = clamp(1.0 + grain + face_bias, 0.78, 1.22);
    return vec3<f32>(tint);
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let is_procedural_voxel = input.uv.x >= 2.0 && input.uv.y >= 2.0;
    var albedo: vec3<f32>;
    if is_procedural_voxel {
        albedo = procedural_voxel_albedo(input.world_position, input.normal, input.uv);
    } else {
        albedo = textureSample(atlas_texture, atlas_sampler, input.uv).rgb;
    }
    let sun_dir = normalize(vec3<f32>(0.45, 0.85, 0.3));
    let up_dir = vec3<f32>(0.0, 1.0, 0.0);
    let diffuse = max(dot(input.normal, sun_dir), 0.0);
    let skylight = max(dot(input.normal, up_dir), 0.0);
    let lighting = 0.32 + diffuse * 0.48 + skylight * 0.20;
    let lit_color = albedo * input.color * lighting;
    return vec4<f32>(lit_color, 1.0);
}
