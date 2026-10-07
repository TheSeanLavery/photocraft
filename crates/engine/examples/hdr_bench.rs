//! Release-mode 24 MP CPU fallback/export transform benchmark; not GPU frame time.
use photocraft_color::hdr::HdrWorkbench;
use photocraft_color::{ColorMode, SampleType};
use photocraft_compose::Buffer;
use photocraft_doc::{Document, Size};
use photocraft_geom::Rect;
fn main() {
    if let Err(error) = run() {
        eprintln!("HDR benchmark: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut doc = Document::new("Synthetic HDR ramp", Size::new(6000, 4000), ColorMode::Rgb, SampleType::F32);
    doc.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
    let mut b = Buffer::transparent(Rect::new(0, 0, 6000, 4000));
    for (i, p) in b.px.iter_mut().enumerate() {
        let v = (i % 6000) as f32 / 5999.0 * 4.0;
        *p = [v, v * 0.5, v * 0.25, 1.0];
    }
    let display = photocraft_engine::color_cmds::ColorState::default().canvas_display(&doc)?;
    for method in ["previous clipped SDR", "Natural SDR"] {
        let mut times = Vec::new();
        for _ in 0..3 {
            let start = std::time::Instant::now();
            if method == "previous clipped SDR" {
                std::hint::black_box(display.to_rgba8(&b));
            } else {
                std::hint::black_box(photocraft_engine::hdr_cmds::sdr_buffer(&doc, &b, HdrWorkbench::default())?.to_rgba8());
            }
            times.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        println!("{method}, 24 MP: {times:?} ms");
    }
    Ok(())
}
