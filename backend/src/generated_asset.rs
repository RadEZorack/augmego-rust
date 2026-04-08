use anyhow::{Context, Result, anyhow};
use flate2::{Compression, write::GzEncoder};
use gltf::binary::{Glb, Header as GlbHeader};
use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::io::Cursor;
use std::io::Write;

pub fn maybe_gzip_bytes(bytes: &[u8]) -> Result<(Vec<u8>, Option<&'static str>)> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(bytes)
        .context("gzip generated GLB bytes")?;
    let compressed = encoder.finish().context("finalize gzipped generated GLB")?;
    if compressed.len() + 32 >= bytes.len() {
        return Ok((bytes.to_vec(), None));
    }
    Ok((compressed, Some("gzip")))
}

pub fn downscale_glb_embedded_images(
    bytes: &[u8],
    max_dimension: u32,
    jpeg_quality: u8,
) -> Result<Vec<u8>> {
    let glb = Glb::from_slice(bytes).context("parse GLB")?;
    let Some(bin_chunk) = glb.bin.as_ref() else {
        return Ok(bytes.to_vec());
    };
    let mut root: Value =
        serde_json::from_slice(glb.json.as_ref()).context("parse GLB JSON as value")?;
    let mut changed = strip_material_normal_textures(&mut root);

    let Some(original_buffer_views) = root.get("bufferViews").and_then(Value::as_array).cloned()
    else {
        return Ok(bytes.to_vec());
    };

    let Some(original_images) = root.get("images").and_then(Value::as_array).cloned() else {
        return Ok(bytes.to_vec());
    };
    let original_textures = root
        .get("textures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let used_texture_indices = collect_used_texture_indices(&root);
    let (rebuilt_textures, texture_remap, texture_changed) =
        rebuild_textures(&original_textures, &used_texture_indices);
    changed |= texture_changed;
    remap_material_texture_indices(&mut root, &texture_remap)?;
    if let Some(textures) = root.get_mut("textures") {
        *textures = Value::Array(rebuilt_textures.clone());
    }

    let used_image_indices = collect_used_image_indices(&rebuilt_textures);
    let (mut rebuilt_images, image_remap, image_changed) =
        rebuild_images(&original_images, &used_image_indices);
    changed |= image_changed;
    remap_texture_sources(root.get_mut("textures"), &image_remap)?;
    if let Some(images) = root.get_mut("images") {
        *images = Value::Array(rebuilt_images.clone());
    }

    let used_buffer_view_indices = collect_buffer_view_indices(&root);
    let image_buffer_view_map = build_image_buffer_view_map(&rebuilt_images);
    let (rebuilt_buffer_views, rebuilt_bin, buffer_view_remap, buffer_changed) =
        rebuild_buffer_views_and_bin(
            &original_buffer_views,
            bin_chunk,
            &used_buffer_view_indices,
            &image_buffer_view_map,
            max_dimension,
            jpeg_quality,
            &mut rebuilt_images,
        )?;
    changed |= buffer_changed;
    remap_buffer_view_indices(&mut root, &buffer_view_remap)?;
    if let Some(buffer_views) = root.get_mut("bufferViews") {
        *buffer_views = Value::Array(rebuilt_buffer_views);
    }
    if let Some(images) = root.get_mut("images") {
        *images = Value::Array(rebuilt_images);
    }
    if let Some(buffers) = root.get_mut("buffers").and_then(Value::as_array_mut) {
        if let Some(buffer) = buffers.first_mut().and_then(Value::as_object_mut) {
            buffer.insert(
                "byteLength".to_string(),
                Value::from(rebuilt_bin.len() as u64),
            );
            buffer.remove("uri");
        }
    }

    if !changed {
        return Ok(bytes.to_vec());
    }

    let json = serde_json::to_vec(&root).context("serialize optimized GLB JSON")?;
    let rebuilt = Glb {
        header: GlbHeader {
            magic: *b"glTF",
            version: 2,
            length: 0,
        },
        json: Cow::Owned(json),
        bin: Some(Cow::Owned(rebuilt_bin)),
    };
    rebuilt.to_vec().context("serialize optimized GLB")
}

fn strip_material_normal_textures(root: &mut Value) -> bool {
    let Some(materials) = root.get_mut("materials").and_then(Value::as_array_mut) else {
        return false;
    };

    let mut changed = false;
    for material in materials {
        let Some(object) = material.as_object_mut() else {
            continue;
        };
        if object.remove("normalTexture").is_some() {
            changed = true;
        }
    }

    changed
}

fn collect_used_texture_indices(root: &Value) -> Vec<usize> {
    let Some(materials) = root.get("materials").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut used = BTreeSet::new();
    for material in materials {
        collect_texture_indices_from_material_value(material, None, &mut used);
    }

    used.into_iter().collect()
}

fn collect_texture_indices_from_material_value(
    value: &Value,
    parent_key: Option<&str>,
    used: &mut BTreeSet<usize>,
) {
    match value {
        Value::Object(map) => {
            if matches!(
                parent_key,
                Some(
                    "baseColorTexture"
                        | "metallicRoughnessTexture"
                        | "normalTexture"
                        | "occlusionTexture"
                        | "emissiveTexture"
                )
            ) {
                if let Some(index) = map
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    used.insert(index);
                }
            }

            for (key, child) in map {
                collect_texture_indices_from_material_value(child, Some(key.as_str()), used);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_texture_indices_from_material_value(child, parent_key, used);
            }
        }
        _ => {}
    }
}

