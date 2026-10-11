use photocraft_color::{BlendMode, Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Fill, Layer, LayerContent, Path, PathOp, ShapeLayer, ShapeStroke, Subpath};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;

fn shape(doc: &Document, name: &str, path: Path, color: Color, stroke: Option<ShapeStroke>) -> Layer {
    let mut shape = ShapeLayer { path, fill: Some(Fill::Solid(color)), stroke, ..Default::default() };
    shape.cache = Some(photocraft_vector::render_shape(&shape, PixelFormat::new(doc.mode, doc.depth, true), doc.bounds()));
    Layer::new(name, LayerContent::Shape(shape))
}
fn fixture(depth: SampleType) -> Document {
    let mut doc = Document::new("Mixed", Size::new(64, 48), ColorMode::Rgb, depth);
    doc.resolution_dpi = 144.0;
    let mut raster = Surface::new(PixelFormat::new(doc.mode, doc.depth, true));
    raster.fill_rect(Rect::new(0, 0, 64, 24), &[0.15, 0.25, 0.7, 1.0]);
    raster.fill_rect(Rect::new(0, 24, 64, 48), &[0.7, 0.2, 0.15, 0.8]);
    doc.layers.push(Layer::new("Pixels", LayerContent::Raster(raster)));
    let path = Path::new(vec![
        Subpath::polygon(&[(8.0, 8.0), (52.0, 8.0), (52.0, 40.0), (8.0, 40.0)]),
        Subpath::polygon(&[(20.0, 16.0), (20.0, 32.0), (40.0, 32.0), (40.0, 16.0)]).with_op(PathOp::Join),
    ]);
    let stroke = ShapeStroke { width: 3.0, paint: Fill::Solid(Color::rgba(0.9, 0.8, 0.1, 0.7)), opacity: 0.8, dashes: vec![3.0, 2.0], ..Default::default() };
    let mut layer = shape(&doc, "Vector & <stroke>", path, Color::rgba(0.2, 0.8, 0.4, 0.6), Some(stroke));
    layer.opacity = 0.65;
    doc.layers.push(layer);
    doc
}
fn equal_pixels(a: &Document, b: &Document) {
    let a = photocraft_compose::flatten(a).to_rgba8();
    let b = photocraft_compose::flatten(b).to_rgba8();
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert!(a.pixels.iter().zip(b.pixels).all(|(a, b)| a.abs_diff(b) <= 2));
}
#[test]
fn actual_paths_and_bitmaps_coexist_and_svg_reopens_as_shapes() {
    for depth in [SampleType::U8, SampleType::U16, SampleType::F32] {
        let original = fixture(depth);
        for ext in ["svg", "svgz", "pdf"] {
            let exported = photocraft_io::export(&original, ext, &Default::default()).unwrap();
            if ext == "pdf" {
                let source = String::from_utf8_lossy(&exported.bytes);
                assert!(source.contains("/Subtype /Form") && source.contains("/Subtype /Image"));
                assert!(source.contains("/Group << /S /Transparency /I true"));
                assert!(source.contains("/MediaBox [0 0 32 24]"));
            } else {
                let reopened = photocraft_io::import(&format!("mixed.{ext}"), &exported.bytes).unwrap().document;
                assert!(reopened.layers.iter().any(|l| matches!(l.content, LayerContent::Shape(_))));
                assert!(reopened.layers.iter().any(|l| matches!(l.content, LayerContent::Raster(_))));
                assert_eq!(reopened.resolution_dpi, original.resolution_dpi);
                equal_pixels(&original, &reopened);
            }
            if let Ok(dir) = std::env::var("PHOTOCRAFT_MIXED_ORACLE_DIR") {
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(std::path::Path::new(&dir).join(format!("mixed-{depth:?}.{ext}")), exported.bytes).unwrap();
                std::fs::write(
                    std::path::Path::new(&dir).join(format!("expected-{depth:?}.png")),
                    photocraft_io::export(&original, "png", &Default::default()).unwrap().bytes,
                )
                .unwrap();
                std::fs::write(std::path::Path::new(&dir).join("mixed.pcraft"), photocraft_format::save_to_bytes(&original, &Default::default()).unwrap())
                    .unwrap();
            }
        }
    }
}
#[test]
fn vector_only_pages_need_no_bitmap_and_hidden_shapes_are_absent() {
    let mut doc = fixture(SampleType::U16);
    doc.layers.remove(0);
    let mut hidden = doc.layers[0].clone();
    hidden.visible = false;
    hidden.name = "Invisible".into();
    doc.layers.push(hidden);
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    let source = String::from_utf8(svg.bytes.clone()).unwrap();
    assert!(source.contains("<path") && !source.contains("<image") && !source.contains("Invisible"));
    let reopened = photocraft_io::import("vector.svg", &svg.bytes).unwrap();
    equal_pixels(&doc, &reopened.document);
    let pdf = photocraft_io::export(&doc, "pdf", &Default::default()).unwrap();
    let source = String::from_utf8_lossy(&pdf.bytes);
    assert!(source.contains("/Subtype /Form") && !source.contains("/Subtype /Image"));
}
#[test]
fn backdrop_dependent_layers_bake_the_prefix_and_keep_later_vectors() {
    let mut doc = fixture(SampleType::U16);
    let mut multiply = doc.layers[0].clone();
    multiply.blend = BlendMode::Multiply;
    multiply.opacity = 0.4;
    doc.layers.push(multiply);
    let top = shape(&doc, "Top vector", Path::new(vec![Subpath::polygon(&[(3.0, 3.0), (10.0, 3.0), (3.0, 10.0)])]), Color::rgb(0.9, 0.1, 0.2), None);
    doc.layers.push(top);
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    assert!(svg.warnings.iter().any(|w| w.contains("underlying stack")));
    let text = String::from_utf8(svg.bytes.clone()).unwrap();
    assert_eq!(text.matches("<path ").count(), 1);
    let reopened = photocraft_io::import("blend.svg", &svg.bytes).unwrap();
    equal_pixels(&doc, &reopened.document);
}
#[test]
fn external_svg_edits_do_not_restore_stale_private_geometry() {
    let doc = fixture(SampleType::U8);
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    let text = String::from_utf8(svg.bytes).unwrap().replace("fill-opacity=\"0.6\"", "fill-opacity=\"0\"");
    if let Ok(reopened) = photocraft_io::import("edited.svg", text.as_bytes()) {
        let a = photocraft_compose::flatten(&doc).to_rgba8();
        let b = photocraft_compose::flatten(&reopened.document).to_rgba8();
        assert_ne!(a.pixels, b.pixels, "must use edited visual attributes");
    }
}
#[test]
fn unsupported_colour_space_and_boolean_geometry_keep_raster_fidelity() {
    let mut doc = fixture(SampleType::U16);
    doc.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    assert!(!String::from_utf8_lossy(&svg.bytes).contains("data-photocraft-mixed"));
    let reopened = photocraft_io::import("linear.svg", &svg.bytes).unwrap().document;
    assert_eq!(reopened.icc_profile, doc.icc_profile);
    equal_pixels(&doc, &reopened);
    doc.icc_profile = None;
    if let LayerContent::Shape(shape) = &mut doc.layers[1].content {
        shape.path.subpaths[1].op = PathOp::Subtract;
    }
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    assert!(!String::from_utf8_lossy(&svg.bytes).contains("data-photocraft-mixed"));
}
#[test]
fn hostile_geometry_dimensions_and_resolution_fail_without_panicking() {
    let doc = fixture(SampleType::U8);
    let svg = String::from_utf8(photocraft_io::export(&doc, "svg", &Default::default()).unwrap().bytes).unwrap();
    for bad in [
        svg.replace("width=\"64\"", "width=\"4294967295\""),
        svg.replace("data-dpi=\"144\"", "data-dpi=\"NaN\""),
        svg.replace("data-depth=\"8\"", "data-depth=\"999\""),
    ] {
        assert!(std::panic::catch_unwind(|| photocraft_io::import("hostile.svg", bad.as_bytes())).unwrap().is_err());
    }
    let mut bad = doc;
    bad.resolution_dpi = f32::NAN;
    for ext in ["svg", "pdf"] {
        assert!(photocraft_io::export(&bad, ext, &Default::default()).is_err());
    }
}

