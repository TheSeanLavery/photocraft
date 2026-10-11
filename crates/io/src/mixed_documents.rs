//! Mixed delivery: analytic solid shapes and PNG pixels in the same SVG/PDF page.
//! A backdrop-dependent layer bakes the preceding stack, never just that layer.
use crate::{ExportOptions, ExportResult, IoError};
use base64::Engine;
use photocraft_codecs::{Format, Image};
use photocraft_color::{BlendMode, Color, ColorMode};
use photocraft_doc::vector::{FillRule, LineCap, LineJoin, PathOp, StrokeAlign};
use photocraft_doc::{Document, Fill, Layer, LayerContent, Path, ShapeLayer, ShapeStroke};
use serde::{Deserialize, Serialize};
use std::io::Write;

const MAX_XML: usize = 128 << 20;
const MAX_GEOMETRY: usize = 4 << 20;
const MAX_NODES: usize = 4096;
fn error(message: impl std::fmt::Display) -> IoError {
    IoError::Unsupported(format!("mixed vector/raster export: {message}"))
}
#[derive(Clone, Serialize, Deserialize)]
struct Vector {
    name: String,
    path: Path,
    fill: Option<Fill>,
    stroke: Option<ShapeStroke>,
    opacity: f32,
}
enum Node {
    Vector(Box<Vector>),
    Raster { bytes: Vec<u8>, opacity: f32 },
}
fn push(nodes: &mut Vec<Node>, used: &mut usize, node: Node) -> Result<(), IoError> {
    let cost = match &node {
        Node::Raster { bytes, .. } => bytes.len(),
        Node::Vector(vector) => serde_json::to_vec(vector).map_err(error)?.len(),
    };
    *used = used.checked_add(cost).filter(|size| *size <= 96 << 20).ok_or_else(|| error("scene payload exceeds 96 MiB budget"))?;
    nodes.push(node);
    Ok(())
}
fn common(layer: &Layer) -> bool {
    layer.mask.is_none()
        && layer.vector_mask.is_none()
        && !layer.clipped
        && layer.excluded_channels == 0
        && layer.blend_if.is_default()
        && layer.video.is_none()
        && (!layer.effects.enabled || (layer.effects.items.is_empty() && layer.effects.psd_raw.is_none()))
        && layer.opacity.is_finite()
        && (0.0..=1.0).contains(&layer.opacity)
        && layer.fill_opacity.is_finite()
        && (0.0..=1.0).contains(&layer.fill_opacity)
}
fn paint(fill: &Fill) -> Option<Color> {
    let Fill::Solid(c) = fill else { return None };
    (c.c.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) && c.alpha.is_finite() && (0.0..=1.0).contains(&c.alpha)).then_some(*c)
}
fn valid(vector: &Vector) -> bool {
    !vector.path.inverted
        && vector.name.len() <= 65536
        && vector.opacity.is_finite()
        && (0.0..=1.0).contains(&vector.opacity)
        && vector.path.subpaths.len() <= 16384
        && vector.path.subpaths.iter().enumerate().all(|(i, p)| {
            (i == 0 || p.op == PathOp::Join)
                && p.knots.len() <= 16384
                && p.knots
                    .iter()
                    .all(|k| [k.anchor, k.in_ctrl, k.out_ctrl].iter().all(|p| p.x.is_finite() && p.y.is_finite() && p.x.abs().max(p.y.abs()) <= 1_000_000.0))
        })
        && vector.path.subpaths.iter().map(|p| p.knots.len()).sum::<usize>() <= 16384
        && vector.fill.as_ref().is_none_or(|p| paint(p).is_some())
        && vector
            .stroke
            .as_ref()
            .is_none_or(|s| s.align == StrokeAlign::Center && valid_stroke(s) && paint(&s.paint).is_some() && bounded_dashes(&vector.path, s))
}
fn valid_stroke(s: &ShapeStroke) -> bool {
    s.width.is_finite()
        && (0.0..=1_000_000.0).contains(&s.width)
        && s.opacity.is_finite()
        && (0.0..=1.0).contains(&s.opacity)
        && s.miter_limit.is_finite()
        && (1.0..=500.0).contains(&s.miter_limit)
        && s.dash_offset.is_finite()
        && s.dash_offset.abs() <= 1_000_000.0
        && s.dashes.len() <= 32
        && s.dashes.iter().all(|d| d.is_finite() && (0.0..=10000.0).contains(d))
        && (s.dashes.is_empty() || s.dashes.iter().sum::<f32>() >= 0.01)
}
// A control polygon bounds each cubic's length. Reject pathological dash densities before
// invoking the native shape renderer, including the older fork's larger stroking budget.
fn bounded_dashes(path: &Path, stroke: &ShapeStroke) -> bool {
    if stroke.width == 0.0 || stroke.dashes.is_empty() {
        return true;
    }
    let period = f64::from(stroke.width) * stroke.dashes.iter().map(|v| f64::from(*v)).sum::<f64>();
    if period <= 1e-9 {
        return false;
    }
    let length = path
        .subpaths
        .iter()
        .flat_map(|subpath| subpath.segments())
        .map(|segment| {
            segment
                .windows(2)
                .map(|pair| {
                    let [a, b] = pair else { return 0.0 };
                    (b.x - a.x).hypot(b.y - a.y)
                })
                .sum::<f64>()
        })
        .sum::<f64>();
    length / period * stroke.dashes.len() as f64 <= 50_000.0
}
fn vector(layer: &Layer, doc: &Document) -> Option<Vector> {
    if !common(layer) || layer.blend != BlendMode::Normal || layer.fill_cache.is_some() {
        return None;
    }
    let canvas;
    let shape = match &layer.content {
        LayerContent::Shape(shape) => shape,
        LayerContent::Fill(Fill::Solid(color)) => {
            let (w, h) = (f64::from(doc.size.width), f64::from(doc.size.height));
            canvas = ShapeLayer {
                path: Path::new(vec![photocraft_doc::Subpath::polygon(&[(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)])]),
                fill: Some(Fill::Solid(*color)),
                ..Default::default()
            };
            &canvas
        }
        _ => return None,
    };
    let v = Vector {
        name: layer.name.clone(),
        path: shape.path.clone(),
        fill: shape.fill.clone(),
        stroke: shape.stroke.clone(),
        opacity: layer.opacity * layer.fill_opacity,
    };
    (valid(&v) && serde_json::to_vec(&v).ok()?.len() <= MAX_GEOMETRY).then_some(v)
}
fn flatten_groups<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>, depth: usize) -> Result<(), IoError> {
    if depth > photocraft_doc::MAX_GROUP_DEPTH || out.len().saturating_add(layers.len()) > MAX_NODES {
        return Err(error("layer tree exceeds export budget"));
    }
    for layer in layers {
        if !layer.visible {
            continue;
        }
        if let LayerContent::Group(g) = &layer.content
            && g.artboard.is_none()
            && common(layer)
            && layer.opacity == 1.0
            && layer.fill_opacity == 1.0
            && matches!(layer.blend, BlendMode::Normal | BlendMode::PassThrough)
            && group_safe(&g.children, depth + 1)
        {
            flatten_groups(&g.children, out, depth + 1)?;
        } else {
            out.push(layer);
        }
        if out.len() > MAX_NODES {
            return Err(error("too many export layers"));
        }
    }
    Ok(())
}
fn group_safe(layers: &[Layer], depth: usize) -> bool {
    depth <= photocraft_doc::MAX_GROUP_DEPTH
        && layers.len() <= MAX_NODES
        && layers.iter().all(|l| {
            !l.visible
                || (common(l)
                    && match &l.content {
                        LayerContent::Group(g) => {
                            g.artboard.is_none()
                                && l.opacity == 1.0
                                && l.fill_opacity == 1.0
                                && matches!(l.blend, BlendMode::Normal | BlendMode::PassThrough)
                                && group_safe(&g.children, depth + 1)
                        }
                        LayerContent::Adjustment(_) => false,
                        _ => l.blend == BlendMode::Normal,
                    })
        })
}
fn png(doc: &Document, layers: &[&Layer], options: &ExportOptions, warnings: &mut Vec<String>) -> Result<Vec<u8>, IoError> {
    let mut subset = doc.clone();
    subset.layers = layers.iter().map(|layer| (*layer).clone()).collect();
    let result = crate::flat::export_flat(&subset, Format::Png, options)?;
    for warning in result.warnings {
        if !warnings.contains(&warning) {
            warnings.push(warning);
        }
    }
    Ok(result.bytes)
}
pub(crate) fn export(doc: &Document, ext: &str, options: &ExportOptions) -> Result<Option<ExportResult>, IoError> {
    // SVG paints use sRGB. Converting the individual operands of an arbitrary working-space
    // composite is not equivalent to converting that composite; preserve it as pixels instead.
    if doc.mode != ColorMode::Rgb || doc.icc_profile.as_ref().is_some_and(|p| p.as_slice() != photocraft_cms::Builtin::Srgb.profile().to_bytes().as_slice()) {
        return Ok(None);
    }
    if !doc.resolution_dpi.is_finite() || doc.resolution_dpi <= 0.0 {
        return Err(error("resolution must be finite and positive"));
    }
    let mut layers = Vec::new();
    flatten_groups(&doc.layers, &mut layers, 0)?;
    if !layers.iter().any(|l| vector(l, doc).is_some()) {
        return Ok(None);
    }
    photocraft_codecs::Limits { max_alloc: 256 << 20, ..Default::default() }.check(
        doc.size.width,
        doc.size.height,
        photocraft_codecs::ChannelLayout::Rgba,
        photocraft_codecs::SampleType::U16,
    )?;
    let mut nodes = Vec::new();
    let mut used = 0;
    let mut pending = None;
    let mut warnings = Vec::new();
    for (i, layer) in layers.iter().enumerate() {
        let v = vector(layer, doc);
        let bitmap = common(layer) && layer.blend == BlendMode::Normal && matches!(layer.content, LayerContent::Raster(_));
        if v.is_some() || bitmap {
            if let Some(end) = pending.take() {
                nodes.clear();
                used = 0;
                let prefix = layers.get(..end).ok_or_else(|| error("invalid stack range"))?;
                push(&mut nodes, &mut used, Node::Raster { bytes: png(doc, prefix, options, &mut warnings)?, opacity: 1.0 })?;
            }
            if let Some(v) = v {
                push(&mut nodes, &mut used, Node::Vector(Box::new(v)))?;
            } else {
                let mut opaque = (*layer).clone();
                opaque.opacity = 1.0;
                opaque.fill_opacity = 1.0;
                push(
                    &mut nodes,
                    &mut used,
                    Node::Raster { bytes: png(doc, &[&opaque], options, &mut warnings)?, opacity: layer.opacity * layer.fill_opacity },
                )?;
            }
        } else {
            pending = Some(i + 1);
            warnings.push(format!(
                "Layer '{}' and its underlying stack were rasterized to preserve unsupported paint, text, masks, blending or effects",
                layer.name.chars().take(256).collect::<String>()
            ));
        }
    }
    if let Some(end) = pending {
        nodes.clear();
        used = 0;
        push(
            &mut nodes,
            &mut used,
            Node::Raster { bytes: png(doc, layers.get(..end).ok_or_else(|| error("invalid stack range"))?, options, &mut warnings)?, opacity: 1.0 },
        )?;
    }
    if (ext == "pdf" || nodes.iter().all(|node| matches!(node, Node::Vector(_)))) && doc.metadata != Default::default() {
        warnings.push("Document metadata is omitted by this vector delivery container".into());
    }
    let bytes = if ext == "pdf" { pdf(doc, &nodes, options.encode.embed_icc)? } else { svg(doc, &nodes, ext == "svgz")? };
    Ok(Some(ExportResult { bytes, warnings }))
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\u{fffe}', "\\ufffe").replace('\u{ffff}', "\\uffff")
}
fn paths(path: &Path, svg: bool) -> String {
    let mut out = String::new();
    for subpath in &path.subpaths {
        let Some(first) = subpath.knots.first() else { continue };
        out.push_str(&format!("{} {} {} ", if svg { "M" } else { "" }, first.anchor.x, first.anchor.y));
        if !svg {
            out.push_str("m\n");
        }
        for segment in subpath.segments() {
            let [_, a, b, c] = segment;
            if svg {
                out.push_str("C ");
            }
            out.push_str(&format!("{} {} {} {} {} {} ", a.x, a.y, b.x, b.y, c.x, c.y));
            if !svg {
                out.push_str("c\n");
            }
        }
        if subpath.closed {
            out.push_str(if svg { "Z " } else { "h\n" });
        }
    }
    out
}
fn rgb(color: Color) -> String {
    let [r, g, b] = color.to_rgb();
    format!("rgb({}%,{}%,{}%)", r * 100.0, g * 100.0, b * 100.0)
}
fn svg_attrs(v: &Vector) -> String {
    let fill = v.fill.as_ref().and_then(paint);
    let mut attrs = format!(
        "d=\"{}\" fill=\"{}\" fill-opacity=\"{}\" fill-rule=\"{}\"",
        paths(&v.path, true),
        fill.map_or_else(|| "none".into(), rgb),
        fill.map_or(1.0, |c| c.alpha),
        if v.path.fill_rule == FillRule::EvenOdd { "evenodd" } else { "nonzero" }
    );
    if let Some(s) = v.stroke.as_ref().filter(|s| s.width > 0.0)
        && let Some(color) = paint(&s.paint)
    {
        let cap = match s.cap {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        };
        let join = match s.join {
            LineJoin::Miter => "miter",
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
        };
        let dash = if s.dashes.is_empty() { "none".into() } else { s.dashes.iter().map(|d| (d * s.width).to_string()).collect::<Vec<_>>().join(" ") };
        attrs.push_str(&format!(" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"{}\" stroke-linecap=\"{cap}\" stroke-linejoin=\"{join}\" stroke-miterlimit=\"{}\" stroke-dasharray=\"{dash}\" stroke-dashoffset=\"{}\"",
                rgb(color), color.alpha * s.opacity, s.width, s.miter_limit, s.dash_offset * s.width));
    }
    attrs
}
fn svg(doc: &Document, nodes: &[Node], gzip: bool) -> Result<Vec<u8>, IoError> {
    let (w, h) = (doc.size.width, doc.size.height);
    let depth = if doc.depth == photocraft_color::SampleType::U8 { 8 } else { 16 };
    let mut out = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" data-photocraft-mixed=\"1\" overflow=\"hidden\" data-depth=\"{depth}\" data-dpi=\"{}\" width=\"{w}\" height=\"{h}\" viewBox=\"0 0 {w} {h}\">",
        doc.resolution_dpi
    );
    for node in nodes {
        match node {
            Node::Raster { bytes: png, opacity } => out.push_str(&format!(
                "<image width=\"{w}\" height=\"{h}\" opacity=\"{opacity}\" xlink:href=\"data:image/png;base64,{}\"/>",
                base64::engine::general_purpose::STANDARD.encode(png)
            )),
            Node::Vector(v) => {
                let json = serde_json::to_string(v).map_err(error)?;
                out.push_str(&format!(
                    "<g opacity=\"{}\"><metadata data-photocraft-shape=\"1\">{}</metadata><path {}/></g>",
                    v.opacity,
                    xml(&json),
                    svg_attrs(v)
                ));
            }
        }
        if out.len() > MAX_XML {
            return Err(error("SVG exceeds XML budget"));
        }
    }
    out.push_str("</svg>");
    if out.len() > MAX_XML {
        return Err(error("SVG exceeds XML budget"));
    }
    if gzip {
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(out.as_bytes()).map_err(error)?;
        z.finish().map_err(error)
    } else {
        Ok(out.into_bytes())
    }
}

