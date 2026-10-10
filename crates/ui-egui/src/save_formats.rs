//! Writable document formats shared by the native save panel and macOS format chooser.

/// One format group in a save chooser.
pub struct SaveFormat {
    pub name: &'static str,
    pub extensions: &'static [&'static str],
}

/// Only formats the current build can export. PSD and PSB are separate choices so a user can
/// explicitly choose the large-document container even for a small image.
pub fn document_formats() -> Vec<SaveFormat> {
    let mut formats = vec![
        SaveFormat { name: "PSD Document", extensions: &["psd"] },
        SaveFormat { name: "Large Document (PSB)", extensions: &["psb"] },
        SaveFormat { name: "PhotoCraft", extensions: &["pcraft"] },
    ];
    formats.extend(
        photocraft_codecs::Format::ALL.into_iter().filter(|f| f.caps().write).map(|f| SaveFormat {
            name: if f == photocraft_codecs::Format::Pnm { "Netpbm (automatic subtype)" } else { f.name() },
            extensions: f.extensions(),
        }),
    );
    formats
}

/// Render a format chooser. `Some(None)` cancels; `Some(Some(extension))` continues.
pub(crate) fn show_choice(ctx: &egui::Context, format: &mut String) -> Option<Option<String>> {
    let formats = document_formats();
    let mut next = false;
    let mut cancel = false;
    let modal = egui::Modal::new(egui::Id::new("save-format")).show(ctx, |ui| {
        ui.set_min_width(360.0);
        ui.label(egui::RichText::new(tl!("Save As")).font(crate::theme::semibold(15.0)));
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(tl!("Format"));
            let label = formats.iter().find(|f| f.extensions.contains(&format.as_str())).map_or_else(|| format.clone(), |f| format!("{} (.{format})", f.name));
            egui::ComboBox::from_id_salt("save-format-choice").selected_text(label).width(270.0).height(420.0).icon(crate::widgets::chevron_icon).show_ui(
                ui,
                |ui| {
                    for f in &formats {
                        if let Some(ext) = f.extensions.first() {
                            let selected = f.extensions.contains(&format.as_str());
                            if ui.selectable_label(selected, format!("{} (.{ext})", f.name)).clicked() {
                                *format = (*ext).into();
                            }
                        }
                    }
                },
            );
        });
        ui.add_space(8.0);
        if !matches!(format.as_str(), "pcraft" | "psd" | "psb" | "tif" | "tiff") {
            ui.label(tl!("This format saves a flattened image. Use Save a Copy to keep your editable document."));
            if format.as_str() == "pnm" {
                ui.label(tl!("Netpbm chooses the subtype from the image depth and channels."));
            }
        }
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            next = crate::widgets::primary_button(ui, tl!("Continue"), 96.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    if next || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        Some(Some(format.clone()))
    } else if cancel || modal.should_close() {
        Some(None)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_writable_codec_is_discoverable_and_import_only_formats_are_absent() {
        let formats = document_formats();
        for codec in photocraft_codecs::Format::ALL {
            for ext in codec.extensions() {
                assert_eq!(formats.iter().any(|f| f.extensions.contains(ext)), codec.caps().write, "{ext}");
            }
        }
        for ext in ["psd", "psb", "pcraft", "bmp", "dib", "gif", "ico", "qoi", "hdr", "pfm", "jpeg", "tiff"] {
            assert!(formats.iter().any(|f| f.extensions.contains(&ext)), "{ext}");
        }
    }
    #[test]
    fn every_offered_format_encodes_and_reopens_at_multiple_depths() {
        for depth in [8, 16, 32] {
            let mut session = photocraft_engine::Session::new();
            session.execute("file.new", serde_json::json!({"width": 4, "height": 4, "depth": depth})).unwrap();
            let doc = &session.active().unwrap().doc;
            for format in document_formats() {
                let name = format!("test.{}", format.extensions.first().unwrap());
                let exported = photocraft_io::export(doc, &name, &Default::default()).unwrap();
                if let Some(codec) = photocraft_codecs::from_extension(&name) {
                    assert_eq!(photocraft_codecs::detect(&exported.bytes), Some(codec), "{name}, depth {depth}");
                }
                if name.ends_with("avif") {
                    continue;
                } // Encode-only when compiled in.
                let opened = photocraft_io::import(&name, &exported.bytes).unwrap();
                assert_eq!(opened.document.size, doc.size, "{name}, depth {depth}");
            }
        }
    }
}
