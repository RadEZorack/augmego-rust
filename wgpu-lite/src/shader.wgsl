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
    @location(4) material_id: f32,
};

const MAX_SKIN_JOINTS: u32 = 128u;
const ATLAS_TILE_COUNT: f32 = 12.0;

struct SkinnedVertexInput {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) joints: vec4<f32>,
    @location(4) weights: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) world_position: vec3<f32>,
    @location(4) material_id: f32,
};

@group(1) @binding(0)
var atlas_texture: texture_2d<f32>;

@group(1) @binding(1)
var atlas_sampler: sampler;

struct SkinUniforms {
    model: mat4x4<f32>,
    joints: array<mat4x4<f32>, MAX_SKIN_JOINTS>,
};

@group(2) @binding(0)
var<uniform> skinning: SkinUniforms;

@vertex
fn vs_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    output.clip_position = camera.view_proj * vec4<f32>(input.position, 1.0);
    output.color = input.color;
    output.normal = normalize(input.normal);
    output.uv = input.uv;
    output.world_position = input.position;
    output.material_id = input.material_id;
    return output;
}

@vertex
fn vs_modeled_main(input: VertexInput) -> VertexOutput {
    var output: VertexOutput;
    let world_position = skinning.model * vec4<f32>(input.position, 1.0);
    let world_normal = normalize((skinning.model * vec4<f32>(input.normal, 0.0)).xyz);
    output.clip_position = camera.view_proj * world_position;
    output.color = input.color;
    output.normal = world_normal;
    output.uv = input.uv;
    output.world_position = world_position.xyz;
    output.material_id = input.material_id;
    return output;
}

@vertex
fn vs_skinned_main(input: SkinnedVertexInput) -> VertexOutput {
    var output: VertexOutput;
    var skinned_position = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    var skinned_normal = vec3<f32>(0.0, 0.0, 0.0);
    var total_weight = 0.0;

    if input.weights.x > 0.0 {
        let joint = skinning.joints[u32(input.joints.x)];
        skinned_position += (joint * vec4<f32>(input.position, 1.0)) * input.weights.x;
        skinned_normal += (joint * vec4<f32>(input.normal, 0.0)).xyz * input.weights.x;
        total_weight += input.weights.x;
    }
    if input.weights.y > 0.0 {
        let joint = skinning.joints[u32(input.joints.y)];
        skinned_position += (joint * vec4<f32>(input.position, 1.0)) * input.weights.y;
        skinned_normal += (joint * vec4<f32>(input.normal, 0.0)).xyz * input.weights.y;
        total_weight += input.weights.y;
    }
    if input.weights.z > 0.0 {
        let joint = skinning.joints[u32(input.joints.z)];
        skinned_position += (joint * vec4<f32>(input.position, 1.0)) * input.weights.z;
        skinned_normal += (joint * vec4<f32>(input.normal, 0.0)).xyz * input.weights.z;
        total_weight += input.weights.z;
    }
    if input.weights.w > 0.0 {
        let joint = skinning.joints[u32(input.joints.w)];
        skinned_position += (joint * vec4<f32>(input.position, 1.0)) * input.weights.w;
        skinned_normal += (joint * vec4<f32>(input.normal, 0.0)).xyz * input.weights.w;
        total_weight += input.weights.w;
    }

    var model_position = vec4<f32>(input.position, 1.0);
    var model_normal = input.normal;
    if total_weight > 0.0 {
        model_position = skinned_position / total_weight;
        model_normal = normalize(skinned_normal / total_weight);
    }

    let world_position = skinning.model * model_position;
    let world_normal = normalize((skinning.model * vec4<f32>(model_normal, 0.0)).xyz);

    output.clip_position = camera.view_proj * world_position;
    output.color = vec3<f32>(1.0, 1.0, 1.0);
    output.normal = world_normal;
    output.uv = input.uv;
    output.world_position = world_position.xyz;
    output.material_id = 0.0;
    return output;
}

struct MaterialPalette {
    base: vec3<f32>,
    accent: vec3<f32>,
    accent_amount: f32,
};

fn hash13(value: vec3<f32>) -> f32 {
    let q = vec3<f32>(
        dot(value, vec3<f32>(127.1, 311.7, 74.7)),
        dot(value, vec3<f32>(269.5, 183.3, 246.1)),
        dot(value, vec3<f32>(113.5, 271.9, 124.6)),
    );
    return fract(sin(dot(q, vec3<f32>(1.0, 1.0, 1.0))) * 43758.5453);
}

