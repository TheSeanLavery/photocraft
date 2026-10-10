//! Native egui theme editor. Drafts and previews never change saved preferences.
use egui::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{PhotocraftApp, theme_pack::ThemePack, themes};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Manager {
    pub selected_id: String,
    pub json: String,
    pub previewing: bool,
    pub error: Option<String>,
}

fn definition(pack: &ThemePack) -> Result<String, String> {
    serde_json::to_string_pretty(pack).map_err(|e| e.to_string())
}

fn switch(app: &mut PhotocraftApp, ctx: &Context, id: &str) -> Result<(), String> {
    let pack = app.theme_rt.catalog.get(id).ok_or_else(|| format!("unknown theme `{id}`"))?;
    let text = definition(&pack)?;
    restore(app, ctx);
    app.ui.theme_manager = Some(Manager { selected_id: id.into(), json: text, previewing: false, error: None });
    Ok(())
}

fn restore(app: &mut PhotocraftApp, ctx: &Context) {
    if app.theme_rt.preview_preferences.take().is_some() {
        app.theme_rt.applied = None;
        themes::sync(app, ctx);
    }
    if let Some(manager) = app.ui.theme_manager.as_mut() {
        manager.previewing = false;
    }
}

fn draft(app: &PhotocraftApp) -> Result<ThemePack, String> {
    ThemePack::parse(&app.ui.theme_manager.as_ref().ok_or("Theme Manager is not open")?.json)
}

fn preview(app: &mut PhotocraftApp, ctx: &Context) -> Result<(), String> {
    let pack = draft(app)?;
    themes::apply_pack(app, ctx, &pack)?;
    app.theme_rt.preview_preferences = Some((app.session.prefs().interface.theme, app.session.prefs().interface.theme_id.clone()));
    if let Some(manager) = app.ui.theme_manager.as_mut() {
        manager.previewing = true;
        manager.error = None;
    }
    Ok(())
}

fn apply(app: &mut PhotocraftApp, ctx: &Context) -> Result<(), String> {
    let pack = draft(app)?;
    let old_catalog = app.theme_rt.catalog.clone();
    let changed = app.theme_rt.catalog.get(&pack.id).as_ref() != Some(&pack);
    if changed {
        let mut catalog = old_catalog.clone();
        catalog.insert(pack.clone(), true)?;
        themes::store(app, catalog)?;
    }
    if let Err(error) = themes::select(app, ctx, &pack.id) {
        if changed {
            // Catalog and selection have separate files. Roll back the first write if the second fails.
            if let Err(rollback) = themes::store(app, old_catalog.clone()) {
                app.theme_rt.catalog = old_catalog;
                return Err(format!("{error}; theme-library rollback failed: {rollback}"));
            }
        }
        return Err(error);
    }
    if let Some(manager) = app.ui.theme_manager.as_mut() {
        manager.selected_id = pack.id.clone();
        manager.json = definition(&pack)?;
        manager.previewing = false;
        manager.error = None;
    }
    Ok(())
}

pub(crate) fn command(app: &mut PhotocraftApp, ctx: &Context, action: &str, params: &Value) -> Result<Value, String> {
    match action {
        "manager" => {
            let id = match params.get("id") {
                Some(value) => value.as_str().ok_or("`id` must be a string")?.to_owned(),
                None => app.ui.theme_id.clone(),
            };
            switch(app, ctx, &id)?;
        }
        "draft" => {
            let text = params.get("json").and_then(Value::as_str).ok_or("missing theme `json` string")?;
            if text.len() > crate::theme_pack::MAX_THEME_BYTES {
                return Err("theme definition exceeds 64 KiB".into());
            }
            let manager = app.ui.theme_manager.as_mut().ok_or("Theme Manager is not open")?;
            manager.json = text.into();
            manager.error = None;
        }
        "preview" => preview(app, ctx)?,
        "apply" => apply(app, ctx)?,
        "cancel" => {
            restore(app, ctx);
            app.ui.theme_manager = None;
        }
        "duplicate" => {
            let mut pack = draft(app)?;
            let id = (1..=themes::MAX_CUSTOM_THEMES + 1)
                .map(|i| format!("custom.theme-{i}"))
                .find(|id| app.theme_rt.catalog.get(id).is_none())
                .ok_or("no free theme ID")?;
            pack.id = id;
            while pack.name.len() > 120 {
                pack.name.pop();
            }
            pack.name.push_str(" Copy");
            let manager = app.ui.theme_manager.as_mut().ok_or("Theme Manager is not open")?;
            manager.json = definition(&pack)?;
            manager.error = None;
        }
        "reset" => {
            let id = app.ui.theme_manager.as_ref().ok_or("Theme Manager is not open")?.selected_id.clone();
            switch(app, ctx, &id)?;
        }
        _ => return Err(format!("unknown manager action `{action}`")),
    }
    Ok(json!({"manager":app.ui.theme_manager,"selected":app.session.prefs().interface.theme_id,"preview":app.theme_rt.preview_preferences.is_some()}))
}

