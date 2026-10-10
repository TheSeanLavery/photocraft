//! Native bounded background reads. One latest result, no filesystem work in `poll`.
use std::{
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

type Update<T> = Result<Option<T>, String>;

pub struct FileWatch<T: Send + 'static> {
    path: PathBuf,
    limit: usize,
    validate: fn(&str) -> Result<T, String>,
    interval: Duration,
    latest: Arc<Mutex<Option<Update<T>>>>,
    epoch: Arc<AtomicU64>,
    stop: Option<mpsc::SyncSender<()>>,
    started: bool,
}

fn read(path: &std::path::Path, limit: usize) -> Result<Option<String>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("theme file: {e}")),
    };
    if !metadata.is_file() {
        return Err("theme path must be a regular file".into());
    }
    if metadata.len() > limit as u64 {
        return Err(format!("theme file exceeds {limit} bytes"));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("theme file: {e}"))?;
    let mut text = String::new();
    file.take(limit.saturating_add(1) as u64).read_to_string(&mut text).map_err(|e| format!("theme file: {e}"))?;
    if text.len() > limit {
        return Err(format!("theme file exceeds {limit} bytes"));
    }
    Ok(Some(text))
}

fn publish<T>(latest: &Mutex<Option<Update<T>>>, epoch: &AtomicU64, generation: u64, update: Update<T>) -> bool {
    let mut slot = latest.lock().unwrap_or_else(|e| e.into_inner());
    if epoch.load(Ordering::SeqCst) != generation {
        return false;
    }
    let retired = slot.replace(update);
    drop(slot);
    drop(retired);
    true
}

impl<T: Send + 'static> FileWatch<T> {
    pub fn new(path: PathBuf, limit: usize, validate: fn(&str) -> Result<T, String>) -> Self {
        Self { path, limit, validate, interval: Duration::from_secs(2), latest: Arc::default(), epoch: Arc::new(AtomicU64::new(0)), stop: None, started: false }
    }

    /// After an atomic local save, discard any earlier read, including one still in progress.
    pub fn acknowledge_save(&mut self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        let retired = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take();
        drop(retired);
    }

    fn start(&mut self, ctx: &egui::Context) {
        self.started = true;
        let (stop, receive) = mpsc::sync_channel(1);
        let (path, limit, validate, interval, latest, epoch, ctx) =
            (self.path.clone(), self.limit, self.validate, self.interval, self.latest.clone(), self.epoch.clone(), ctx.clone());
        let result = std::thread::Builder::new().name("photocraft-theme-watch".into()).spawn(move || {
            let mut previous: Option<Result<Option<String>, String>> = None;
            let mut previous_generation = None;
            loop {
                let generation = epoch.load(Ordering::SeqCst);
                let current = read(&path, limit);
                if previous_generation != Some(generation) || previous.as_ref() != Some(&current) {
                    let update = match &current {
                        Ok(Some(text)) => validate(text).map(Some),
                        Ok(None) => Ok(None),
                        Err(error) => Err(error.clone()),
                    };
                    // Check the generation while owning the slot. A concurrent save either
                    // clears this publication or causes this stale read to be discarded.
                    if publish(&latest, &epoch, generation, update) {
                        previous = Some(current);
                        previous_generation = Some(generation);
                        ctx.request_repaint();
                    }
                }
                match receive.recv_timeout(interval) {
                    Err(mpsc::RecvTimeoutError::Timeout) => (),
                    _ => break,
                }
            }
        });
        match result {
            Ok(_handle) => self.stop = Some(stop), // detached; shutdown never blocks the UI on file I/O
            Err(error) => *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(format!("theme watcher could not start: {error}"))),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> Option<Update<T>> {
        if !self.started {
            self.start(ctx);
        }
        match self.latest.try_lock() {
            Ok(mut slot) => slot.take(),
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner().take(),
            Err(std::sync::TryLockError::WouldBlock) => None,
        }
    }
}

impl<T: Send + 'static> Drop for FileWatch<T> {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.try_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn text(s: &str) -> Result<String, String> {
        if s.starts_with('{') { Ok(s.into()) } else { Err("malformed".into()) }
    }
    fn next<T: Send + 'static>(watch: &mut FileWatch<T>) -> Update<T> {
        let ctx = egui::Context::default();
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(v) = watch.poll(&ctx) {
                return v;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn changed_bytes_errors_and_missing_files_are_delivered_without_a_queue() {
        let path = std::env::temp_dir().join(format!("photocraft-watch-{}-{:?}.json", std::process::id(), std::thread::current().id()));
        let mut watch = FileWatch::new(path.clone(), 64, text);
        watch.interval = Duration::from_millis(10);
        std::fs::write(&path, "{first}").unwrap();
        assert_eq!(next(&mut watch).unwrap(), Some("{first}".into()));
        // A local save changes the UI baseline even if an external edit restores
        // the exact bytes last observed by the worker. Re-read after acknowledgement.
        watch.acknowledge_save();
        assert_eq!(next(&mut watch).unwrap(), Some("{first}".into()));
        std::fs::write(&path, "{other}").unwrap();
        assert_eq!(next(&mut watch).unwrap(), Some("{other}".into()));
        std::fs::write(&path, "bad").unwrap();
        assert!(next(&mut watch).is_err());
        std::thread::sleep(Duration::from_millis(30));
        assert!(watch.poll(&egui::Context::default()).is_none());
        std::fs::write(&path, "x".repeat(65)).unwrap();
        assert!(next(&mut watch).unwrap_err().contains("exceeds"));
        std::fs::remove_file(&path).unwrap();
        assert!(next(&mut watch).unwrap().is_none());
        std::fs::write(&path, "{again}").unwrap();
        assert_eq!(next(&mut watch).unwrap(), Some("{again}".into()));
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn local_save_acknowledgement_discards_an_already_pending_old_result() {
        let path = std::env::temp_dir().join("photocraft-watch-unused.json");
        let mut watch = FileWatch::new(path, 64, text);
        *watch.latest.lock().unwrap() = Some(Ok(Some("old".into())));
        watch.acknowledge_save();
        assert!(watch.latest.lock().unwrap().is_none());
        assert_eq!(watch.epoch.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn stale_reads_cannot_publish_after_a_local_save_and_poll_never_waits() {
        let mut watch = FileWatch::new(PathBuf::from("unused-theme.json"), 64, text);
        watch.started = true;
        let generation = watch.epoch.load(Ordering::SeqCst);
        watch.acknowledge_save();
        assert!(!publish(&watch.latest, &watch.epoch, generation, Ok(Some("stale".into()))));
        let current = watch.epoch.load(Ordering::SeqCst);
        assert!(publish(&watch.latest, &watch.epoch, current, Ok(Some("first".into()))));
        assert!(publish(&watch.latest, &watch.epoch, current, Ok(Some("latest".into()))));
        let slot = watch.latest.clone();
        let held = slot.lock().unwrap();
        assert!(watch.poll(&egui::Context::default()).is_none());
        drop(held);
        assert_eq!(watch.poll(&egui::Context::default()).unwrap().unwrap(), Some("latest".into()));
        assert!(watch.poll(&egui::Context::default()).is_none());
    }
}