fn rebuild_textures(
    original_textures: &[Value],
    used_texture_indices: &[usize],
) -> (Vec<Value>, HashMap<usize, usize>, bool) {
    let mut remap = HashMap::new();
    let mut rebuilt = Vec::new();

    for old_index in used_texture_indices {
        let Some(texture) = original_textures.get(*old_index) else {
            continue;
        };
        remap.insert(*old_index, rebuilt.len());
        rebuilt.push(texture.clone());
    }

    let changed = rebuilt.len() != original_textures.len()
        || used_texture_indices
            .iter()
            .enumerate()
            .any(|(new_index, old_index)| *old_index != new_index);

    (rebuilt, remap, changed)
}

fn remap_material_texture_indices(
    root: &mut Value,
    texture_remap: &HashMap<usize, usize>,
) -> Result<()> {
    let Some(materials) = root.get_mut("materials").and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for material in materials {
        remap_material_texture_indices_in_value(material, None, texture_remap)?;
    }

    Ok(())
}

fn remap_material_texture_indices_in_value(
    value: &mut Value,
    parent_key: Option<&str>,
    texture_remap: &HashMap<usize, usize>,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            if matches!(
                parent_key,
                Some(
                    "baseColorTexture"
                        | "metallicRoughnessTexture"
                        | "normalTexture"
                        | "occlusionTexture"
                        | "emissiveTexture"
                )
            ) {
                if let Some(index) = map
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    let new_index = texture_remap
                        .get(&index)
                        .copied()
                        .ok_or_else(|| anyhow!("missing remap for texture index {index}"))?;
                    map.insert("index".to_string(), Value::from(new_index as u64));
                }
            }

            for (key, child) in map.iter_mut() {
                remap_material_texture_indices_in_value(child, Some(key.as_str()), texture_remap)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                remap_material_texture_indices_in_value(child, parent_key, texture_remap)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn collect_used_image_indices(textures: &[Value]) -> Vec<usize> {
    let mut used = BTreeSet::new();

    for texture in textures {
        let Some(source) = texture
            .get("source")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        used.insert(source);
    }

    used.into_iter().collect()
}

fn rebuild_images(
    original_images: &[Value],
    used_image_indices: &[usize],
) -> (Vec<Value>, HashMap<usize, usize>, bool) {
    let mut remap = HashMap::new();
    let mut rebuilt = Vec::new();

    for old_index in used_image_indices {
        let Some(image) = original_images.get(*old_index) else {
            continue;
        };
        remap.insert(*old_index, rebuilt.len());
        rebuilt.push(image.clone());
    }

    let changed = rebuilt.len() != original_images.len()
        || used_image_indices
            .iter()
            .enumerate()
            .any(|(new_index, old_index)| *old_index != new_index);

    (rebuilt, remap, changed)
}

fn remap_texture_sources(
    textures: Option<&mut Value>,
    image_remap: &HashMap<usize, usize>,
) -> Result<()> {
    let Some(textures) = textures.and_then(Value::as_array_mut) else {
        return Ok(());
    };

    for texture in textures {
        let Some(object) = texture.as_object_mut() else {
            continue;
        };
        let Some(source) = object
            .get("source")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        let new_source = image_remap
            .get(&source)
            .copied()
            .ok_or_else(|| anyhow!("missing remap for image index {source}"))?;
        object.insert("source".to_string(), Value::from(new_source as u64));
    }

    Ok(())
}

fn collect_buffer_view_indices(root: &Value) -> Vec<usize> {
    let mut used = BTreeSet::new();
    collect_buffer_view_indices_from_value(root, None, &mut used);
    used.into_iter().collect()
}

fn collect_buffer_view_indices_from_value(
    value: &Value,
    parent_key: Option<&str>,
    used: &mut BTreeSet<usize>,
) {
    match value {
        Value::Object(map) => {
            if matches!(parent_key, Some("bufferView")) {
                if let Some(index) = map
                    .get("bufferView")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    used.insert(index);
                }
            }

            for (key, child) in map {
                if key == "bufferView" {
                    if let Some(index) =
                        child.as_u64().and_then(|value| usize::try_from(value).ok())
                    {
                        used.insert(index);
                    }
                }
                collect_buffer_view_indices_from_value(child, Some(key.as_str()), used);
            }
        }
        Value::Array(values) => {
            for child in values {
                collect_buffer_view_indices_from_value(child, parent_key, used);
            }
        }
        _ => {}
    }
}

