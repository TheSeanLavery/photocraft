//! Typed document transactions: complete native metadata plus only new content-addressed tiles/blobs.
use crate::{ProtocolError, Result};
use photocraft_doc::Document;
use photocraft_format::{
    LoadOptions, Manifest, SaveOptions,
    zip::{ZipReader, ZipWriter},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
const MAX_BUNDLE: usize = 512 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DocumentDelta {
    pub base_hash: String,
    pub manifest: Manifest,
    pub base_manifest: Manifest,
    #[serde(with = "crate::wire::resource_bytes")]
    pub base_resources: BTreeMap<String, Vec<u8>>,
    #[serde(with = "crate::wire::resource_bytes")]
    pub resources: BTreeMap<String, Vec<u8>>,
}
#[derive(Clone, Debug)]
pub struct DocumentBundle {
    pub manifest: Manifest,
    pub resources: BTreeMap<String, Vec<u8>>,
}
fn failure(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError(e.to_string())
}
fn valid_resource(name: &str) -> bool {
    let Some(hash) = name.strip_prefix("tiles/").or_else(|| name.strip_prefix("blobs/")).and_then(|n| n.strip_suffix(".zst")) else { return false };
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}
impl DocumentBundle {
    pub fn capture(doc: &Document) -> Result<Self> {
        let mut canonical = doc.clone();
        canonical.id = photocraft_doc::DocId(0);
        canonical.selection = None;
        canonical.quick_mask = None;
        let bytes = photocraft_format::save_to_bytes(&canonical, &SaveOptions::default()).map_err(failure)?;
        if bytes.len() > MAX_BUNDLE {
            return Err(ProtocolError("document transaction exceeds512MiB".into()));
        }
        let reader = ZipReader::new(&bytes).map_err(failure)?;
        let manifest = serde_json::from_slice(&reader.read_by_name("manifest.json", 64 * 1024 * 1024).map_err(failure)?).map_err(failure)?;
        let mut resources = BTreeMap::new();
        for entry in &reader.entries {
            if valid_resource(&entry.name) {
                resources.insert(entry.name.clone(), reader.read(entry, MAX_BUNDLE).map_err(failure)?);
            }
        }
        Ok(Self { manifest, resources })
    }
    pub fn hash(&self) -> Result<String> {
        // Visibility is a per-view preference in personal mode; it cannot invalidate a content transaction.
        let mut manifest = serde_json::to_value(&self.manifest).map_err(failure)?;
        fn normalize(v: &mut serde_json::Value) {
            match v {
                serde_json::Value::Object(o) => {
                    if o.contains_key("visible") {
                        o.insert("visible".into(), serde_json::Value::Bool(true));
                    }
                    for value in o.values_mut() {
                        normalize(value);
                    }
                }
                serde_json::Value::Array(a) => {
                    for value in a {
                        normalize(value);
                    }
                }
                _ => {}
            }
        }
        normalize(&mut manifest);
        Ok(blake3::hash(&serde_json::to_vec(&manifest).map_err(failure)?).to_hex().to_string())
    }
    pub fn load(&self, id: photocraft_doc::DocId) -> Result<Document> {
        let mut zip = ZipWriter::new();
        zip.add("manifest.json", &serde_json::to_vec(&self.manifest).map_err(failure)?).map_err(failure)?;
        let mut referenced = std::collections::BTreeSet::new();
        fn hashes(value: &serde_json::Value, out: &mut std::collections::BTreeSet<String>, depth: usize) -> Result<()> {
            if depth > 128 {
                return Err(ProtocolError("manifest resource nesting exceeds128".into()));
            }
            match value {
                serde_json::Value::String(s) if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) => {
                    out.insert(s.clone());
                }
                serde_json::Value::Array(values) => {
                    for value in values {
                        hashes(value, out, depth + 1)?;
                    }
                }
                serde_json::Value::Object(values) => {
                    for value in values.values() {
                        hashes(value, out, depth + 1)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        hashes(&serde_json::to_value(&self.manifest).map_err(failure)?, &mut referenced, 0)?;
        let mut total = 0usize;
        for (name, bytes) in &self.resources {
            if !valid_resource(name) {
                return Err(ProtocolError("invalid content-addressed resource name".into()));
            }
            let hash = name.strip_prefix("tiles/").or_else(|| name.strip_prefix("blobs/")).and_then(|s| s.strip_suffix(".zst")).unwrap_or_default();
            if !referenced.contains(hash) {
                continue;
            }
            total = total.saturating_add(bytes.len());
            if total > MAX_BUNDLE {
                return Err(ProtocolError("resource memory budget exceeded".into()));
            }
            zip.add(name, bytes).map_err(failure)?;
        }
        let bytes = zip.finish().map_err(failure)?;
        let mut doc = photocraft_format::load_from_bytes_with(
            &bytes,
            &LoadOptions { max_manifest_bytes: 64 * 1024 * 1024, max_blob_bytes: MAX_BUNDLE, max_total_bytes: MAX_BUNDLE as u64, preserve_ids: true },
        )
        .map_err(failure)?;
        doc.id = id;
        Ok(doc)
    }
}
pub fn delta_between(before: &Document, after: &Document) -> Result<DocumentDelta> {
    let before = DocumentBundle::capture(before)?;
    let mut after = DocumentBundle::capture(after)?;
    let base_resources =
        before.resources.iter().filter(|(name, _)| !after.resources.contains_key(*name)).map(|(name, bytes)| (name.clone(), bytes.clone())).collect();
    after.resources.retain(|name, _| !before.resources.contains_key(name));
    Ok(DocumentDelta { base_hash: before.hash()?, manifest: after.manifest, base_manifest: before.manifest, base_resources, resources: after.resources })
}
pub fn apply_delta(doc: &Document, delta: &DocumentDelta) -> Result<Document> {
    apply_delta_rebased(doc, delta, true)
}
/// `strict=false` is reserved for host-resolved dependency replay of an already admitted transaction.
pub fn apply_delta_rebased(doc: &Document, delta: &DocumentDelta, strict: bool) -> Result<Document> {
    let mut bundle = DocumentBundle::capture(doc)?;
    if strict && bundle.hash()? != delta.base_hash {
        return Err(ProtocolError("transaction base changed; resolve against current host document".into()));
    }
    let before = serde_json::to_value(&delta.base_manifest).map_err(failure)?;
    let after = serde_json::to_value(&delta.manifest).map_err(failure)?;
    let current = serde_json::to_value(&bundle.manifest).map_err(failure)?;
    bundle.resources.extend(delta.base_resources.clone());
    bundle.resources.extend(delta.resources.clone());
    let manifest = if strict { after } else { merge_value(&before, &after, &current, &mut bundle.resources, 0)? };
    bundle.manifest = serde_json::from_value(manifest).map_err(failure)?;
    let mut out = bundle.load(doc.id)?;
    out.selection = doc.selection.clone();
    out.quick_mask = doc.quick_mask.clone();
    Ok(out)
}

fn identity(value: &serde_json::Value) -> Option<String> {
    if let Some(id) = value.get("id") {
        return Some(format!("id:{id}"));
    }
    if let (Some(tx), Some(ty)) = (value.get("tx"), value.get("ty")) {
        return Some(format!("tile:{tx}:{ty}"));
    }
    None
}
fn merge_value(
    before: &serde_json::Value,
    after: &serde_json::Value,
    current: &serde_json::Value,
    resources: &mut BTreeMap<String, Vec<u8>>,
    depth: usize,
) -> Result<serde_json::Value> {
    use serde_json::Value;
    if depth > 128 {
        return Err(ProtocolError("document merge nesting exceeds128".into()));
    }
    if before == after {
        return Ok(current.clone());
    }
    if before.get("tiles").is_some() && after.get("tiles").is_some() && current.get("tiles").is_some() {
        return merge_surface(before, after, current, resources);
    }
    if let (Some(b), Some(a), Some(c)) = (before.as_object(), after.as_object(), current.as_object()) {
        let mut out = c.clone();
        for key in b.keys().chain(a.keys()) {
            match (b.get(key), a.get(key)) {
                (Some(b), Some(a)) => {
                    out.insert(key.clone(), merge_value(b, a, c.get(key).unwrap_or(&Value::Null), resources, depth + 1)?);
                }
                (Some(_), None) => {
                    out.remove(key);
                }
                (None, Some(a)) => {
                    out.insert(key.clone(), a.clone());
                }
                _ => {}
            }
        }
        return Ok(Value::Object(out));
    }
    if let (Some(b), Some(a), Some(c)) = (before.as_array(), after.as_array(), current.as_array())
        && b.iter().chain(a).chain(c).all(|v| identity(v).is_some())
    {
        let bm: BTreeMap<_, _> = b.iter().filter_map(|v| identity(v).map(|id| (id, v))).collect();
        let am: BTreeMap<_, _> = a.iter().filter_map(|v| identity(v).map(|id| (id, v))).collect();
        let mut out: Vec<Value> = Vec::new();
        // Keep foreign items in their current positions; reorder this command's own items below.
        for value in c {
            let Some(id) = identity(value) else { continue };
            match (bm.get(&id), am.get(&id)) {
                (Some(b), Some(a)) => out.push(merge_value(b, a, value, resources, depth + 1)?),
                (Some(_), None) => {}
                (None, Some(a)) => out.push((*a).clone()),
                (None, None) => out.push(value.clone()),
            }
        }
        for value in a {
            let Some(id) = identity(value) else { continue };
            if !out.iter().any(|v| identity(v).as_ref() == Some(&id)) && !bm.contains_key(&id) {
                out.push(value.clone());
            }
        }
        let before_order: Vec<_> = b.iter().filter_map(identity).collect();
        let after_order: Vec<_> = a.iter().filter_map(identity).collect();
        if before_order != after_order {
            let positions: Vec<_> = out.iter().enumerate().filter_map(|(i, v)| identity(v).filter(|id| am.contains_key(id)).map(|_| i)).collect();
            let ordered: Vec<_> = after_order.iter().filter_map(|id| out.iter().find(|v| identity(v).as_ref() == Some(id)).cloned()).collect();
            for (position, value) in positions.into_iter().zip(ordered) {
                if let Some(slot) = out.get_mut(position) {
                    *slot = value;
                }
            }
        }
        return Ok(Value::Array(out));
    }
    Ok(after.clone())
}
fn unhex(hex: &str) -> Result<Vec<u8>> {
    if !hex.len().is_multiple_of(2) || hex.len() > 64 {
        return Err(ProtocolError("invalid default pixel".into()));
    }
    hex.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).map_err(failure)?;
            u8::from_str_radix(text, 16).map_err(failure)
        })
        .collect()
}
fn tile_bytes(surface: &photocraft_format::manifest::SurfaceM, coord: (i32, i32), resources: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    use std::io::Read;
    let pixel = unhex(&surface.default)?;
    let expected = (photocraft_geom::TILE_SIZE as usize).saturating_mul(photocraft_geom::TILE_SIZE as usize).saturating_mul(surface.format.bytes_per_pixel());
    if pixel.len() != surface.format.bytes_per_pixel() || expected > 2 << 20 {
        return Err(ProtocolError("invalid tile format".into()));
    }
    let Some(tile) = surface.tiles.iter().find(|t| (t.tx, t.ty) == coord) else {
        return Ok(pixel.repeat(expected / pixel.len()));
    };
    let name = format!("tiles/{}.zst", tile.hash);
    let bytes = resources.get(&name).ok_or_else(|| ProtocolError(format!("missing rebase tile {name}")))?;
    let decoder = ruzstd::decoding::StreamingDecoder::new(bytes.as_slice()).map_err(failure)?;
    let mut out = Vec::with_capacity(expected);
    decoder.take(expected as u64 + 1).read_to_end(&mut out).map_err(failure)?;
    if out.len() != expected || blake3::hash(&out).to_hex().as_str() != tile.hash {
        return Err(ProtocolError("invalid rebase tile hash/size".into()));
    }
    Ok(out)
}
fn merge_surface(
    before: &serde_json::Value,
    after: &serde_json::Value,
    current: &serde_json::Value,
    resources: &mut BTreeMap<String, Vec<u8>>,
) -> Result<serde_json::Value> {
    use photocraft_format::manifest::{SurfaceM, TileRef};
    let b: SurfaceM = serde_json::from_value(before.clone()).map_err(failure)?;
    let a: SurfaceM = serde_json::from_value(after.clone()).map_err(failure)?;
    let mut c: SurfaceM = serde_json::from_value(current.clone()).map_err(failure)?;
    if b.format != a.format || c.format != a.format || b.default != a.default {
        return Ok(after.clone());
    }
    let coords: std::collections::BTreeSet<_> = b.tiles.iter().chain(&a.tiles).map(|t| (t.tx, t.ty)).collect();
    if coords.len() > 8192 {
        return Err(ProtocolError("tile rebase work budget exceeded".into()));
    }
    for coord in coords {
        let bh = b.tiles.iter().find(|t| (t.tx, t.ty) == coord).map(|t| &t.hash);
        let ah = a.tiles.iter().find(|t| (t.tx, t.ty) == coord).map(|t| &t.hash);
        if bh == ah {
            continue;
        }
        let before = tile_bytes(&b, coord, resources)?;
        let after = tile_bytes(&a, coord, resources)?;
        let mut current = tile_bytes(&c, coord, resources)?;
        let sample = a.format.sample.bytes();
        for ((b, a), c) in before.chunks_exact(sample).zip(after.chunks_exact(sample)).zip(current.chunks_exact_mut(sample)) {
            if b != a {
                c.copy_from_slice(a);
            }
        }
        let hash = blake3::hash(&current).to_hex().to_string();
        let compressed = ruzstd::encoding::compress_to_vec(current.as_slice(), ruzstd::encoding::CompressionLevel::Fastest);
        resources.insert(format!("tiles/{hash}.zst"), compressed);
        c.tiles.retain(|t| (t.tx, t.ty) != coord);
        c.tiles.push(TileRef { tx: coord.0, ty: coord.1, hash });
    }
    serde_json::to_value(c).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::{Layer, Size};
    fn document(depth: SampleType) -> Document {
        let mut doc = Document::new("merge", Size::new(16, 16), ColorMode::Rgb, depth);
        doc.layers.push(Layer::raster("base", doc.pixel_format()));
        doc
    }
    fn paint(doc: &mut Document, x: i32, color: [f32; 4]) {
        let layer = doc.layers.first_mut().unwrap();
        layer.surface_mut().unwrap().write_pixel(x, 4, &color);
    }
    #[test]
    fn resource_rebase_preserves_foreign_pixels_and_metadata_at_every_depth() {
        for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
            let mut before = document(depth);
            paint(&mut before, 2, [1., 0., 0., 1.]);
            let mut after = before.clone();
            paint(&mut after, 12, [0., 1., 0., 1.]);
            after.layers.first_mut().unwrap().opacity = 0.5;
            let delta = delta_between(&before, &after).unwrap();
            let mut current = before.clone();
            paint(&mut current, 2, [0., 0., 1., 1.]);
            current.layers.first_mut().unwrap().name = "peer name".into();
            let merged = apply_delta_rebased(&current, &delta, false).unwrap();
            let layer = merged.layers.first().unwrap();
            assert_eq!(layer.name, "peer name");
            assert_eq!(layer.opacity, 0.5);
            let surface = layer.surface().unwrap();
            let mut left = [0.; 4];
            let mut right = [0.; 4];
            surface.read_pixel(2, 4, &mut left);
            surface.read_pixel(12, 4, &mut right);
            assert_eq!(left, [0., 0., 1., 1.]);
            assert_eq!(right, [0., 1., 0., 1.]);
            assert!(apply_delta(&current, &delta).is_err());
        }
    }
    #[test]
    fn absent_unchanged_old_tiles_do_not_restore_an_undone_stroke() {
        let mut before = document(SampleType::U8);
        paint(&mut before, 2, [1., 0., 0., 1.]);
        let mut after = before.clone();
        paint(&mut after, 12, [0., 1., 0., 1.]);
        let delta = delta_between(&before, &after).unwrap();
        let mut current = before.clone();
        current.layers.first_mut().unwrap().surface_mut().unwrap().write_pixel(2, 4, &[0., 0., 0., 0.]);
        let merged = apply_delta_rebased(&current, &delta, false).unwrap();
        let mut pixel = [0.; 4];
        merged.layers.first().unwrap().surface().unwrap().read_pixel(2, 4, &mut pixel);
        assert_eq!(pixel, [0., 0., 0., 0.]);
    }
}
