//! 24 MP comparison: old sequential history traversal versus one direct destination restore.
use photocraft_color::PixelFormat;
use photocraft_doc::{ColorMode, Document, Layer, LayerContent, SampleType, Size, Surface};
use photocraft_engine::{EngineError, Session};
use photocraft_geom::TileCoord;
use photocraft_ops::History;
use serde_json::json;
use std::time::{Duration, Instant};
fn settle(session: &mut Session) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        session.poll_history_cache();
        if let Some(error) = session.history_storage_error() {
            return Err(error.to_string().into());
        }
        if !session.history_cache_busy() {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("History cache timed out".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::new();
    session.edit_prefs(|prefs| prefs.performance.memory_usage_mb = 128);
    let mut surface = Surface::new(PixelFormat::RGBA8);
    let coords: Vec<_> = (0..16).flat_map(|y| (0..24).map(move |x| TileCoord::new(x, y))).collect();
    for coord in &coords {
        for (i, byte) in surface.tile_mut(*coord).bytes_mut().iter_mut().enumerate() {
            *byte = ((i / 16 + coord.tx as usize * 17 + coord.ty as usize * 31) % 251) as u8;
        }
    }
    let mut document = Document::new("24 MP direct history", Size::new(6000, 4000), ColorMode::Rgb, SampleType::U8);
    document.layers.push(Layer::new("Image", LayerContent::Raster(surface)));
    session.add_document(document, None);
    for step in 0..12 {
        session.edit("Brush paint", |doc, _| {
            let surface = doc.layers.first_mut().and_then(Layer::surface_mut).ok_or(EngineError::NoDocument)?;
            for coord in &coords {
                for byte in surface.tile_mut(*coord).bytes_mut() {
                    *byte = byte.wrapping_add((step % 7 + 1) as u8);
                }
            }
            Ok(())
        })?;
        settle(&mut session)?;
    }
    let active = session.active().ok_or(EngineError::NoDocument)?;
    let checkpoint = active.history.checkpoint();
    let initial = active.doc.clone();
    let archived = active.history.archived_states();
    let started = Instant::now();
    {
        let mut history = History::from_checkpoint(checkpoint)?;
        let mut doc = initial;
        while let Some(previous) = history.try_undo(doc.clone())? {
            doc = previous;
        }
    }
    let serial_ms = started.elapsed().as_secs_f64() * 1000.0;
    let before_revision = session.active().map_or(0, |state| state.revision);
    let started = Instant::now();
    session.execute("history.jump", json!({"state":0}))?;
    let direct_ms = started.elapsed().as_secs_f64() * 1000.0;
    let active = session.active().ok_or(EngineError::NoDocument)?;
    println!(
        "{}",
        json!({"image":"6000x4000","steps":12,"archived":archived,"retained":active.history.timeline().len(),
        "serial_ms":serial_ms,"direct_ms":direct_ms,"revision_delta":active.revision.saturating_sub(before_revision),
        "resident_bytes":session.history_resident_bytes(),"disk_bytes":session.history_disk_bytes(),
        "note":"Sequential baseline omits per-command UI/hooks/cache costs. Synthetic snapshot restore, not native canvas presentation latency."})
    );
    Ok(())
}
