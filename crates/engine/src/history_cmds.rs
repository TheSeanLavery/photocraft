//! Direct, fallible history navigation. Only the selected state is hydrated.
use crate::{CommandSpec, EngineError, Result, Session};
use serde_json::{Value, json};

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let state = s.active().ok_or("no document open")?;
    if s.collaboration.room.is_some() && s.collaboration.document_id == Some(state.doc.id) {
        return Err("Shared documents use personal collaboration undo".into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "history.jump",
            label: "Jump to History State",
            menu: &[],
            shortcut: None,
            params: r#"{"state":stable_state_id}"#,
            enabled,
            run: jump,
            journal: true,
        },
        CommandSpec { id: "history.inspect", label: "Inspect History", menu: &[], shortcut: None, params: "{}", enabled, run: inspect, journal: false },
        CommandSpec {
            id: "history.retryStorage",
            label: "Retry History Storage",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled,
            run: |s, _| {
                s.configure_history_cache();
                Ok(json!({"ok":true}))
            },
            journal: false,
        },
    ]
}

fn inspect(s: &mut Session, _: &Value) -> Result<Value> {
    let state = s.active().ok_or(EngineError::NoDocument)?;
    Ok(json!({"current":state.history.current_id(), "unlimited":s.prefs().performance.unlimited_history,
        "storageError":s.history_storage_error(), "states":state.history.timeline().iter().map(|entry|
        json!({"id":entry.identity.id,"parent":entry.identity.parent,"branch":entry.identity.branch,"action":entry.label,"current":entry.current})).collect::<Vec<_>>() }))
}

fn jump(s: &mut Session, params: &Value) -> Result<Value> {
    jump_on_path(s, params, None)
}

fn jump_on_path(s: &mut Session, params: &Value, branch: Option<u64>) -> Result<Value> {
    let id = params
        .get("state")
        .and_then(Value::as_u64)
        .ok_or_else(|| EngineError::BadParams { cmd: "history.jump".into(), msg: "state must be an unsigned state ID".into() })?;
    let active = s.active().ok_or(EngineError::NoDocument)?;
    let state = active.history.state_by_id(active.doc.clone(), id).ok_or_else(|| EngineError::Other("History state is no longer available".into()))?;
    if active.history.current_id() == id {
        return Ok(json!({"state":id,"changed":false}));
    }
    crate::jobs::run(
        s,
        "Loading history state",
        true,
        move |ctx| {
            ctx.check()?;
            let document = state.load_document().map_err(EngineError::Other)?;
            ctx.check()?;
            Ok(document)
        },
        move |session, document| {
            session.apply_history_destination(id, document, branch)?;
            Ok(json!({"state":id,"changed":true}))
        },
    )
}

