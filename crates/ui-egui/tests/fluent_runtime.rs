//! The public UI translation API reads all staged Fluent locales.
use photocraft_ui_egui::i18n::{self, Lang};

#[test]
fn all_locales_keep_plain_context_and_plural_behavior() {
    let expected = [
        ("ja", "レイヤー"),
        ("zh-hans", "图层"),
        ("zh-hant", "圖層"),
        ("es", "Capa"),
        ("ru", "Слой"),
        ("cs", "Vrstva"),
        ("fr", "Calque"),
        ("id", "Layer"),
        ("ko", "레이어"),
        ("de", "Ebene"),
        ("pt-br", "Camada"),
        ("it", "Livello"),
    ];
    for (code, translation) in expected {
        let lang = Lang::from_code(code).expect("registered language");
        assert_eq!(i18n::tr(lang, "Layer"), translation, "{code}");
        assert_eq!(i18n::tr(lang, "untranslated diagnostic"), "untranslated diagnostic", "{code}");
        assert!(!i18n::trn(lang, 2, "{n} item", "{n} items").contains("{n}"), "{code}");
    }
}

#[test]
fn fluent_variables_preserve_reordered_placeholders() {
    let zh = Lang::from_code("zh-hans").expect("registered language");
    i18n::with_language(zh, || {
        let text = i18n::fmt("Add a mask  (from the selection; {key} inverts)", &[("key", "⌥")]);
        assert!(text.contains('⌥'));
        assert!(!text.contains("{key}"));
    });
}