#[test]
fn curved_evenodd_shapes_keep_control_handles_and_complex_groups_bake() {
    use photocraft_doc::{FillRule, Group, Knot};
    use photocraft_geom::Point;
    let mut doc = fixture(SampleType::U16);
    let mut curve = Subpath::polygon(&[(16.0, 8.0), (28.0, 20.0), (16.0, 32.0), (4.0, 20.0)]);
    curve.knots = vec![
        Knot::smooth(Point::new(16.0, 8.0), Point::new(10.0, 8.0), Point::new(22.0, 8.0)),
        Knot::smooth(Point::new(28.0, 20.0), Point::new(28.0, 14.0), Point::new(28.0, 26.0)),
        Knot::smooth(Point::new(16.0, 32.0), Point::new(22.0, 32.0), Point::new(10.0, 32.0)),
        Knot::smooth(Point::new(4.0, 20.0), Point::new(4.0, 26.0), Point::new(4.0, 14.0)),
    ];
    let mut path = Path::new(vec![curve, Subpath::polygon(&[(12.0, 16.0), (20.0, 16.0), (20.0, 24.0), (12.0, 24.0)]).with_op(PathOp::Join)]);
    path.fill_rule = FillRule::EvenOdd;
    let vector = shape(&doc, "Curve", path, Color::rgba(0.9, 0.2, 0.1, 0.7), None);
    doc.layers.push(vector.clone());
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    let source = String::from_utf8(svg.bytes.clone()).unwrap();
    assert!(source.contains("C 22 8 28 14 28 20") && source.contains("fill-rule=\"evenodd\""));
    let reopened = photocraft_io::import("curve.svg", &svg.bytes).unwrap();
    equal_pixels(&doc, &reopened.document);
    let pdf = photocraft_io::export(&doc, "pdf", &Default::default()).unwrap();
    let source = String::from_utf8_lossy(&pdf.bytes);
    assert!(source.contains("22 8 28 14 28 20 c") && source.contains("f*"));
    let mut group = Layer::new("Translucent group", LayerContent::Group(Group { children: vec![vector], expanded: true, artboard: None }));
    group.opacity = 0.5;
    doc.layers.push(group);
    let top = doc.layers[1].clone();
    doc.layers.push(top);
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    assert!(svg.warnings.iter().any(|warning| warning.contains("underlying stack")));
    assert_eq!(String::from_utf8_lossy(&svg.bytes).matches("<path ").count(), 1);
    equal_pixels(&doc, &photocraft_io::import("group.svg", &svg.bytes).unwrap().document);
}

