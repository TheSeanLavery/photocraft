//! Offscreen acceptance of the desktop's actual theme storage callbacks and Theme Manager.
#[path = "../src/theme_storage.rs"]
mod theme_storage;
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_ui_egui::{
    PhotocraftApp, Services,
    control::{ControlRequest, Outcome, handle},
    theme::{LayoutTokens, ThemeKind, Tokens},
    theme_pack::ThemePack,
    themes::Catalog,
};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant},
};

#[derive(Clone)]
struct Choosers {
    theme: Rc<RefCell<Option<PathBuf>>>,
    export: Rc<RefCell<Option<PathBuf>>>,
}
impl Choosers {
    fn new(root: &Path) -> Self {
        Self { theme: Rc::new(RefCell::new(Some(root.join("import.theme.json")))), export: Rc::new(RefCell::new(Some(root.join("export.theme.json")))) }
    }
}

type AppHarness = Harness<'static, PhotocraftApp>;
fn harness(library: &Path, preferences: &Path, scale: f32, missing: Rc<Cell<bool>>, choosers: &Choosers) -> AppHarness {
    let (load, save, mut poll) = theme_storage::theme_services(Some(library.into()));
    let (read, write) = (preferences.to_owned(), preferences.to_owned());
    let (theme_choice, export_choice) = (choosers.theme.clone(), choosers.export.clone());
    let services = Services {
        pick_theme: Some(Box::new(move || {
            let selected = theme_choice.try_borrow().map_err(|_| "theme chooser busy")?;
            theme_storage::read_selected_theme(selected.as_deref())
        })),
        pick_save: Some(Box::new(move |_| {
            let selected = export_choice.try_borrow().ok()?;
            selected.as_ref().map(|p| p.to_string_lossy().into_owned())
        })),
        write: Some(Box::new(|path, bytes| photocraft_format::atomic_write(Path::new(path), bytes).map_err(|e| e.to_string()))),
        load_themes: Some(load),
        save_themes: Some(save),
        poll_themes: Some(Box::new(move |ctx| {
            let result = poll(ctx);
            if matches!(&result, Some(Ok(None))) {
                missing.set(true);
            }
            result
        })),
        load_prefs: Some(Box::new(move || std::fs::read_to_string(&read).ok())),
        save_prefs: Some(Box::new(move |text| photocraft_format::atomic_write(&write, text.as_bytes()).map_err(|e| e.to_string()))),
        ..Default::default()
    };
    Harness::builder().with_size(egui::vec2(1280.0, 900.0)).with_pixels_per_point(scale).wgpu().build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Studio);
        PhotocraftApp::new(photocraft_engine::Session::new(), services)
    })
}
fn command(h: &mut AppHarness, action: &str, params: Value) -> Result<Value, String> {
    let (request, _) = ControlRequest::new(format!("ui.theme.{action}"), params);
    let ctx = h.ctx.clone();
    match handle(h.state_mut(), &ctx, &request) {
        Outcome::Done(value) if value.get("ok") == Some(&Value::Bool(true)) => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
        Outcome::Done(value) => Err(value.to_string()),
        _ => Err("unexpected asynchronous theme command".into()),
    }
}
fn click(h: &mut AppHarness, label: &str) -> Result<(), String> {
    {
        let mut nodes = h.query_all_by_label(label);
        let node = nodes.next().ok_or_else(|| format!("missing UI button {label}"))?;
        if nodes.next().is_some() {
            return Err(format!("ambiguous UI button {label}"));
        }
        node.click();
    }
    h.run_steps(4);
    Ok(())
}
fn wait(h: &mut AppHarness, predicate: impl Fn(&AppHarness) -> bool) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        h.step();
        if predicate(h) {
            return Ok(());
        }
        if Instant::now() > until {
            return Err(format!("theme condition timed out: {}", h.state().ui.status));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn settle(h: &mut AppHarness) {
    let until = Instant::now() + Duration::from_millis(2200);
    while Instant::now() < until {
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn write_catalog(path: &Path, catalog: &Catalog) -> Result<(), Box<dyn std::error::Error>> {
    photocraft_format::atomic_write(path, catalog.serialize()?.as_bytes())?;
    Ok(())
}
fn changed(pack: &ThemePack, color: &str, height: f32) -> Result<ThemePack, String> {
    let mut v = serde_json::to_value(pack).map_err(|e| e.to_string())?;
    v["colors"]["card"] = json!(color);
    v["geometry"]["buttonHeight"] = json!(height);
    ThemePack::parse(&serde_json::to_string(&v).map_err(|e| e.to_string())?)
}
fn screenshot(h: &mut AppHarness, root: &Path, slug: &str, phase: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    h.run_steps(4);
    let path = root.join(format!("{slug}-{phase}.png"));
    h.render()?.save(&path)?;
    Ok(path)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PHOTOCRAFT_THEME_FILE").is_some() {
        return Err("unset PHOTOCRAFT_THEME_FILE for library acceptance".into());
    }
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).ok_or("supply fresh evidence directory")?);
    let scale = args.get(2).ok_or("supply scale")?.parse::<f32>()?;
    if ![1.0, 1.5, 2.0].contains(&scale) {
        return Err("scale must be 1, 1.5 or 2".into());
    }
    let library = root.join("Themes/library.json");
    let preferences = root.join("preferences.json");
    if library.exists() || preferences.exists() {
        return Err("existing library/preferences preserved: use a fresh evidence directory".into());
    }
    if root.exists() && std::fs::read_dir(&root)?.next().is_some() {
        return Err("use an empty evidence directory; existing files are preserved".into());
    }
    std::fs::create_dir_all(&root)?;
    let mut catalog = Catalog::default();
    let mut packs = Vec::new();
    for (slug, text) in [
        ("windows-98-se", include_str!("../../../assets/themes/windows-98-se.json")),
        ("mac-os-9-1", include_str!("../../../assets/themes/mac-os-9-1.json")),
        ("system-6", include_str!("../../../assets/themes/system-6.json")),
    ] {
        let mut pack = ThemePack::parse(text)?;
        pack.id = format!("acceptance.{slug}");
        catalog.insert(pack.clone(), false)?;
        packs.push((slug, pack));
    }
    write_catalog(&library, &catalog)?;
    let missing = Rc::new(Cell::new(false));
    let choosers = Choosers::new(&root);
    let import_path = root.join("import.theme.json");
    let export_path = root.join("export.theme.json");
    let mut h = harness(&library, &preferences, scale, missing.clone(), &choosers);
    h.state_mut().run("file.new", json!({"width":640,"height":480}))?;
    h.run_steps(4);
    let mut evidence = Vec::new();
    for (slug, pack) in packs {
        command(&mut h, "select", json!({"id":pack.id}))?;
        let normal = screenshot(&mut h, &root, slug, "selected")?;
        command(&mut h, "manager", json!({"id":pack.id}))?;
        let draft = h.state().ui.theme_manager.as_ref().ok_or("manager missing")?.json.clone();
        let external = changed(&pack, "#C5D5E5", 28.0)?;
        catalog.insert(external.clone(), true)?;
        write_catalog(&library, &catalog)?;
        let external_tokens = external.resolve()?.tokens;
        wait(&mut h, |h| Tokens::get(&h.ctx) == external_tokens)?;
        if LayoutTokens::get(&h.ctx).button_height != 28.0 || h.state().ui.theme_manager.as_ref().map(|m| &m.json) != Some(&draft) {
            return Err("external edit lost metrics or manager draft".into());
        }
        let updated = screenshot(&mut h, &root, slug, "external-edit")?;
        std::fs::write(&library, "{partial")?;
        wait(&mut h, |h| h.state().ui.status.starts_with("Theme library unchanged:"))?;
        if Tokens::get(&h.ctx) != external_tokens {
            return Err("invalid library changed appearance".into());
        }
        let preview = changed(&pack, "#ADBEEF", 32.0)?;
        command(&mut h, "draft", json!({"json":serde_json::to_string(&preview)?}))?;
        h.run_steps(4);
        click(&mut h, "Preview")?;
        let preview_tokens = preview.resolve()?.tokens;
        let preview_image = screenshot(&mut h, &root, slug, "preview")?;
        let deferred = changed(&pack, "#FFE8CC", 22.0)?;
        catalog.insert(deferred.clone(), true)?;
        write_catalog(&library, &catalog)?;
        settle(&mut h);
        if Tokens::get(&h.ctx) != preview_tokens || LayoutTokens::get(&h.ctx).button_height != 32.0 {
            return Err("external library edit interrupted preview".into());
        }
        click(&mut h, "Cancel")?;
        let deferred_tokens = deferred.resolve()?.tokens;
        wait(&mut h, |h| Tokens::get(&h.ctx) == deferred_tokens)?;
        if LayoutTokens::get(&h.ctx).button_height != 22.0 {
            return Err("cancel failed to adopt deferred metrics".into());
        }
        if h.state().ui.status.starts_with("Theme library unchanged:") {
            return Err("valid library recovery retained an obsolete error".into());
        }
        command(&mut h, "manager", json!({"id":pack.id}))?;
        let mut applied = changed(&pack, "#BBDDCC", 24.0)?;
        command(&mut h, "draft", json!({"json":serde_json::to_string(&applied)?}))?;
        h.run_steps(4);
        click(&mut h, "Apply")?;
        catalog = Catalog::parse(&std::fs::read_to_string(&library)?)?;
        if catalog.get(&pack.id).as_ref() != Some(&applied) || h.state().session.prefs().interface.theme_id != pack.id {
            return Err("Apply did not persist definition and selection".into());
        }
        settle(&mut h);
        if Tokens::get(&h.ctx) != applied.resolve()?.tokens || LayoutTokens::get(&h.ctx).button_height != 24.0 {
            return Err("stale read overwrote Apply".into());
        }
        let applied_image = screenshot(&mut h, &root, slug, "applied")?;
        click(&mut h, "Export…")?;
        if ThemePack::parse(&std::fs::read_to_string(&export_path)?)? != applied {
            return Err("exported definition differs from draft".into());
        }
        let imported = changed(&pack, "#FFEEDD", 26.0)?;
        photocraft_format::atomic_write(&import_path, serde_json::to_string_pretty(&imported)?.as_bytes())?;
        click(&mut h, "Import…")?;
        if ThemePack::parse(&h.state().ui.theme_manager.as_ref().ok_or("missing imported draft")?.json)? != imported
            || Tokens::get(&h.ctx) != applied.resolve()?.tokens
        {
            return Err("import did not load a draft without changing appearance".into());
        }
        click(&mut h, "Preview")?;
        if Tokens::get(&h.ctx) != imported.resolve()?.tokens {
            return Err("imported preview was not applied".into());
        }
        click(&mut h, "Cancel")?;
        if Tokens::get(&h.ctx) != applied.resolve()?.tokens {
            return Err("import preview cancellation lost saved appearance".into());
        }
        command(&mut h, "manager", json!({"id":pack.id}))?;
        h.run_steps(4);
        click(&mut h, "Import…")?;
        click(&mut h, "Apply")?;
        applied = imported;
        catalog = Catalog::parse(&std::fs::read_to_string(&library)?)?;
        if catalog.get(&pack.id).as_ref() != Some(&applied) || LayoutTokens::get(&h.ctx).button_height != 26.0 {
            return Err("imported Apply did not persist metrics/definition".into());
        }
        let imported_image = screenshot(&mut h, &root, slug, "imported-applied")?;
        let before_bad = h.state().ui.theme_manager.as_ref().ok_or("missing manager")?.json.clone();
        std::fs::write(&import_path, "{invalid")?;
        click(&mut h, "Import…")?;
        if h.state().ui.theme_manager.as_ref().and_then(|m| m.error.as_ref()).is_none()
            || h.state().ui.theme_manager.as_ref().map(|m| &m.json) != Some(&before_bad)
        {
            return Err("invalid import lost valid draft or diagnostic".into());
        }
        std::fs::write(&import_path, " ".repeat(photocraft_ui_egui::theme_pack::MAX_THEME_BYTES + 1))?;
        click(&mut h, "Import…")?;
        if !h.state().ui.theme_manager.as_ref().and_then(|m| m.error.as_ref()).is_some_and(|e| e.contains("exceeds"))
            || Tokens::get(&h.ctx) != applied.resolve()?.tokens
        {
            return Err("oversized import was not rejected without style loss".into());
        }
        photocraft_format::atomic_write(&import_path, serde_json::to_string_pretty(&applied)?.as_bytes())?;
        click(&mut h, "Import…")?;
        *choosers.theme.try_borrow_mut()? = None;
        click(&mut h, "Import…")?;
        if ThemePack::parse(&h.state().ui.theme_manager.as_ref().ok_or("missing canceled-import manager")?.json)? != applied {
            return Err("cancelled import changed draft".into());
        }
        *choosers.theme.try_borrow_mut()? = Some(import_path.clone());
        std::fs::remove_file(&export_path)?;
        *choosers.export.try_borrow_mut()? = None;
        click(&mut h, "Export…")?;
        if export_path.exists() {
            return Err("cancelled export wrote a file".into());
        }
        *choosers.export.try_borrow_mut()? = Some(export_path.clone());
        click(&mut h, "Export…")?;
        if ThemePack::parse(&std::fs::read_to_string(&export_path)?)? != applied {
            return Err("final portable export failed round trip".into());
        }

        command(&mut h, "draft", json!({"json":"{invalid"}))?;
        h.run_steps(4);
        click(&mut h, "Reset Changes")?;
        let reset = ThemePack::parse(&h.state().ui.theme_manager.as_ref().ok_or("missing reset manager")?.json)?;
        if reset != applied {
            return Err("Reset did not restore persisted definition".into());
        }
        click(&mut h, "Cancel")?;
        missing.set(false);
        std::fs::remove_file(&library)?;
        wait(&mut h, |_| missing.get())?;
        if Tokens::get(&h.ctx) != applied.resolve()?.tokens {
            return Err("deleted library changed appearance".into());
        }
        drop(h);
        h = harness(&library, &preferences, scale, missing.clone(), &choosers);
        h.run_steps(4);
        if h.state().session.prefs().interface.theme_id != pack.id || h.state().ui.theme_id == pack.id {
            return Err("missing startup library lost requested ID or failed to show fallback".into());
        }
        write_catalog(&library, &catalog)?;
        let applied_tokens = applied.resolve()?.tokens;
        wait(&mut h, |h| h.state().ui.theme_id == pack.id && Tokens::get(&h.ctx) == applied_tokens)?;
        let prefs: Value = serde_json::from_str(&std::fs::read_to_string(&preferences)?)?;
        if prefs["interface"]["themeId"] != pack.id {
            return Err(format!("selection not on disk: {prefs}").into());
        }
        drop(h);
        h = harness(&library, &preferences, scale, missing.clone(), &choosers);
        h.run_steps(4);
        if h.state().ui.theme_id != pack.id || Tokens::get(&h.ctx) != applied.resolve()?.tokens {
            return Err("new session failed to restore custom ID/appearance".into());
        }
        h.state_mut().run("file.new", json!({"width":640,"height":480}))?;
        h.run_steps(4);
        evidence.push(json!({"theme":slug,"selected":normal,"externalEdit":updated,"preview":preview_image,"applied":applied_image,"draftPreserved":true,"invalidRetained":true,"previewDeferred":true,"applyPersisted":true,"reset":true,"deletedRetained":true,"newSessionRestored":true,"missingStartupRecovered":true,"buttonsClicked":true,"importedApplied":imported_image,"portableImportExport":true,"invalidOversizedImportRetained":true,"chooserCancellation":true}));
    }
    std::fs::write(
        root.join("acceptance.json"),
        serde_json::to_string_pretty(
            &json!({"release":!cfg!(debug_assertions),"scale":scale,"chooser":"injected owned paths; native bounded reader and atomic writer","evidence":evidence}),
        )?,
    )?;
    println!("accepted native library/manager lifecycle at {scale}x: {}", root.display());
    Ok(())
}
