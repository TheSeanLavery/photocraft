//! Release-build, offscreen live-file acceptance. Uses only its caller-owned evidence directory.
use egui_kittest::{Harness, kittest::Queryable};
use photocraft_ui_egui::{
    PhotocraftApp, Services,
    theme::{LayoutTokens, ThemeKind, Tokens},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).ok_or("supply an evidence directory")?);
    let scale = args.get(2).ok_or("supply scale")?.parse::<f32>()?;
    let gpu = match args.get(3).map(String::as_str).unwrap_or("cpu") {
        "cpu" => false,
        "gpu" => true,
        _ => return Err("renderer must be cpu or gpu".into()),
    };
    if ![1.0, 1.5, 2.0].contains(&scale) {
        return Err("scale must be 1, 1.5 or 2".into());
    }
    std::fs::create_dir_all(&root)?;
    if root.join("live.theme.json").exists() || root.join("pending.json").exists() {
        return Err("use a fresh evidence directory; existing live files are preserved".into());
    }
    let path = root.join("live.theme.json");
    if std::env::var_os("PHOTOCRAFT_THEME_FILE").map(PathBuf::from).as_ref() != Some(&path) {
        return Err("PHOTOCRAFT_THEME_FILE must equal evidence-directory/live.theme.json".into());
    }
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 800.0)).with_pixels_per_point(scale).wgpu().build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, ThemeKind::Studio);
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        if gpu && let Some(rs) = cc.wgpu_render_state.as_ref() {
            app.set_wgpu(rs.clone());
        }
        app
    });
    if gpu && !h.state().gpu_active() {
        return Err("GPU canvas requested but unavailable".into());
    }
    h.state_mut().run("file.new", json!({"width":640,"height":480}))?;
    let fixtures = [
        ("windows-98-se", include_str!("../../../assets/themes/windows-98-se.json")),
        ("mac-os-9-1", include_str!("../../../assets/themes/mac-os-9-1.json")),
        ("system-6", include_str!("../../../assets/themes/system-6.json")),
    ];
    let mut evidence = Vec::new();
    for (slug, text) in fixtures {
        let mut value: Value = serde_json::from_str(text)?;
        let expected = photocraft_ui_egui::theme_pack::ThemePack::parse(text)?.resolve()?.tokens;
        std::fs::write(&path, text)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            h.step();
            if Tokens::get(&h.ctx) == expected {
                break;
            }
            if Instant::now() > deadline {
                return Err(format!("{slug}: valid file was not adopted").into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        h.run_steps(4);
        // Exercise the real Fit control after each theme reflows the available canvas.
        h.get_by_label("Fit").click();
        h.run_steps(4);
        if h.state().ui.views.first().is_none_or(|view| view.fit_pending) {
            return Err(format!("{slug}: Fit did not complete").into());
        }
        if gpu && !h.state().perf.gpu {
            return Err("document did not render through GPU canvas".into());
        }
        let normal = root.join(format!("{slug}-live-{scale}x.png"));
        let image = h.render()?;
        if slug == "system-6" {
            let app = h.state();
            let view = app.ui.views.first().ok_or("missing document view")?;
            let doc = app.session.active().ok_or("missing document")?;
            let xf = photocraft_ui_egui::canvas::ViewXform {
                rect: app.last_canvas_rect,
                zoom: view.zoom / h.ctx.pixels_per_point(),
                center: view.center,
                flip: false,
                aspect: 1.0,
                rotation: 0.0,
            };
            let document = xf.doc_rect(doc.doc.bounds());
            // A modern blurred shadow darkens the pasteboard next to a document edge.
            // Select a fully visible strip in the actual fitted canvas, then require every
            // sample to remain white. Logical canvas coordinates scale exactly once to pixels.
            let strips = [
                [5.0, 10.0, 18.0].map(|offset| egui::pos2(document.center().x, document.bottom() + offset)),
                [5.0, 10.0, 18.0].map(|offset| egui::pos2(document.right() + offset, document.center().y)),
                [5.0, 10.0, 18.0].map(|offset| egui::pos2(document.center().x, document.top() - offset)),
                [5.0, 10.0, 18.0].map(|offset| egui::pos2(document.left() - offset, document.center().y)),
            ];
            let ppp = h.ctx.pixels_per_point();
            let strip = strips
                .into_iter()
                .find(|points| {
                    points.iter().all(|point| {
                        xf.rect.contains(*point)
                            && !document.contains(*point)
                            && point.x >= 0.0
                            && point.y >= 0.0
                            && (point.x * ppp).round() < image.width() as f32
                            && (point.y * ppp).round() < image.height() as f32
                    })
                })
                .ok_or("fitted System 6 document has no visible pasteboard strip")?;
            for point in strip {
                let (x, y) = ((point.x * ppp).round() as u32, (point.y * ppp).round() as u32);
                if image.get_pixel(x, y).0 != [255, 255, 255, 255] {
                    return Err("System 6 pasteboard contains a soft document shadow".into());
                }
            }
        }
        image.save(&normal)?;
        std::fs::write(&path, "{invalid")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.state().ui.status.contains("theme file") && !h.state().ui.status.contains("theme JSON") {
            h.step();
            if Instant::now() > deadline {
                return Err(format!("{slug}: missing invalid-file diagnostic: {}", h.state().ui.status).into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if Tokens::get(&h.ctx) != expected {
            return Err("invalid edit changed appearance".into());
        }
        let diagnostic = h.state().ui.status.clone();
        // A disk edit changes both the palette and metrics without recompilation.
        value["colors"]["card"] = json!("#C5D5E5");
        value["geometry"]["buttonHeight"] = json!(28.0);
        let changed = serde_json::to_string_pretty(&value)?;
        let recovered = photocraft_ui_egui::theme_pack::ThemePack::parse(&changed)?.resolve()?.tokens;
        std::fs::write(root.join("pending.json"), &changed)?;
        std::fs::rename(root.join("pending.json"), &path)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Tokens::get(&h.ctx) != recovered {
            h.step();
            if Instant::now() > deadline {
                return Err(format!("{slug}: atomic recovery was not adopted").into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if LayoutTokens::get(&h.ctx).button_height != 28.0 {
            return Err("recovered file did not update button metrics".into());
        }
        h.run_steps(4);
        let recovery = root.join(format!("{slug}-recovered-{scale}x.png"));
        h.render()?.save(&recovery)?;
        std::fs::remove_file(&path)?;
        let until = Instant::now() + Duration::from_secs(5);
        while !h.state().ui.status.contains("theme file is unavailable") {
            h.step();
            if Instant::now() > until {
                return Err("deleted override was not observed".into());
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        if Tokens::get(&h.ctx) != recovered {
            return Err("deleted override changed appearance".into());
        }
        evidence.push(json!({"theme":slug,"scale":scale,"normal":normal,"recovered":recovery,"invalidDiagnostic":diagnostic,"valid":true,"invalidRetained":true,"atomicRecovery":true,"buttonHeight":LayoutTokens::get(&h.ctx).button_height,"deletedRetained":true}));
        h.state_mut().ui.status.clear();
    }
    std::fs::write(
        root.join("acceptance.json"),
        serde_json::to_string_pretty(&json!({"release":!cfg!(debug_assertions),"renderer":if gpu {"gpu"} else {"cpu"},"evidence":evidence}))?,
    )?;
    println!("accepted all three live themes at {scale}x; {}", root.display());
    Ok(())
}