fn import_text(app: &mut PhotocraftApp, ctx: &Context, text: String) -> Result<(), String> {
    ThemePack::parse(&text)?;
    if app.ui.theme_manager.is_none() {
        let id = app.ui.theme_id.clone();
        switch(app, ctx, &id)?;
    }
    let manager = app.ui.theme_manager.as_mut().ok_or("Theme Manager is not open")?;
    manager.json = text;
    manager.error = None;
    Ok(())
}

pub(crate) fn tick(app: &mut PhotocraftApp, ctx: &Context) {
    let results = app.services.theme_inbox.as_ref().map(|inbox| std::mem::take(&mut *inbox.lock().unwrap_or_else(|e| e.into_inner()))).unwrap_or_default();
    for result in results {
        if let Err(error) = result.and_then(|text| import_text(app, ctx, text)) {
            if let Some(manager) = app.ui.theme_manager.as_mut() {
                manager.error = Some(error);
            } else {
                app.ui.status = format!("Theme import failed: {error}");
            }
        }
    }
}

fn import_file(app: &mut PhotocraftApp, ctx: &Context) -> Result<(), String> {
    let pick = app.services.pick_theme.as_mut().ok_or("theme file picker is unavailable")?;
    if let Some(text) = pick()? {
        import_text(app, ctx, text)?;
    }
    Ok(())
}

fn export_file(app: &mut PhotocraftApp) -> Result<(), String> {
    let pack = draft(app)?;
    let text = definition(&pack)?;
    let suggested = format!("{}.theme.json", pack.id);
    let pick = app.services.pick_save.as_mut().ok_or("save dialog is unavailable")?;
    let Some(path) = pick(&suggested) else { return Ok(()) };
    app.services.write.as_mut().ok_or("file writer is unavailable")?(&path, text.as_bytes())
}