fn compressed(data: &[u8]) -> Result<Vec<u8>, IoError> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(data).map_err(error)?;
    z.finish().map_err(error)
}
fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}
fn object(objects: &mut Vec<Vec<u8>>, data: Vec<u8>) -> usize {
    objects.push(data);
    objects.len()
}
fn state(alpha: f32) -> Vec<u8> {
    format!("<< /Type /ExtGState /ca {alpha} /CA {alpha} >>").into_bytes()
}
fn pdf(doc: &Document, nodes: &[Node], embed_icc: bool) -> Result<Vec<u8>, IoError> {
    let dpi = f64::from(doc.resolution_dpi);
    if !dpi.is_finite() || dpi <= 0.0 {
        return Err(error("PDF needs finite positive resolution"));
    }
    let (w, h) = (doc.size.width, doc.size.height);
    let (pw, ph) = (f64::from(w) * 72.0 / dpi, f64::from(h) * 72.0 / dpi);
    if pw > 14400.0 || ph > 14400.0 {
        return Err(error("PDF page exceeds 200 inches"));
    }
    let mut objects = vec![b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(), b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(), Vec::new(), Vec::new()];
    let space = if embed_icc {
        let profile = photocraft_cms::Builtin::Srgb.profile().to_bytes();
        let id = object(&mut objects, stream("/N 3 /Alternate /DeviceRGB /Filter /FlateDecode", &compressed(&profile)?));
        format!("[/ICCBased {id} 0 R]")
    } else {
        "/DeviceRGB".into()
    };
    let mut xobjects = String::new();
    let mut states = String::new();
    let mut content = format!("q {} 0 0 {} 0 {ph} cm\n0 0 {w} {h} re W n\n", 72.0 / dpi, -72.0 / dpi);
    for (i, node) in nodes.iter().enumerate() {
        let id = match node {
            Node::Raster { bytes, opacity } => {
                let alpha = object(&mut objects, state(*opacity));
                states.push_str(&format!("/L{i} {alpha} 0 R "));
                let image = photocraft_codecs::decode(bytes)?;
                let image = image.convert(photocraft_codecs::ChannelLayout::Rgba, image.sample_type());
                image_object(&mut objects, &image, &space)?
            }
            Node::Vector(v) => {
                let mut draw = String::new();
                let mut resources = String::new();
                if let Some(c) = v.fill.as_ref().and_then(paint) {
                    let id = object(&mut objects, state(c.alpha));
                    resources.push_str(&format!("/F {id} 0 R "));
                    let [r, g, b] = c.to_rgb();
                    draw.push_str(&format!(
                        "q /F gs /CS cs {r} {g} {b} scn\n{}{}\nQ\n",
                        paths(&v.path, false),
                        if v.path.fill_rule == FillRule::EvenOdd { "f*" } else { "f" }
                    ));
                }
                if let Some(s) = v.stroke.as_ref().filter(|s| s.width > 0.0)
                    && let Some(c) = paint(&s.paint)
                {
                    let id = object(&mut objects, state(c.alpha * s.opacity));
                    resources.push_str(&format!("/S {id} 0 R "));
                    let cap = match s.cap {
                        LineCap::Butt => 0,
                        LineCap::Round => 1,
                        LineCap::Square => 2,
                    };
                    let join = match s.join {
                        LineJoin::Miter => 0,
                        LineJoin::Round => 1,
                        LineJoin::Bevel => 2,
                    };
                    let dashes = s.dashes.iter().map(|d| (d * s.width).to_string()).collect::<Vec<_>>().join(" ");
                    let [r, g, b] = c.to_rgb();
                    draw.push_str(&format!(
                        "q /S gs /CS CS {r} {g} {b} SCN {} w {cap} J {join} j {} M [{dashes}] {} d\n{}S\nQ\n",
                        s.width,
                        s.miter_limit,
                        s.dash_offset * s.width,
                        paths(&v.path, false)
                    ));
                }
                let alpha = object(&mut objects, state(v.opacity));
                states.push_str(&format!("/L{i} {alpha} 0 R "));
                object(
                    &mut objects,
                    stream(
                        &format!(
                            "/Type /XObject /Subtype /Form /FormType 1 /BBox [0 0 {w} {h}] /Group << /S /Transparency /I true /K false /CS {space} >> /Resources << /ColorSpace << /CS {space} >> /ExtGState << {resources} >> >>"
                        ),
                        draw.as_bytes(),
                    ),
                )
            }
        };
        xobjects.push_str(&format!("/N{i} {id} 0 R "));
        match node {
            Node::Raster { .. } => content.push_str(&format!("q /L{i} gs {w} 0 0 -{h} 0 {h} cm /N{i} Do Q\n")),
            Node::Vector(_) => content.push_str(&format!("q /L{i} gs /N{i} Do Q\n")),
        }
    }
    content.push_str("Q\n");
    let page = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {pw} {ph}] /Resources << /XObject << {xobjects} >> /ExtGState << {states} >> >> /Contents 4 0 R >>"
    );
    *objects.get_mut(2).ok_or_else(|| error("missing page object"))? = page.into_bytes();
    *objects.get_mut(3).ok_or_else(|| error("missing content object"))? = stream("", content.as_bytes());
    let mut output = b"%PDF-1.6\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, obj) in objects.iter().enumerate() {
        offsets.push(output.len());
        output.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        output.extend_from_slice(obj);
        output.extend_from_slice(b"\nendobj\n");
        if output.len() > 512 << 20 {
            return Err(error("PDF exceeds output budget"));
        }
    }
    let xref = output.len();
    output.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        output.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    output.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    if output.len() > 512 << 20 {
        return Err(error("PDF exceeds output budget"));
    }
    Ok(output)
}
fn image_object(objects: &mut Vec<Vec<u8>>, image: &Image, space: &str) -> Result<usize, IoError> {
    let mut color = Vec::new();
    let mut alpha = Vec::new();
    let bps = image.sample_type().bytes();
    for pixel in image.data().chunks_exact(4 * bps) {
        for (i, channel) in pixel.chunks_exact(bps).enumerate() {
            let destination = if i == 3 { &mut alpha } else { &mut color };
            if bps == 1 {
                destination.extend_from_slice(channel);
            } else {
                destination.extend_from_slice(&u16::from_ne_bytes(channel.try_into().map_err(error)?).to_be_bytes());
            }
        }
    }
    let bits = bps * 8;
    let (w, h) = image.dimensions();
    let mask = object(
        objects,
        stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent {bits} /Interpolate false /Filter /FlateDecode"
            ),
            &compressed(&alpha)?,
        ),
    );
    Ok(object(
        objects,
        stream(
            &format!(
                "/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace {space} /BitsPerComponent {bits} /Interpolate false /Filter /FlateDecode /SMask {mask} 0 R"
            ),
            &compressed(&color)?,
        ),
    ))
}

