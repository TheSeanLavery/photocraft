//! Runtime theme catalog, preference migration and shell commands.
//! Platform storage is injected through Services; this module also builds for WASM.

use std::collections::BTreeMap;

use egui::Context;
use photocraft_engine::prefs::Theme;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::ThemeKind;
use crate::theme_pack::{MAX_THEME_BYTES, ThemePack};

pub const MAX_CUSTOM_THEMES: usize = 64;
pub const MAX_LIBRARY_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Library {
    version: u32,
    packs: Vec<ThemePack>,
}

#[derive(Clone, Default)]
pub struct Catalog {
    packs: BTreeMap<String, ThemePack>,
    revision: u64,
}

fn builtin(id: &str) -> Option<ThemeKind> {
    ThemeKind::ALL.into_iter().find(|kind| kind.id() == id)
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_LIBRARY_BYTES {
            return Err(format!("theme library exceeds {MAX_LIBRARY_BYTES} bytes"));
        }
        let library: Library = serde_json::from_str(text).map_err(|e| format!("theme library JSON: {e}"))?;
        if library.version != 1 {
            return Err(format!("unsupported theme library version {}", library.version));
        }
        if library.packs.len() > MAX_CUSTOM_THEMES {
            return Err(format!("theme library exceeds {MAX_CUSTOM_THEMES} custom themes"));
        }
        let mut catalog = Self::default();
        for pack in library.packs {
            catalog.insert(pack, false)?;
        }
        Ok(catalog)
    }

    pub fn serialize(&self) -> Result<String, String> {
        let library = Library { version: 1, packs: self.packs.values().cloned().collect() };
        let text = serde_json::to_string_pretty(&library).map_err(|e| e.to_string())?;
        if text.len() > MAX_LIBRARY_BYTES {
            return Err(format!("theme library exceeds {MAX_LIBRARY_BYTES} bytes"));
        }
        Ok(text)
    }

    pub fn get(&self, id: &str) -> Option<ThemePack> {
        if let Some(kind) = builtin(id) {
            return Some(ThemePack {
                version: 1,
                id: id.into(),
                name: kind.label().into(),
                base: kind.label().into(),
                appearance: Default::default(),
                colors: BTreeMap::new(),
                geometry: Default::default(),
                typography: Default::default(),
            });
        }
        crate::historical::preset(id).or_else(|| self.packs.get(id).cloned())
    }

    pub fn list(&self) -> Vec<Value> {
        ThemeKind::ALL
            .into_iter()
            .filter_map(|kind| self.get(kind.id()))
            .chain(crate::historical::IDS.into_iter().filter_map(crate::historical::preset))
            .chain(self.packs.values().cloned())
            .map(|pack| json!({"id": pack.id, "name": pack.name, "base": pack.base, "builtin": builtin(&pack.id).is_some() || crate::historical::IDS.contains(&pack.id.as_str())}))
            .collect()
    }

    pub fn insert(&mut self, pack: ThemePack, replace: bool) -> Result<(), String> {
        if pack.id.starts_with("builtin.") {
            return Err("built-in theme IDs are reserved; duplicate the theme under a custom ID".into());
        }
        let text = serde_json::to_string(&pack).map_err(|e| e.to_string())?;
        if text.len() > MAX_THEME_BYTES {
            return Err(format!("theme exceeds {MAX_THEME_BYTES} bytes"));
        }
        pack.resolve()?;
        if self.packs.contains_key(&pack.id) {
            if !replace {
                return Err(format!("theme `{}` already exists; use replace:true to update it", pack.id));
            }
        } else if self.packs.len() >= MAX_CUSTOM_THEMES {
            return Err(format!("theme library exceeds {MAX_CUSTOM_THEMES} custom themes"));
        }
        self.packs.insert(pack.id.clone(), pack);
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }

    fn remove(&mut self, id: &str) -> Result<(), String> {
        if builtin(id).is_some() || crate::historical::IDS.contains(&id) {
            return Err("built-in themes cannot be removed".into());
        }
        if self.packs.remove(id).is_none() {
            return Err(format!("unknown theme `{id}`"));
        }
        self.revision = self.revision.saturating_add(1);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct Runtime {
    pub catalog: Catalog,
    pub(crate) last_preferences: Option<(Theme, String)>,
    pub(crate) preview_preferences: Option<(Theme, String)>,
    pub(crate) applied: Option<(String, u64)>,
}

pub(crate) fn load(app: &mut PhotocraftApp) {
    let Some(load) = app.services.load_themes.as_mut() else { return };
    match load().and_then(|text| text.map(|s| Catalog::parse(&s)).transpose()) {
        Ok(Some(catalog)) => app.theme_rt.catalog = catalog,
        Ok(None) => (),
        Err(error) => app.ui.status = format!("Theme library unavailable; using built-in themes: {error}"),
    }
}

pub(crate) fn apply_pack(app: &mut PhotocraftApp, ctx: &Context, pack: &ThemePack) -> Result<(), String> {
    let resolved = pack.resolve()?;
    resolved.apply(ctx);
    app.ui.theme = resolved.tokens.kind;
    app.ui.theme_id = pack.id.clone();
    app.checker = None;
    #[cfg(not(target_arch = "wasm32"))]
    app.live_tokens.invalidate();
    Ok(())
}

/// Migrate legacy preferences and observe changes made through prefs.set or the built-in menu.
pub(crate) fn sync(app: &mut PhotocraftApp, ctx: &Context) {
    let interface = &app.session.prefs().interface;
    let current = (interface.theme, interface.theme_id.clone());
    if app.theme_rt.preview_preferences.as_ref() == Some(&current) {
        return;
    }
    if app.theme_rt.preview_preferences.take().is_some() {
        app.theme_rt.applied = None;
        if let Some(manager) = app.ui.theme_manager.as_mut() {
            manager.previewing = false;
        }
    }
    let fallback = interface.theme;
    let mut id = interface.theme_id.clone();
    let legacy_changed = app.theme_rt.last_preferences.as_ref().is_some_and(|(old_theme, old_id)| *old_theme != fallback && *old_id == id);
    if id.is_empty() || legacy_changed {
        id = crate::prefs_ui::theme_kind(fallback).id().into();
        app.session.prefs.edit(|p| p.interface.theme_id = id.clone());
    }
    let key = (id.clone(), app.theme_rt.catalog.revision);
    if app.theme_rt.applied.as_ref() == Some(&key) {
        app.theme_rt.last_preferences = Some((fallback, id));
        return;
    }
    let pack = app.theme_rt.catalog.get(&id);
    let result = pack.as_ref().ok_or_else(|| format!("selected theme `{id}` is unavailable")).and_then(|pack| apply_pack(app, ctx, pack));
    if let Err(error) = result {
        let kind = crate::prefs_ui::theme_kind(fallback);
        crate::theme::apply(ctx, kind);
        app.ui.theme = kind;
        app.ui.theme_id = kind.id().into();
        app.checker = None;
        #[cfg(not(target_arch = "wasm32"))]
        app.live_tokens.invalidate();
        app.ui.status = format!("{error}; using {}", kind.label());
    } else {
        let base = crate::prefs_ui::theme_pref(app.ui.theme);
        if base != fallback {
            app.session.prefs.edit(|p| p.interface.theme = base);
        }
    }
    app.theme_rt.last_preferences = Some((app.session.prefs().interface.theme, id));
    app.theme_rt.applied = Some(key);
}

pub(crate) fn store(app: &mut PhotocraftApp, candidate: Catalog) -> Result<(), String> {
    let text = candidate.serialize()?;
    if let Some(save) = app.services.save_themes.as_mut() {
        save(&text)?;
    }
    app.theme_rt.catalog = candidate;
    app.theme_rt.applied = None;
    Ok(())
}

pub fn select(app: &mut PhotocraftApp, ctx: &Context, id: &str) -> Result<(), String> {
    let pack = app.theme_rt.catalog.get(id).ok_or_else(|| format!("unknown theme `{id}`"))?;
    let resolved = pack.resolve()?;
    let before = app.session.prefs().interface.clone();
    app.session.prefs.edit(|p| {
        p.interface.theme_id = id.into();
        p.interface.theme = crate::prefs_ui::theme_pref(resolved.tokens.kind);
    });
    if let Err(error) = crate::prefs_ui::save_preferences(app) {
        app.session.prefs.edit(|p| p.interface = before);
        return Err(format!("theme selection could not be saved: {error}"));
    }
    app.theme_rt.preview_preferences = None;
    if let Some(manager) = app.ui.theme_manager.as_mut() {
        manager.previewing = false;
    }
    apply_pack(app, ctx, &pack)?;
    app.theme_rt.last_preferences = Some((app.session.prefs().interface.theme, id.into()));
    app.theme_rt.applied = Some((id.into(), app.theme_rt.catalog.revision));
    Ok(())
}

/// Shell command API; both the native UI and control channel call this dispatcher.
pub fn command(app: &mut PhotocraftApp, ctx: &Context, action: &str, params: &Value) -> Result<Value, String> {
    let id = || params.get("id").and_then(Value::as_str).ok_or_else(|| "missing string `id`".to_string());
    match action {
        "manager" | "draft" | "preview" | "apply" | "cancel" | "duplicate" | "reset" => crate::theme_manager::command(app, ctx, action, params),
        "list" => Ok(json!({"selected": app.ui.theme_id, "requested": app.session.prefs().interface.theme_id, "themes": app.theme_rt.catalog.list()})),
        "get" | "export" => {
            let id = id()?;
            let pack = app.theme_rt.catalog.get(id).ok_or_else(|| format!("unknown theme `{id}`"))?;
            if action == "get" {
                return serde_json::to_value(pack).map_err(|e| e.to_string());
            }
            let text = serde_json::to_string_pretty(&pack).map_err(|e| e.to_string())?;
            Ok(json!({"id": pack.id, "json": text}))
        }
        "select" => {
            select(app, ctx, id()?)?;
            Ok(json!({"selected": app.ui.theme_id, "persisted": app.services.save_prefs.is_some()}))
        }
        "import" => {
            let text = params.get("json").and_then(Value::as_str).ok_or("missing theme `json` string")?;
            let pack = ThemePack::parse(text)?;
            let id = pack.id.clone();
            let replace = match params.get("replace") {
                None => false,
                Some(value) => value.as_bool().ok_or("`replace` must be a boolean")?,
            };
            let mut candidate = app.theme_rt.catalog.clone();
            candidate.insert(pack, replace)?;
            store(app, candidate)?;
            sync(app, ctx);
            Ok(json!({"id": id, "persisted": app.services.save_themes.is_some()}))
        }
        "remove" => {
            let id = id()?;
            if app.session.prefs().interface.theme_id == id {
                return Err("select another theme before removing the current theme".into());
            }
            let mut candidate = app.theme_rt.catalog.clone();
            candidate.remove(id)?;
            store(app, candidate)?;
            Ok(json!({"removed": id}))
        }
        _ => Err(format!("unknown theme action `{action}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use crate::theme::Tokens;
    use photocraft_engine::Session;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn pack(id: &str) -> ThemePack {
        ThemePack::parse(&json!({"version":1,"id":id,"name":"Silver","base":"classic","colors":{"card":"#aabbcc"}}).to_string()).unwrap()
    }

    #[test]
    fn catalog_rejects_duplicates_reserved_ids_bad_packs_and_oversized_libraries() {
        let mut catalog = Catalog::default();
        catalog.insert(pack("silver"), false).unwrap();
        assert!(catalog.insert(pack("silver"), false).is_err());
        assert!(catalog.insert(pack("builtin.classic"), true).is_err());
        let mut bad = pack("bad");
        bad.geometry.radius = Some(f32::INFINITY);
        assert!(catalog.insert(bad, false).is_err());
        assert_eq!(Catalog::parse(&catalog.serialize().unwrap()).unwrap().get("silver"), Some(pack("silver")));
        assert!(Catalog::parse(&json!({"version":1,"packs":[pack("x"),pack("x")]}).to_string()).is_err());
        assert!(Catalog::parse(&" ".repeat(MAX_LIBRARY_BYTES + 1)).is_err());
        for i in 1..MAX_CUSTOM_THEMES {
            catalog.insert(pack(&format!("p{i}")), false).unwrap();
        }
        assert!(catalog.insert(pack("overflow"), false).is_err());
    }

    #[test]
    fn failed_library_save_and_failed_selection_leave_the_working_theme_untouched() {
        let ctx = Context::default();
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        sync(&mut app, &ctx);
        let before = Tokens::get(&ctx);
        app.services.save_themes = Some(Box::new(|_| Err("disk full".into())));
        assert!(command(&mut app, &ctx, "import", &json!({"json":serde_json::to_string(&pack("silver")).unwrap()})).is_err());
        assert!(app.theme_rt.catalog.get("silver").is_none());
        app.theme_rt.catalog.insert(pack("silver"), false).unwrap();
        app.services.save_prefs = Some(Box::new(|_| Err("disk full".into())));
        let pref = app.session.prefs().interface.clone();
        assert!(select(&mut app, &ctx, "silver").is_err());
        assert_eq!(app.session.prefs().interface, pref);
        assert_eq!(Tokens::get(&ctx), before);
        assert_eq!(app.ui.theme_id, ThemeKind::ProMedium.id());
    }

    #[test]
    fn custom_selection_persists_across_restart_and_legacy_preferences_still_select_builtins() {
        let prefs = Rc::new(RefCell::new(String::new()));
        let library = Rc::new(RefCell::new(String::new()));
        let services = |load: bool| {
            let (p1, p2, l1, l2) = (prefs.clone(), prefs.clone(), library.clone(), library.clone());
            Services {
                load_prefs: load.then(|| Box::new(move || Some(p1.borrow().clone())) as crate::LoadTextFn),
                save_prefs: Some(Box::new(move |s| {
                    *p2.borrow_mut() = s.into();
                    Ok(())
                })),
                load_themes: load.then(|| Box::new(move || Ok(Some(l1.borrow().clone()))) as crate::LoadThemesFn),
                save_themes: Some(Box::new(move |s| {
                    *l2.borrow_mut() = s.into();
                    Ok(())
                })),
                ..Default::default()
            }
        };
        let ctx = Context::default();
        let mut app = PhotocraftApp::new(Session::new(), services(false));
        command(&mut app, &ctx, "import", &json!({"json":serde_json::to_string(&pack("silver")).unwrap()})).unwrap();
        select(&mut app, &ctx, "silver").unwrap();
        assert_eq!(app.session.prefs().interface.theme, Theme::Classic);
        let mut restarted = PhotocraftApp::new(Session::new(), services(true));
        sync(&mut restarted, &ctx);
        assert_eq!(restarted.ui.theme_id, "silver");
        assert_eq!(Tokens::get(&ctx).card, egui::Color32::from_rgb(170, 187, 204));
        restarted.session.execute("prefs.set", json!({"path":"interface.theme","value":"studio"})).unwrap();
        sync(&mut restarted, &ctx);
        assert_eq!(restarted.ui.theme_id, ThemeKind::Studio.id());
        assert_eq!(restarted.session.prefs().interface.theme_id, ThemeKind::Studio.id());
    }

    #[test]
    fn missing_selected_pack_uses_fallback_and_recovers_when_imported() {
        let ctx = Context::default();
        let mut session = Session::new();
        session.prefs.edit(|p| {
            p.interface.theme_id = "silver".into();
            p.interface.theme = Theme::Classic;
        });
        let mut app = PhotocraftApp::new(session, Services::default());
        sync(&mut app, &ctx);
        assert_eq!(app.ui.theme_id, ThemeKind::Classic.id());
        assert_eq!(app.session.prefs().interface.theme_id, "silver");
        command(&mut app, &ctx, "import", &json!({"json":serde_json::to_string(&pack("silver")).unwrap()})).unwrap();
        assert_eq!(app.ui.theme_id, "silver");
    }

    #[test]
    fn control_and_ui_commands_share_the_catalog_and_reject_bad_requests_without_changes() {
        use crate::control::{ControlRequest, Outcome, handle};
        let ctx = Context::default();
        let mut app = PhotocraftApp::new(Session::new(), Services::default());
        let text = serde_json::to_string(&pack("silver")).unwrap();
        let (request, _) = ControlRequest::new("ui.theme.import", json!({"json":text}));
        assert!(matches!(handle(&mut app, &ctx, &request), Outcome::Done(v) if v["ok"] == true));
        crate::menus::invoke(&mut app, &ctx, "window.theme.select", json!({"id":"silver"})).unwrap();
        let exported = command(&mut app, &ctx, "export", &json!({"id":"silver"})).unwrap();
        assert_eq!(ThemePack::parse(exported["json"].as_str().unwrap()).unwrap(), pack("silver"));
        let before = Tokens::get(&ctx);
        for (action, params) in [
            ("select", json!({"id":123})),
            ("select", json!({"id":"missing"})),
            ("import", json!({"json":"{partial"})),
            ("import", json!({"json":text,"replace":"yes"})),
            ("remove", json!({"id":"silver"})),
            ("remove", json!({"id":"builtin.pro"})),
        ] {
            assert!(command(&mut app, &ctx, action, &params).is_err());
            assert_eq!(Tokens::get(&ctx), before);
            assert_eq!(app.ui.theme_id, "silver");
        }
        let (request, _) = ControlRequest::new("ui.inspect", json!({}));
        assert!(matches!(handle(&mut app, &ctx, &request), Outcome::Done(v) if v["result"]["themeId"] == "silver"));
    }

    #[test]
    fn corrupt_library_does_not_prevent_legacy_theme_migration() {
        let mut session = Session::new();
        session.prefs.edit(|p| p.interface.theme = Theme::StudioLight);
        let mut app = PhotocraftApp::new(session, Services { load_themes: Some(Box::new(|| Ok(Some("{partial".into())))), ..Default::default() });
        let ctx = Context::default();
        sync(&mut app, &ctx);
        assert_eq!(app.ui.theme_id, ThemeKind::StudioLight.id());
        assert_eq!(app.session.prefs().interface.theme_id, ThemeKind::StudioLight.id());
        assert_eq!(Tokens::get(&ctx), Tokens::for_kind(ThemeKind::StudioLight));
    }

    #[test]
    fn initial_migration_does_not_save_or_dirty_a_preferences_draft() {
        let saves = Rc::new(std::cell::Cell::new(0));
        let count = saves.clone();
        let mut app = PhotocraftApp::new(
            Session::new(),
            Services {
                load_prefs: Some(Box::new(|| Some(r#"{"interface":{"theme":"classic"}}"#.into()))),
                save_prefs: Some(Box::new(move |_| {
                    count.set(count.get() + 1);
                    Ok(())
                })),
                ..Default::default()
            },
        );
        assert_eq!(app.session.prefs().interface.theme_id, ThemeKind::Classic.id());
        crate::prefs_ui::tick(&mut app, &Context::default());
        assert_eq!(saves.get(), 0);
        let id = crate::prefs_ui::open_preferences(&mut app, "interface");
        let draft = &app.ui.dialogs.iter().find(|dialog| dialog.id == id).unwrap().fields;
        assert!(!crate::prefs_ui::preferences_changed(&app, draft));
    }
}