pub(crate) fn show(app: &mut PhotocraftApp, ctx: &Context) {
    let Some(mut manager) = app.ui.theme_manager.take() else { return };
    let mut open = true;
    let mut action = None;
    let mut selected = manager.selected_id.clone();
    let entries: Vec<(String, String)> =
        app.theme_rt.catalog.list().iter().filter_map(|item| Some((item.get("id")?.as_str()?.into(), item.get("name")?.as_str()?.into()))).collect();
    let pairs: Vec<(String, &str)> = entries.iter().map(|(id, name)| (id.clone(), name.as_str())).collect();
    egui::Window::new(tl!("Theme Manager"))
        .id(egui::Id::new("theme-manager"))
        .open(&mut open)
        .collapsible(false)
        .default_pos((ctx.content_rect().center() - egui::vec2(350.0, 260.0)).max(ctx.content_rect().min))
        .default_size(egui::vec2(700.0, 520.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(tl!("Saved theme"));
                crate::widgets::dropdown(ui, "manager-theme", &mut selected, &pairs, 240.0);
                if crate::widgets::secondary_button(ui, tl!("Duplicate"), 0.0).clicked() {
                    action = Some("duplicate");
                }
                if crate::widgets::secondary_button(ui, tl!("Reset Changes"), 0.0).clicked() {
                    action = Some("reset");
                }
            });
            ui.label(tl!("Edit the definition below. Duplicate a built-in theme before changing it."));
            if let Ok(pack) = ThemePack::parse(&manager.json) {
                ui.label(crate::i18n::fmt(tl!("Draft: {name}"), &[("name", &pack.name)]));
            }
            egui::ScrollArea::vertical().max_height(340.0).id_salt("theme-definition-scroll").show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut manager.json)
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY)
                        .desired_rows(18)
                        .char_limit(crate::theme_pack::MAX_THEME_BYTES),
                );
            });
            if manager.json.len() > crate::theme_pack::MAX_THEME_BYTES {
                while manager.json.len() > crate::theme_pack::MAX_THEME_BYTES {
                    manager.json.pop();
                }
                manager.error = Some(tl!("Theme definition exceeds 64 KiB").into());
            }
            if let Some(error) = &manager.error {
                ui.colored_label(crate::theme::Tokens::get(ctx).warning, error);
            }
            if let Some((requested, actual)) = crate::theme_fonts::status(ctx)
                && requested != actual
            {
                ui.label(crate::i18n::fmt(tl!("Font {requested} is unavailable; using {actual}."), &[("requested", &requested), ("actual", &actual)]));
            }
            if manager.previewing {
                ui.label(tl!("Preview is active. Cancel restores your saved theme."));
            }
            ui.horizontal(|ui| {
                if ui.add_enabled(app.services.pick_theme.is_some(), egui::Button::new(tl!("Import…"))).clicked() {
                    action = Some("import-file");
                }
                if ui.add_enabled(app.services.pick_save.is_some() && app.services.write.is_some(), egui::Button::new(tl!("Export…"))).clicked() {
                    action = Some("export-file");
                }
                if crate::widgets::secondary_button(ui, tl!("Copy JSON"), 0.0).clicked() {
                    ctx.copy_text(manager.json.clone());
                }
                if crate::widgets::secondary_button(ui, tl!("Preview"), 0.0).clicked() {
                    action = Some("preview");
                }
                if crate::widgets::secondary_button(ui, tl!("Apply"), 0.0).clicked() {
                    action = Some("apply");
                }
                if crate::widgets::secondary_button(ui, tl!("Cancel"), 0.0).clicked() {
                    action = Some("cancel");
                }
            });
        });
    app.ui.theme_manager = Some(manager);
    let result = if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        command(app, ctx, "cancel", &Value::Null).map(|_| ())
    } else if selected != app.ui.theme_manager.as_ref().map_or("", |m| m.selected_id.as_str()) {
        switch(app, ctx, &selected)
    } else if let Some(action) = action {
        match action {
            "import-file" => import_file(app, ctx),
            "export-file" => export_file(app),
            _ => command(app, ctx, action, &Value::Null).map(|_| ()),
        }
    } else {
        Ok(())
    };
    if let Err(error) = result
        && let Some(manager) = app.ui.theme_manager.as_mut()
    {
        manager.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Services,
        theme::{ThemeKind, Tokens},
    };
    use photocraft_engine::Session;
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
    };

    fn changed_definition(app: &PhotocraftApp) -> String {
        let mut pack = draft(app).unwrap();
        pack.colors.insert("card".into(), "#aabbcc".into());
        definition(&pack).unwrap()
    }

    #[test]
    fn preview_cancel_invalid_edits_and_external_selection_preserve_saved_preferences() {
        let ctx = Context::default();
        let saves = Rc::new(Cell::new(0));
        let count = saves.clone();
        let mut app = PhotocraftApp::new(
            Session::new(),
            Services {
                save_prefs: Some(Box::new(move |_| {
                    count.set(count.get() + 1);
                    Ok(())
                })),
                ..Default::default()
            },
        );
        themes::sync(&mut app, &ctx);
        let prefs = app.session.prefs().clone();
        let before = Tokens::get(&ctx);
        command(&mut app, &ctx, "manager", &Value::Null).unwrap();
        let text = changed_definition(&app);
        command(&mut app, &ctx, "draft", &json!({"json":text})).unwrap();
        command(&mut app, &ctx, "preview", &Value::Null).unwrap();
        themes::sync(&mut app, &ctx);
        assert_eq!(Tokens::get(&ctx).card, egui::Color32::from_rgb(170, 187, 204));
        assert_eq!(*app.session.prefs(), prefs);
        assert_eq!(saves.get(), 0);
        command(&mut app, &ctx, "draft", &json!({"json":"{partial"})).unwrap();
        assert!(command(&mut app, &ctx, "preview", &Value::Null).is_err());
        assert_eq!(Tokens::get(&ctx).card, egui::Color32::from_rgb(170, 187, 204));
        command(&mut app, &ctx, "cancel", &Value::Null).unwrap();
        assert_eq!(Tokens::get(&ctx), before);
        assert!(app.ui.theme_manager.is_none());
        command(&mut app, &ctx, "manager", &Value::Null).unwrap();
        command(&mut app, &ctx, "draft", &json!({"json":text})).unwrap();
        command(&mut app, &ctx, "preview", &Value::Null).unwrap();
        themes::select(&mut app, &ctx, ThemeKind::Classic.id()).unwrap();
        themes::sync(&mut app, &ctx);
        assert!(!app.ui.theme_manager.as_ref().unwrap().previewing);
        assert_eq!(app.ui.theme_id, ThemeKind::Classic.id());
    }

    #[test]
    fn apply_duplicates_builtins_and_rolls_back_a_library_write_if_selection_fails() {
        let ctx = Context::default();
        let library = Rc::new(RefCell::new(String::new()));
        let saved = library.clone();
        let mut app = PhotocraftApp::new(
            Session::new(),
            Services {
                save_themes: Some(Box::new(move |s| {
                    *saved.borrow_mut() = s.into();
                    Ok(())
                })),
                ..Default::default()
            },
        );
        themes::sync(&mut app, &ctx);
        command(&mut app, &ctx, "manager", &Value::Null).unwrap();
        let text = changed_definition(&app);
        command(&mut app, &ctx, "draft", &json!({"json":text})).unwrap();
        assert!(command(&mut app, &ctx, "apply", &Value::Null).is_err());
        command(&mut app, &ctx, "duplicate", &Value::Null).unwrap();
        let id = draft(&app).unwrap().id;
        app.services.save_prefs = Some(Box::new(|_| Err("disk full".into())));
        assert!(command(&mut app, &ctx, "apply", &Value::Null).is_err());
        assert!(app.theme_rt.catalog.get(&id).is_none());
        assert!(themes::Catalog::parse(&library.borrow()).unwrap().get(&id).is_none());
        assert_eq!(app.session.prefs().interface.theme_id, ThemeKind::ProMedium.id());
        app.services.save_prefs = Some(Box::new(|_| Ok(())));
        command(&mut app, &ctx, "apply", &Value::Null).unwrap();
        assert_eq!(app.session.prefs().interface.theme_id, id);
        assert!(app.theme_rt.catalog.get(&id).is_some());
        assert!(!app.ui.theme_manager.as_ref().unwrap().previewing);
        command(&mut app, &ctx, "reset", &Value::Null).unwrap();
        assert_eq!(draft(&app).unwrap(), app.theme_rt.catalog.get(&id).unwrap());
    }

    #[test]
    fn imported_files_and_async_results_are_drafts_and_export_is_a_valid_portable_pack() {
        let ctx = Context::default();
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        themes::sync(&mut app, &ctx);
        command(&mut app, &ctx, "manager", &Value::Null).unwrap();
        command(&mut app, &ctx, "duplicate", &Value::Null).unwrap();
        let pack = draft(&app).unwrap();
        let text = definition(&pack).unwrap();
        let chosen = text.clone();
        app.services.pick_theme = Some(Box::new(move || Ok(Some(chosen.clone()))));
        import_file(&mut app, &ctx).unwrap();
        assert!(app.theme_rt.catalog.get(&pack.id).is_none());
        let bytes = Rc::new(RefCell::new(Vec::new()));
        let written = bytes.clone();
        app.services.pick_save = Some(Box::new(|name| Some(name.into())));
        app.services.write = Some(Box::new(move |path, data| {
            assert!(path.ends_with(".theme.json"));
            *written.borrow_mut() = data.to_vec();
            Ok(())
        }));
        export_file(&mut app).unwrap();
        assert_eq!(ThemePack::parse(std::str::from_utf8(&bytes.borrow()).unwrap()).unwrap(), pack);
        let inbox = crate::ThemeInbox::default();
        inbox.lock().unwrap().push(Err("file too large".into()));
        app.services.theme_inbox = Some(inbox.clone());
        tick(&mut app, &ctx);
        assert_eq!(app.ui.theme_manager.as_ref().unwrap().json, text);
        assert_eq!(app.ui.theme_manager.as_ref().unwrap().error.as_deref(), Some("file too large"));
        inbox.lock().unwrap().push(Ok(text));
        tick(&mut app, &ctx);
        assert!(app.ui.theme_manager.as_ref().unwrap().error.is_none());
    }
    #[test]
    fn manager_buttons_preview_and_cancel_on_a_small_display() {
        use egui_kittest::{Harness, kittest::Queryable};
        let app = PhotocraftApp::new(Session::new(), Services::default());
        let mut h = Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(
            |ui, app| {
                themes::sync(app, ui.ctx());
                show(app, ui.ctx());
            },
            app,
        );
        crate::PhotocraftApp::setup_context(&h.ctx, ThemeKind::ProMedium);
        h.run_steps(3);
        let ctx = h.ctx.clone();
        let before = Tokens::get(&ctx);
        command(h.state_mut(), &ctx, "manager", &Value::Null).unwrap();
        command(h.state_mut(), &ctx, "duplicate", &Value::Null).unwrap();
        let text = changed_definition(h.state());
        command(h.state_mut(), &ctx, "draft", &json!({"json":text})).unwrap();
        h.run_steps(3);
        assert!(h.get_by_label("Cancel").rect().bottom() <= 600.0);
        h.get_by_label("Preview").click();
        h.run_steps(3);
        assert!(h.state().ui.theme_manager.as_ref().unwrap().previewing);
        assert_eq!(Tokens::get(&ctx).card, egui::Color32::from_rgb(170, 187, 204));
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert!(h.state().ui.theme_manager.is_none());
        assert_eq!(Tokens::get(&ctx), before);
        command(h.state_mut(), &ctx, "manager", &Value::Null).unwrap();
        let text = changed_definition(h.state());
        command(h.state_mut(), &ctx, "draft", &json!({"json":text})).unwrap();
        command(h.state_mut(), &ctx, "preview", &Value::Null).unwrap();
        h.run_steps(3);
        h.event(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        h.run_steps(3);
        assert!(h.state().ui.theme_manager.is_none());
        assert_eq!(Tokens::get(&ctx), before);
    }
}
