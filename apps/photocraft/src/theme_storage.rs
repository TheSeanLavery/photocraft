//! Native bounded theme storage and shared background watcher callbacks.
use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    rc::Rc,
};

pub(crate) fn read_themes(path: &Path) -> Result<Option<String>, String> {
    read_theme_text(path, photocraft_ui_egui::themes::MAX_LIBRARY_BYTES)
}

pub(crate) fn read_theme_text(path: &Path, limit: usize) -> Result<Option<String>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("theme library must be a regular file".into());
    }
    if metadata.len() > limit as u64 {
        return Err(format!("theme library exceeds {limit} bytes"));
    }
    let mut text = String::new();
    file.take((limit + 1) as u64).read_to_string(&mut text).map_err(|e| e.to_string())?;
    if text.len() > limit {
        return Err(format!("theme library exceeds {limit} bytes"));
    }
    Ok(Some(text))
}

/// Read the native chooser result; cancellation is distinct from a vanished or invalid file.
pub(crate) fn read_selected_theme(path: Option<&Path>) -> Result<Option<String>, String> {
    let Some(path) = path else { return Ok(None) };
    read_theme_text(path, photocraft_ui_egui::theme_pack::MAX_THEME_BYTES)?.ok_or_else(|| "theme file no longer exists".into()).map(Some)
}

/// All library callbacks share a watcher so a successful save cancels stale background reads.
pub(crate) fn theme_services(path: Option<PathBuf>) -> (photocraft_ui_egui::LoadThemesFn, photocraft_ui_egui::SaveTextFn, photocraft_ui_egui::PollThemesFn) {
    let watch = Rc::new(RefCell::new(path.clone().map(|path| {
        photocraft_ui_egui::theme_watch::FileWatch::new(path, photocraft_ui_egui::themes::MAX_LIBRARY_BYTES, photocraft_ui_egui::themes::Catalog::parse)
    })));
    let (load_path, save_path, save_watch, poll_watch) = (path.clone(), path, watch.clone(), watch);
    (
        Box::new(move || read_themes(load_path.as_deref().ok_or("no config directory")?)),
        Box::new(move |text| {
            let mut watch = save_watch.try_borrow_mut().map_err(|_| "theme storage is busy")?;
            photocraft_format::atomic_write(save_path.as_deref().ok_or("no config directory")?, text.as_bytes()).map_err(|e| e.to_string())?;
            if let Some(watch) = watch.as_mut() {
                watch.acknowledge_save();
            }
            Ok(())
        }),
        Box::new(move |ctx| poll_watch.try_borrow_mut().ok().and_then(|mut watch| watch.as_mut().and_then(|watch| watch.poll(ctx)))),
    )
}
