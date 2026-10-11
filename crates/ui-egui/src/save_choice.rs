//! Deferred document saving for native panels that have no format dropdown.
use crate::PhotocraftApp;
use serde_json::{Value, json};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};

pub type DestinationFn = Box<dyn FnMut(&str, &eframe::Frame, &egui::Context, Sender<Option<String>>)>;
type Then = Box<dyn FnOnce(&mut PhotocraftApp, String) -> Result<Value, String>>;
pub(crate) struct Pending {
    suggested: String,
    format: Option<String>,
    answer: Option<Receiver<Option<String>>>,
    then: Then,
}

impl PhotocraftApp {
    pub(crate) fn pick_document_save(
        &mut self,
        suggested: &str,
        then: impl FnOnce(&mut Self, String) -> Result<Value, String> + 'static,
    ) -> Result<Value, String> {
        if !self.services.choose_save_format {
            let path = self.services.pick_save.as_mut().and_then(|f| f(suggested)).ok_or("cancelled")?;
            return then(self, path);
        }
        if self.save_choice.is_some() {
            return Err("a save dialog is already open".into());
        }
        if self.services.save_destination.is_none() {
            return Err("no save destination service".into());
        }
        let format = std::path::Path::new(suggested).extension().and_then(|e| e.to_str()).unwrap_or("psd").to_ascii_lowercase();
        self.save_choice = Some(Pending { suggested: suggested.into(), format: Some(format), answer: None, then: Box::new(then) });
        Ok(json!({"fileDialog": "save"}))
    }

    pub(crate) fn save_as_choice(&mut self, path: Option<String>) -> Result<Value, String> {
        if path.is_some() || !self.services.choose_save_format || self.session.is_enabled("layer.smartObjects.saveContents") {
            return self.save_as(path).map(|(path, warnings)| json!({"path":path, "warnings":warnings}));
        }
        let st = self.session.active().ok_or("no document")?;
        let id = st.doc.id;
        let source = st.path.as_deref().unwrap_or(&st.doc.name);
        let mut suggested = std::path::PathBuf::from(source);
        let ext = suggested.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
        if !crate::save_formats::document_formats().iter().any(|f| f.extensions.contains(&ext.as_str())) {
            suggested.set_extension("psd");
        }
        self.pick_document_save(&suggested.to_string_lossy(), move |app, path| {
            let prior = app.session.active().map(|st| st.doc.id);
            let index = app.session.documents().iter().position(|st| st.doc.id == id).ok_or("document was closed")?;
            app.session.set_active(index);
            let result = app.save_as(Some(path)).map(|(path, warnings)| json!({"path":path,"warnings":warnings}));
            if let Some(index) = prior.and_then(|id| app.session.documents().iter().position(|st| st.doc.id == id)) {
                app.session.set_active(index);
            }
            result
        })
    }

    pub fn save_format_choice(&self) -> Option<&str> {
        self.save_choice.as_ref()?.format.as_deref()
    }

    pub fn choose_save_format(&mut self, extension: Option<&str>) -> Result<Value, String> {
        let p = self.save_choice.as_mut().filter(|p| p.format.is_some()).ok_or("no save format chooser is open")?;
        if let Some(extension) = extension {
            let extension = extension.to_ascii_lowercase();
            if !crate::save_formats::document_formats().iter().any(|f| f.extensions.contains(&extension.as_str())) {
                return Err("unsupported save format".into());
            }
            p.suggested = std::path::Path::new(&p.suggested).with_extension(extension).to_string_lossy().into_owned();
            p.format = None;
            Ok(json!({"fileDialog":"save"}))
        } else {
            self.save_choice = None;
            Ok(json!({"cancelled":true}))
        }
    }

    pub(crate) fn show_save_choice(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(p) = self.save_choice.as_mut() else { return };
        if let Some(format) = p.format.as_mut() {
            if let Some(choice) = crate::save_formats::show_choice(ctx, format)
                && let Err(error) = self.choose_save_format(choice.as_deref())
            {
                self.ui.status = error;
                self.ui.status_error = true;
            }
            return;
        }
        if p.answer.is_none() {
            let (tx, rx) = mpsc::channel();
            p.answer = Some(rx);
            if let Some(show) = self.services.save_destination.as_mut() {
                show(&p.suggested, frame, ctx, tx);
            }
        }
        let answer = match p.answer.as_ref().map(Receiver::try_recv) {
            Some(Err(TryRecvError::Empty)) => return,
            Some(Ok(answer)) => answer,
            _ => None,
        };
        let Some(p) = self.save_choice.take() else { return };
        if let Some(path) = answer
            && let Err(error) = (p.then)(self, path)
        {
            self.ui.status = error;
            self.ui.status_error = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completing_a_copy_does_not_change_working_path_or_saved_revision() {
        let writes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let w = writes.clone();
        let services = crate::Services {
            choose_save_format: true,
            save_destination: Some(Box::new(|_, _, _, _| {})),
            export: Some(Box::new(|_, _, _| Ok((vec![1, 2, 3], Vec::new())))),
            write: Some(Box::new(move |path, _| {
                w.borrow_mut().push(path.to_string());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":4,"height":4})).unwrap();
        app.session.active_mut().unwrap().path = Some("work.pcraft".into());
        let before = app.session.active().unwrap().saved_revision;
        crate::menus::invoke(&mut app, &egui::Context::default(), "file.saveACopy", json!({})).unwrap();
        app.choose_save_format(Some("bmp")).unwrap();
        let pending = app.save_choice.take().unwrap();
        (pending.then)(&mut app, "copy.bmp".into()).unwrap();
        assert_eq!(*writes.borrow(), ["copy.bmp"]);
        assert_eq!(app.session.active().unwrap().path.as_deref(), Some("work.pcraft"));
        assert_eq!(app.session.active().unwrap().saved_revision, before);
    }
    #[test]
    fn chooser_preserves_folder_rejects_import_only_and_cancels_without_writing() {
        let services = crate::Services { choose_save_format: true, save_destination: Some(Box::new(|_, _, _, _| {})), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":4,"height":4})).unwrap();
        app.session.active_mut().unwrap().path = Some("/pics/work.pcraft".into());
        app.save_as_choice(None).unwrap();
        assert!(app.choose_save_format(Some("heic")).is_err());
        assert_eq!(app.save_format_choice(), Some("pcraft"));
        app.choose_save_format(Some("BMP")).unwrap();
        assert_eq!(app.save_choice.as_ref().unwrap().suggested, "/pics/work.bmp");
        app.save_choice = None;
        app.save_as_choice(None).unwrap();
        app.choose_save_format(None).unwrap();
        assert!(app.save_choice.is_none());
        assert_eq!(app.session.active().unwrap().path.as_deref(), Some("/pics/work.pcraft"));
    }
}