fn build_image_buffer_view_map(images: &[Value]) -> HashMap<usize, Vec<usize>> {
    let mut map = HashMap::<usize, Vec<usize>>::new();

    for (image_index, image) in images.iter().enumerate() {
        let Some(buffer_view) = image
            .get("bufferView")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        map.entry(buffer_view).or_default().push(image_index);
    }

    map
}

fn rebuild_buffer_views_and_bin(
    original_buffer_views: &[Value],
    original_bin: &[u8],
    used_buffer_view_indices: &[usize],
    image_buffer_view_map: &HashMap<usize, Vec<usize>>,
    max_dimension: u32,
    jpeg_quality: u8,
    images: &mut [Value],
) -> Result<(Vec<Value>, Vec<u8>, HashMap<usize, usize>, bool)> {
    let mut remap = HashMap::new();
    let mut rebuilt_views = Vec::new();
    let mut rebuilt_bin = Vec::new();
    let mut changed = false;

    for old_index in used_buffer_view_indices {
        let Some(view) = original_buffer_views.get(*old_index) else {
            continue;
        };
        let Some(object) = view.as_object() else {
            continue;
        };
        let Some(byte_length) = object
            .get("byteLength")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
        else {
            continue;
        };
        let byte_offset = object
            .get("byteOffset")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(0);
        let end = byte_offset.saturating_add(byte_length);
        if end > original_bin.len() {
            continue;
        }

        let mut stored_bytes = original_bin[byte_offset..end].to_vec();
        let mut mime_override = None;

        if max_dimension > 0 {
            if let Some(image_indices) = image_buffer_view_map.get(old_index) {
                if !image_indices.is_empty() {
                    if let Some((optimized_image_bytes, optimized_mime)) =
                        optimize_embedded_image(&stored_bytes, max_dimension, jpeg_quality)?
                    {
                        if optimized_image_bytes.len() < stored_bytes.len() {
                            stored_bytes = optimized_image_bytes;
                            mime_override = Some(optimized_mime);
                            changed = true;
                        }
                    }
                }
            }
        }

        let aligned_offset = align_bin_len(&mut rebuilt_bin);
        rebuilt_bin.extend_from_slice(&stored_bytes);

        let mut rebuilt_view = object.clone();
        rebuilt_view.insert("byteOffset".to_string(), Value::from(aligned_offset as u64));
        rebuilt_view.insert(
            "byteLength".to_string(),
            Value::from(stored_bytes.len() as u64),
        );
        if aligned_offset != byte_offset || stored_bytes.len() != byte_length {
            changed = true;
        }

        remap.insert(*old_index, rebuilt_views.len());
        rebuilt_views.push(Value::Object(rebuilt_view));

        if let Some(optimized_mime) = mime_override {
            if let Some(image_indices) = image_buffer_view_map.get(old_index) {
                for image_index in image_indices {
                    if let Some(image) = images.get_mut(*image_index).and_then(Value::as_object_mut)
                    {
                        image.insert("mimeType".to_string(), Value::from(optimized_mime));
                    }
                }
            }
        }
    }

    Ok((rebuilt_views, rebuilt_bin, remap, changed))
}

fn remap_buffer_view_indices(
    value: &mut Value,
    buffer_view_remap: &HashMap<usize, usize>,
) -> Result<()> {
    match value {
        Value::Object(map) => {
            if let Some(buffer_view) = map.get_mut("bufferView") {
                if let Some(old_index) = buffer_view
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                {
                    let new_index = buffer_view_remap
                        .get(&old_index)
                        .copied()
                        .ok_or_else(|| anyhow!("missing remap for bufferView index {old_index}"))?;
                    *buffer_view = Value::from(new_index as u64);
                }
            }

            for child in map.values_mut() {
                remap_buffer_view_indices(child, buffer_view_remap)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                remap_buffer_view_indices(child, buffer_view_remap)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn optimize_embedded_image(
    bytes: &[u8],
    max_dimension: u32,
    jpeg_quality: u8,
) -> Result<Option<(Vec<u8>, &'static str)>> {
    let image = image::load_from_memory(bytes).context("decode embedded GLB image")?;
    let (width, height) = image.dimensions();
    let resized = if width.max(height) > max_dimension {
        image.resize(max_dimension, max_dimension, FilterType::Triangle)
    } else {
        image
    };

    let mut encoded = Vec::new();
    let flattened = DynamicImage::ImageRgb8(resized.to_rgb8());
    flattened
        .write_to(&mut Cursor::new(&mut encoded), ImageFormat::Jpeg)
        .context("encode resized GLB texture")?;

    if encoded.is_empty() {
        return Ok(None);
    }

    if jpeg_quality < 100 {
        let mut jpeg = Vec::new();
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, jpeg_quality.max(1));
        encoder
            .encode_image(&flattened)
            .context("encode optimized GLB texture")?;
        encoded = jpeg;
    }

    Ok(Some((encoded, "image/jpeg")))
}

fn align_bin_len(bin: &mut Vec<u8>) -> usize {
    let aligned = (bin.len() + 3) & !3;
    if aligned > bin.len() {
        bin.resize(aligned, 0);
    }
    aligned
}
