//! 24 MP SVG/PDF export benchmark: cargo run --release -p photocraft-io --example mixed_export.
use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Fill, Layer, LayerContent, Path, ShapeLayer, Subpath};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut doc = Document::new("24 MP mixed export", Size::new(6000, 4000), ColorMode::Rgb, SampleType::U8);
    let mut pixels = Surface::new(PixelFormat::new(doc.mode, doc.depth, true));
    pixels.fill_rect(Rect::new(0, 0, 6000, 4000), &[0.15, 0.25, 0.7, 1.0]);
    doc.layers.push(Layer::new("Pixels", LayerContent::Raster(pixels)));
    let mut shape = ShapeLayer {
        path: Path::new(vec![Subpath::polygon(&[(500.0, 500.0), (5500.0, 500.0), (5500.0, 3500.0)])]),
        fill: Some(Fill::Solid(Color::rgba(0.2, 0.8, 0.4, 0.6))),
        ..Default::default()
    };
    shape.cache = Some(photocraft_vector::render_shape(&shape, PixelFormat::new(doc.mode, doc.depth, true), doc.bounds()));
    doc.layers.push(Layer::new("Vector", LayerContent::Shape(shape)));
    for ext in ["svg", "pdf"] {
        let baseline = std::time::Instant::now();
        let surface = photocraft_compose::flatten_to_surface(&doc, PixelFormat::new(doc.mode, doc.depth, true), None);
        let mut flat = doc.clone();
        flat.layers = vec![Layer::new("Composite", LayerContent::Raster(surface))];
        let legacy = photocraft_io::export(&flat, ext, &Default::default())?;
        println!("raster {ext}: {:.0} ms, {} bytes", baseline.elapsed().as_secs_f64() * 1000.0, legacy.bytes.len());
        let now = std::time::Instant::now();
        let result = photocraft_io::export(&doc, ext, &Default::default())?;
        println!("{ext}: {:.0} ms, {} bytes", now.elapsed().as_secs_f64() * 1000.0, result.bytes.len());
    }
    Ok(())
}