fn material_palette(material_id: f32) -> MaterialPalette {
    if material_id < 0.5 {
        return MaterialPalette(vec3<f32>(1.0), vec3<f32>(1.0), 0.0);
    }

    if material_id < 1.5 {
        return MaterialPalette(vec3<f32>(0.43, 0.66, 0.29), vec3<f32>(0.36, 0.52, 0.86), 0.05);
    }
    if material_id < 2.5 {
        return MaterialPalette(vec3<f32>(0.47, 0.33, 0.22), vec3<f32>(0.34, 0.22, 0.14), 0.12);
    }
    if material_id < 3.5 {
        return MaterialPalette(vec3<f32>(0.58, 0.58, 0.6), vec3<f32>(0.39, 0.39, 0.43), 0.10);
    }
    if material_id < 4.5 {
        return MaterialPalette(vec3<f32>(0.82, 0.76, 0.52), vec3<f32>(0.67, 0.58, 0.34), 0.10);
    }
    if material_id < 5.5 {
        return MaterialPalette(vec3<f32>(0.38, 0.58, 0.78), vec3<f32>(0.24, 0.43, 0.63), 0.14);
    }
    if material_id < 6.5 {
        return MaterialPalette(vec3<f32>(0.52, 0.38, 0.22), vec3<f32>(0.71, 0.57, 0.36), 0.16);
    }
    if material_id < 7.5 {
        return MaterialPalette(vec3<f32>(0.30, 0.54, 0.24), vec3<f32>(0.82, 0.18, 0.16), 0.07);
    }
    if material_id < 8.5 {
        return MaterialPalette(vec3<f32>(0.72, 0.56, 0.34), vec3<f32>(0.52, 0.38, 0.21), 0.12);
    }
    if material_id < 9.5 {
        return MaterialPalette(vec3<f32>(0.78, 0.88, 0.92), vec3<f32>(0.56, 0.71, 0.78), 0.10);
    }
    if material_id < 10.5 {
        return MaterialPalette(vec3<f32>(0.96, 0.78, 0.36), vec3<f32>(1.0, 0.94, 0.62), 0.30);
    }
    if material_id < 11.5 {
        return MaterialPalette(vec3<f32>(0.60, 0.42, 0.24), vec3<f32>(0.42, 0.28, 0.15), 0.14);
    }
    if material_id < 12.5 {
        return MaterialPalette(vec3<f32>(0.55, 0.55, 0.58), vec3<f32>(0.90, 0.72, 0.20), 0.18);
    }

    return MaterialPalette(vec3<f32>(1.0), vec3<f32>(1.0), 0.0);
}

fn procedural_voxel_albedo(
    world_position: vec3<f32>,
    normal: vec3<f32>,
    uv: vec2<f32>,
    material_id: f32,
) -> vec3<f32> {
    let block_origin = floor(world_position - normal * 0.5);
    let local_uv = uv - vec2<f32>(2.0, 2.0);
    let pixel = floor(local_uv * 16.0);
    let coarse_noise = hash13(block_origin * 0.73 + vec3<f32>(pixel, 0.0));
    let fine_noise = hash13(block_origin * 1.91 + vec3<f32>(pixel.yx, pixel.x + pixel.y));
    let macro_noise = hash13(block_origin * 0.37 + vec3<f32>(pixel * 0.5, pixel.x - pixel.y));
    let accent_noise = hash13(block_origin * 1.43 + vec3<f32>(pixel.y, pixel.x, pixel.x * pixel.y * 0.25));
    let palette = material_palette(material_id);
    let grain = (coarse_noise - 0.5) * 0.34 + (fine_noise - 0.5) * 0.22;
    let patches = (macro_noise - 0.5) * 0.28;
    let face_bias = (hash13(block_origin + normal * 3.17) - 0.5) * 0.10;
    let tint = clamp(1.0 + grain + patches + face_bias, 0.56, 1.46);
    let accent_mask = select(0.0, 1.0, accent_noise > (1.0 - palette.accent_amount));
    let palette_color = mix(palette.base, palette.accent, accent_mask);
    return palette_color * tint;
}

fn textured_voxel_albedo(
    world_position: vec3<f32>,
    normal: vec3<f32>,
    uv: vec2<f32>,
    material_id: f32,
) -> vec3<f32> {
    var albedo = textureSample(atlas_texture, atlas_sampler, uv).rgb;

    if material_id >= 12.5 && material_id < 13.5 {
        let block_origin = floor(world_position - normal * 0.5);
        let local_uv = fract(uv * ATLAS_TILE_COUNT);
        let pixel = floor(local_uv * 16.0);
        let grain_noise = hash13(block_origin * 0.83 + vec3<f32>(pixel, 0.0));
        let face_noise = hash13(block_origin * 1.41 + vec3<f32>(pixel.yx, dot(normal, vec3<f32>(3.0, 5.0, 7.0))));
        let tint = clamp(1.0 + (grain_noise - 0.5) * 0.12 + (face_noise - 0.5) * 0.08, 0.88, 1.12);
        albedo *= tint;
    }

    return albedo;
}

@fragment
fn fs_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let is_textured_voxel = input.material_id >= 12.5 && input.material_id < 13.5;
    let is_procedural_voxel = input.material_id >= 0.5 && !is_textured_voxel;
    var albedo: vec3<f32>;
    if is_textured_voxel {
        albedo = textured_voxel_albedo(input.world_position, input.normal, input.uv, input.material_id);
    } else if is_procedural_voxel {
        albedo = procedural_voxel_albedo(input.world_position, input.normal, input.uv, input.material_id);
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
