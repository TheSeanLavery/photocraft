//! Original synthetic HDR chart for native display validation; no personal photos or assets.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("output EXR path required")?;
    let (w, h) = (960u32, 540u32);
    let mut values = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let v = if y < 180 { [0.0, 0.18, 1.0, 2.0, 4.0].get((x / 192) as usize).copied().unwrap_or(0.0) } else { 4.0 * x as f32 / (w - 1) as f32 };
            let c = if y >= 360 { [v, v * 0.3, v * 0.06, 1.0] } else { [v, v, v, 1.0] };
            values.extend_from_slice(&c);
        }
    }
    let image = photocraft_codecs::Image::from_f32(w, h, photocraft_codecs::ChannelLayout::Rgba, &values)?;
    let bytes = photocraft_codecs::encode(&image, photocraft_codecs::Format::OpenExr, &Default::default())?;
    photocraft_format::atomic_write(std::path::Path::new(&path), &bytes)?;
    println!("wrote synthetic linear-sRGB HDR chart: {path}");
    Ok(())
}