#[test]
fn oversized_layer_lists_and_untrusted_shape_metadata_are_rejected() {
    let mut doc = Document::new("Budget", Size::new(1, 1), ColorMode::Rgb, SampleType::U8);
    doc.layers = (0..4097).map(|_| Layer::new("Fill", LayerContent::Fill(Fill::Solid(Color::WHITE)))).collect();
    assert!(photocraft_io::export(&doc, "pdf", &Default::default()).is_err());
    let svg = photocraft_io::export(&fixture(SampleType::U8), "svg", &Default::default()).unwrap();
    let source = String::from_utf8(svg.bytes).unwrap();
    let oversized = source.replace("Vector &amp; &lt;stroke&gt;", &"a".repeat(70000));
    assert!(photocraft_io::import("hostile.svg", oversized.as_bytes()).is_err());
    assert!(source.contains("&quot;width&quot;:3.0"));
    let dense = source.replace("&quot;width&quot;:3.0", "&quot;width&quot;:0.00000001");
    assert!(photocraft_io::import("dense-dashes.svg", dense.as_bytes()).is_err());
    let stylesheet = format!("<?xml-stylesheet href=\"https://invalid.example/style.css\" type=\"text/css\"?>{source}");
    if let Ok(result) = photocraft_io::import("edited.svg", stylesheet.as_bytes()) {
        // General SVG parsing may proceed; it must never restore stale private shapes.
        assert!(!result.document.layers.iter().any(|layer| layer.name == "Vector & <stroke>"));
    }
}

#[test]
fn bitmap_opacity_remains_float_instead_of_quantizing_each_layer() {
    let mut doc = Document::new("Low opacity", Size::new(32, 32), ColorMode::Rgb, SampleType::U8);
    let mut pixels = Surface::new(PixelFormat::new(doc.mode, doc.depth, true));
    pixels.fill_rect(doc.bounds(), &[0.1, 0.3, 0.9, 1.0]);
    for _ in 0..20 {
        let mut layer = Layer::new("Faint pixels", LayerContent::Raster(pixels.clone()));
        layer.opacity = 0.001;
        layer.fill_opacity = 0.5;
        doc.layers.push(layer);
    }
    doc.layers.push(shape(&doc, "Vector", Path::new(vec![Subpath::polygon(&[(8.0, 8.0), (16.0, 8.0), (8.0, 16.0)])]), Color::rgb(0.2, 0.8, 0.4), None));
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    let source = String::from_utf8(svg.bytes.clone()).unwrap();
    assert_eq!(source.matches("opacity=\"0.0005\"").count(), 20);
    let reopened = photocraft_io::import("faint.svg", &svg.bytes).unwrap().document;
    equal_pixels(&doc, &reopened);
    assert_eq!(reopened.layers[0].opacity, 0.0005);
    assert!(photocraft_compose::flatten(&reopened).to_rgba8().pixels[3] > 0);
    let bad = source.replacen("opacity=\"0.0005\"", "opacity=\"NaN\"", 1);
    assert!(photocraft_io::import("bad.svg", bad.as_bytes()).is_err());
    let pdf = photocraft_io::export(&doc, "pdf", &Default::default()).unwrap();
    assert_eq!(String::from_utf8_lossy(&pdf.bytes).matches("/ca 0.0005 /CA 0.0005").count(), 20);
}

#[test]
fn layer_names_with_xml_noncharacters_roundtrip_as_json_escapes() {
    let mut doc = fixture(SampleType::U8);
    doc.layers[1].name = "Name \u{fffe} \u{ffff} \0".into();
    let svg = photocraft_io::export(&doc, "svg", &Default::default()).unwrap();
    let source = std::str::from_utf8(&svg.bytes).unwrap();
    assert!(!source.contains('\u{fffe}') && !source.contains('\u{ffff}'));
    let reopened = photocraft_io::import("names.svg", &svg.bytes).unwrap().document;
    assert_eq!(reopened.layers[1].name, doc.layers[1].name);
    equal_pixels(&doc, &reopened);
}
