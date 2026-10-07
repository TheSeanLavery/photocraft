//! HDR presentation policy and lossless capture of the window's float framebuffer.
use eframe::egui_wgpu::WgpuConfiguration;

/// Automatic EDR on macOS; `PHOTOCRAFT_HDR=0` forces the original SDR surface.
/// Other platforms retain SDR until their display paths have runtime validation.
pub fn configure(options: &mut WgpuConfiguration, safe_gpu: bool) {
    options.surface.hdr = cfg!(target_os = "macos") && !safe_gpu && std::env::var("PHOTOCRAFT_HDR").as_deref() != Ok("0");
    if let Some(dir) = std::env::var_os("PHOTOCRAFT_HDR_CAPTURE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        // One worker and one queued frame bound memory even if screenshots arrive in a burst.
        let (tx, rx) = std::sync::mpsc::sync_channel::<([u32; 2], Vec<[f32; 4]>)>(1);
        match std::thread::Builder::new().name("HDR capture".into()).spawn(move || {
            for (size, pixels) in rx {
                if let Err(e) = save_capture(&dir, size, &pixels) {
                    log::error!("HDR capture: {e}");
                }
            }
        }) {
            Ok(_) => {
                options.on_hdr_capture = Some(std::sync::Arc::new(move |size, pixels| {
                    if let Err(e) = tx.try_send((size, pixels)) {
                        log::warn!("HDR capture skipped: {e}");
                    }
                }))
            }
            Err(e) => log::error!("HDR capture worker: {e}"),
        }
    }
}

fn save_capture(dir: &std::path::Path, size: [u32; 2], pixels: &[[f32; 4]]) -> Result<(), String> {
    let linear: Vec<f32> = pixels
        .iter()
        .flat_map(|p| {
            [
                photocraft_color::convert::srgb_to_linear(p[0].max(0.0)),
                photocraft_color::convert::srgb_to_linear(p[1].max(0.0)),
                photocraft_color::convert::srgb_to_linear(p[2].max(0.0)),
                p[3],
            ]
        })
        .collect();
    let img = photocraft_codecs::Image::from_f32(size[0], size[1], photocraft_codecs::ChannelLayout::Rgba, &linear).map_err(|e| e.to_string())?;
    let bytes = photocraft_codecs::encode(&img, photocraft_codecs::Format::OpenExr, &Default::default()).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("window-{}-{id}.exr", std::process::id()));
    photocraft_format::atomic_write(&path, &bytes).map_err(|e| e.to_string())?;
    log::info!("HDR framebuffer saved: {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_round_trip_preserves_highlights() {
        let dir = std::env::temp_dir().join(format!("photocraft-hdr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let encoded = photocraft_color::convert::linear_to_srgb(4.0);
        save_capture(&dir, [2, 1], &[[encoded, encoded, encoded, 1.0], [1.0; 4]]).unwrap();
        let path = std::fs::read_dir(&dir).unwrap().next().unwrap().unwrap().path();
        let img = photocraft_codecs::decode(&std::fs::read(path).unwrap()).unwrap();
        let values: Vec<f32> = (0..8).map(|i| img.sample_normalized(i)).collect();
        assert!((values[0] - 4.0).abs() < 0.01, "{values:?}");
        assert!((values[4] - 1.0).abs() < 0.01);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