/// Our SVG extension contains only the public shape geometry/paint already drawn by the SVG.
/// Reopening it never trusts stale private geometry after an external editor changes the path.
pub(crate) fn import(name: &str, bytes: &[u8]) -> Result<Option<crate::ImportResult>, IoError> {
    let marker = b"data-photocraft-mixed";
    if !bytes.windows(marker.len()).any(|s| s == marker) {
        return Ok(None);
    }
    if bytes.len() > MAX_XML {
        return Err(error("SVG exceeds XML budget"));
    }
    let source = std::str::from_utf8(bytes).map_err(error)?;
    let tree = roxmltree::Document::parse_with_options(source, roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 20000 }).map_err(error)?;
    if tree.descendants().any(|node| node.is_pi()) {
        return Ok(None);
    }
    let root = tree.root_element();
    if !root.has_tag_name(("http://www.w3.org/2000/svg", "svg")) || root.attribute("data-photocraft-mixed") != Some("1") {
        return Ok(None);
    }
    if root.attributes().any(|a| !matches!(a.name(), "data-photocraft-mixed" | "overflow" | "data-depth" | "data-dpi" | "width" | "height" | "viewBox")) {
        return Ok(None);
    }
    if root.attribute("overflow") != Some("hidden") {
        return Ok(None);
    }
    let dimension = |key| root.attribute(key).and_then(|v| v.parse::<u32>().ok()).ok_or_else(|| error("invalid SVG dimensions"));
    let (w, h) = (dimension("width")?, dimension("height")?);
    photocraft_codecs::Limits { max_alloc: 256 << 20, ..Default::default() }.check(
        w,
        h,
        photocraft_codecs::ChannelLayout::Rgba,
        photocraft_codecs::SampleType::U16,
    )?;
    if root.attribute("viewBox") != Some(format!("0 0 {w} {h}").as_str()) {
        return Ok(None);
    }
    let depth = match root.attribute("data-depth") {
        Some("8") => photocraft_color::SampleType::U8,
        Some("16") => photocraft_color::SampleType::U16,
        _ => return Err(error("unsupported SVG pixel depth")),
    };
    let dpi =
        root.attribute("data-dpi").and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite() && *v > 0.0).ok_or_else(|| error("invalid SVG resolution"))?;
    let mut doc = Document::new(name, photocraft_geom::Size::new(w, h), ColorMode::Rgb, depth);
    doc.resolution_dpi = dpi;
    let format = photocraft_color::PixelFormat::new(ColorMode::Rgb, depth, true);
    let mut decoded = 0usize;
    let mut restored_metadata = false;
    for child in root.children().filter(|n| n.is_element()) {
        if doc.layers.len() >= MAX_NODES {
            return Err(error("too many SVG layers"));
        }
        if child.has_tag_name(("http://www.w3.org/2000/svg", "image")) {
            if !matches!(child.attributes().len(), 3 | 4)
                || child
                    .attributes()
                    .any(|a| !matches!((a.namespace(), a.name()), (None, "width" | "height" | "opacity") | (Some("http://www.w3.org/1999/xlink"), "href")))
                || child.children().any(|n| n.is_element())
                || child.attribute("width") != Some(w.to_string().as_str())
                || child.attribute("height") != Some(h.to_string().as_str())
            {
                return Ok(None);
            }
            let opacity = child
                .attribute("opacity")
                .unwrap_or("1")
                .parse::<f32>()
                .ok()
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| error("invalid SVG image opacity"))?;
            let payload = child
                .attribute(("http://www.w3.org/1999/xlink", "href"))
                .and_then(|v| v.strip_prefix("data:image/png;base64,"))
                .ok_or_else(|| error("SVG image must embed PNG"))?;
            let png = base64::engine::general_purpose::STANDARD.decode(payload).map_err(error)?;
            let cost = cache_cost(w, h)?;
            decoded = decoded.checked_add(cost).ok_or_else(|| error("SVG decoded size overflow"))?;
            if decoded > 1 << 30 {
                return Err(error("SVG decoded layers exceed 1 GiB"));
            }
            let pixels = photocraft_codecs::decode_as_with(
                Format::Png,
                &png,
                &photocraft_codecs::DecodeOptions { limits: photocraft_codecs::Limits { max_alloc: 256 << 20, ..Default::default() }, ..Default::default() },
            )?;
            if pixels.dimensions() != (w, h) {
                return Err(error("SVG image dimensions disagree"));
            }
            if pixels.icc.as_ref().is_some_and(|p| p.as_slice() != photocraft_cms::Builtin::Srgb.profile().to_bytes().as_slice()) {
                return Ok(None);
            }
            let pixels = pixels.convert(photocraft_codecs::ChannelLayout::Rgba, pixels.sample_type());
            let imported = crate::flat::image_to_document("Raster", &pixels)?;
            if !restored_metadata {
                doc.icc_profile = imported.document.icc_profile.clone();
                doc.metadata = imported.document.metadata.clone();
                restored_metadata = true;
            }
            let Some(mut layer) = imported.document.layers.into_iter().next() else {
                return Err(error("empty SVG image"));
            };
            if let LayerContent::Raster(surface) = &mut layer.content
                && surface.format() != format
            {
                let region = doc.bounds();
                let values = surface.read_region(region);
                let mut converted = photocraft_raster::Surface::new(format);
                converted.write_region(region, &values);
                *surface = converted;
            }
            layer.opacity = opacity;
            doc.layers.push(layer);
        } else if child.has_tag_name(("http://www.w3.org/2000/svg", "g")) {
            if child.attributes().any(|a| a.name() != "opacity") {
                return Ok(None);
            }
            let children = child.children().filter(|n| n.is_element()).collect::<Vec<_>>();
            let [metadata, path] = children.as_slice() else {
                return Ok(None);
            };
            if !metadata.has_tag_name(("http://www.w3.org/2000/svg", "metadata"))
                || metadata.attribute("data-photocraft-shape") != Some("1")
                || metadata.attributes().len() != 1
                || metadata.children().any(|node| node.is_element())
                || !path.has_tag_name(("http://www.w3.org/2000/svg", "path"))
                || path.children().any(|n| n.is_element())
            {
                return Ok(None);
            }
            let json = metadata.text().ok_or_else(|| error("missing SVG shape geometry"))?;
            // JSON escapes for XML-forbidden noncharacters can add at most the name byte budget.
            if json.len() > MAX_GEOMETRY + 65536 {
                return Err(error("SVG shape geometry exceeds budget"));
            }
            let vector: Vector = serde_json::from_str(json).map_err(error)?;
            if !valid(&vector) {
                return Err(error("unsupported SVG shape geometry"));
            }
            if child.attribute("opacity") != Some(vector.opacity.to_string().as_str()) {
                return Ok(None);
            }
            // Compare every visual attribute, including fill/stroke: edited SVG goes to the
            // general SVG importer rather than silently restoring old editor metadata.
            let expected = format!("<path {}/>", svg_attrs(&vector));
            let expected_tree = roxmltree::Document::parse(&expected).map_err(error)?;
            let expected = expected_tree.root_element();
            if path.attributes().len() != expected.attributes().len() || expected.attributes().any(|a| path.attribute(a.name()) != Some(a.value())) {
                return Ok(None);
            }
            decoded = decoded.checked_add(cache_cost(w, h)?).ok_or_else(|| error("SVG decoded size overflow"))?;
            if decoded > 1 << 30 {
                return Err(error("SVG decoded layers exceed 1 GiB"));
            }
            let mut shape = ShapeLayer { path: vector.path, fill: vector.fill, stroke: vector.stroke, ..Default::default() };
            shape.cache = Some(photocraft_vector::render_shape(&shape, format, doc.bounds()));
            let mut layer = Layer::new(vector.name, LayerContent::Shape(shape));
            layer.opacity = vector.opacity;
            doc.layers.push(layer);
        } else {
            return Ok(None);
        }
    }
    Ok(Some(crate::ImportResult { document: doc, warnings: Vec::new() }))
}

fn cache_cost(w: u32, h: u32) -> Result<usize, IoError> {
    let tile = u64::try_from(photocraft_geom::TILE_SIZE).map_err(error)?;
    if tile == 0 {
        return Err(error("invalid tile size"));
    }
    let bytes = u64::from(w)
        .div_ceil(tile)
        .checked_mul(u64::from(h).div_ceil(tile))
        .and_then(|v| v.checked_mul(tile))
        .and_then(|v| v.checked_mul(tile))
        .and_then(|v| v.checked_mul(8))
        .ok_or_else(|| error("cache allocation overflow"))?;
    usize::try_from(bytes).map_err(error)
}