/// Keyboard undo/redo use the same single-destination background restore as row jumps.
pub(crate) fn step(s: &mut Session, redo: bool) -> Result<Value> {
    if s.collaboration.room.is_some() && s.active().is_some_and(|st| s.collaboration.document_id == Some(st.doc.id)) {
        return Ok(json!(if redo { s.try_redo()? } else { s.try_undo()? }));
    }
    let id = s.active().and_then(|st| st.history.adjacent_id(redo)).ok_or_else(|| EngineError::Other("No adjacent history state".into()))?;
    let branch = s.active().map(|state| state.history.navigation_branch());
    let result = jump_on_path(s, &json!({"state": id}), branch)?;
    Ok(if result.get("job").is_some() { result } else { json!(true) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{ColorMode, Document, SampleType, Size};
    fn session() -> Session {
        let mut session = Session::new();
        session.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
        session
    }
    #[test]
    fn direct_jump_is_one_revision_and_preserves_alternative_paths() {
        let mut s = session();
        for name in ["Brush paint", "Erase", "New layer"] {
            s.edit(name, |doc, _| {
                doc.name = name.into();
                Ok(())
            })
            .unwrap();
        }
        let before = s.active().unwrap().revision;
        s.execute("history.jump", json!({"state":0})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Original");
        assert_eq!(s.active().unwrap().revision, before + 1);
        s.edit("Move layer", |doc, _| {
            doc.name = "Moved".into();
            Ok(())
        })
        .unwrap();
        let entries = s.execute("history.inspect", json!({})).unwrap();
        assert_eq!(entries["states"].as_array().unwrap().len(), 5);
        s.execute("history.jump", json!({"state":3})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "New layer");
    }
    #[test]
    fn unlimited_defaults_and_bad_params_leave_document_and_history_unchanged() {
        let mut s = session();
        assert!(s.prefs().performance.unlimited_history);
        assert!(s.prefs().scratch_disks.use_available_space);
        assert_eq!(s.active().unwrap().history.max_states, 0);
        for params in [json!({}), json!({"state":-1}), json!({"state":"0"}), json!({"state":u64::MAX})] {
            assert!(s.execute("history.jump", params).is_err());
            assert_eq!(s.active().unwrap().doc.name, "Original");
            assert_eq!(s.active().unwrap().history.timeline().len(), 1);
        }
    }
    #[test]
    fn legacy_explicit_limits_still_select_bounded_mode() {
        let mut s = session();
        s.execute("prefs.set", json!({"values":{"performance.historyStates":5,"scratchDisks.budgetMb":32}})).unwrap();
        assert!(!s.prefs().performance.unlimited_history);
        assert!(!s.prefs().scratch_disks.use_available_space);
        assert_eq!(s.active().unwrap().history.max_states, 5);
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use photocraft_doc::{ColorMode, Document, SampleType, Size};
    use photocraft_ops::ArchivedDocument;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[test]
    fn undo_redo_follows_the_selected_branch_at_a_fork() {
        let mut s = Session::new();
        s.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
        for name in ["Paint", "Erase"] {
            s.edit(name, |doc, _| {
                doc.name = name.into();
                Ok(())
            })
            .unwrap();
        }
        s.execute("edit.undo", json!({})).unwrap();
        s.edit("Move", |doc, _| {
            doc.name = "Moved".into();
            Ok(())
        })
        .unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Original");
        s.execute("edit.redo", json!({})).unwrap();
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc.name, "Moved");
    }
    #[derive(Debug)]
    struct Blocked {
        doc: Arc<Document>,
        release: Arc<AtomicBool>,
    }
    impl ArchivedDocument for Blocked {
        fn load(&self) -> std::result::Result<Arc<Document>, String> {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !self.release.load(Ordering::Relaxed) {
                if std::time::Instant::now() > deadline {
                    return Err("test read timed out".into());
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Ok(self.doc.clone())
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn cold_jump_returns_a_job_without_blocking_and_cancel_keeps_the_document() {
        for cancel in [false, true] {
            let mut s = Session::new();
            s.add_document(Document::new("Original", Size::new(2, 2), ColorMode::Rgb, SampleType::U8), None);
            let original = s.active().unwrap().doc.clone();
            s.edit("Paint", |doc, _| {
                doc.name = "Painted".into();
                Ok(())
            })
            .unwrap();
            let release = Arc::new(AtomicBool::new(false));
            s.active_mut().unwrap().history.replace_resident(&original, Arc::new(Blocked { doc: original.clone(), release: release.clone() }));
            let job = match s.start("history.jump", json!({"state":0})).unwrap() {
                crate::jobs::Started::Job(id) => id,
                _ => panic!("cold jump ran inline"),
            };
            assert_eq!(s.active().unwrap().doc.name, "Painted");
            if cancel {
                s.cancel_job(job);
            }
            release.store(true, Ordering::Relaxed);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while s.active_job().is_some() {
                s.poll_jobs();
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(s.active().unwrap().doc.name, if cancel { "Painted" } else { "Original" });
        }
    }
}
