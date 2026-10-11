//! Stable Fluent message IDs layered under the existing public translation API.
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

use fluent_bundle::{FluentArgs, FluentResource, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

type Bundle = FluentBundle<FluentResource>;

struct Catalog {
    bundle: Bundle,
    simple: OnceLock<HashMap<String, String>>,
}

impl Catalog {
    fn new(code: &str, source: &str) -> Self {
        let locale: LanguageIdentifier = code.parse().unwrap_or_else(|_| "en".parse().unwrap_or_default());
        let mut bundle = Bundle::new_concurrent(vec![locale]);
        bundle.set_use_isolating(false);
        let resource = match FluentResource::try_new(source.to_owned()) {
            Ok(resource) | Err((resource, _)) => resource,
        };
        let _ = bundle.add_resource(resource);
        Self { bundle, simple: OnceLock::new() }
    }

    fn simple(&self) -> &HashMap<String, String> {
        self.simple.get_or_init(|| {
            keys().plain.values().chain(keys().other.values()).filter_map(|id| render(&self.bundle, id, None).map(|value| (id.clone(), value))).collect()
        })
    }
}

static CATALOGS: [OnceLock<Option<Catalog>>; 17] = [const { OnceLock::new() }; 17];

// Keeping the authored source in native builds also keeps native tools independent of an
// external locale bundle. The web build retains every language in a smaller, lossless payload.
macro_rules! source {
    ($locale:literal) => {{
        #[cfg(not(target_arch = "wasm32"))]
        let text = include_str!(concat!("locales/", $locale, "/messages.ftl"));
        #[cfg(target_arch = "wasm32")]
        let text = include_bytes!(concat!(env!("OUT_DIR"), "/i18n/fluent-", $locale, ".ftl.deflate")).as_slice();
        text
    }};
}

fn catalog(code: &str) -> Option<&'static Catalog> {
    let (index, locale, source) = match code {
        "en" => (0, "en", source!("en")),
        "ja" => (1, "ja", source!("ja")),
        "zh-hans" => (2, "zh-CN", source!("zh-CN")),
        "zh-hant" => (3, "zh-TW", source!("zh-TW")),
        "es" => (4, "es", source!("es")),
        "ru" => (5, "ru", source!("ru")),
        "cs" => (6, "cs", source!("cs")),
        "fr" => (7, "fr", source!("fr")),
        "id" => (8, "id", source!("id")),
        "ko" => (9, "ko", source!("ko")),
        "de" => (10, "de", source!("de")),
        "pt-br" => (11, "pt-BR", source!("pt-BR")),
        "it" => (12, "it", source!("it")),
        "uk" => (13, "uk", source!("uk")),
        "pl" => (14, "pl", source!("pl")),
        "el" => (15, "el", source!("el")),
        "nl" => (16, "nl", source!("nl")),
        _ => return None,
    };
    CATALOGS
        .get(index)?
        .get_or_init(|| {
            #[cfg(not(target_arch = "wasm32"))]
            let source = Cow::Borrowed(source);
            #[cfg(target_arch = "wasm32")]
            let source: Cow<'_, str> = Cow::Owned(super::inflate(source).ok()?);
            Some(Catalog::new(locale, &source))
        })
        .as_ref()
}

struct Keys {
    plain: HashMap<String, String>,
    other: HashMap<String, String>,
}

fn keys() -> &'static Keys {
    static KEYS: OnceLock<Keys> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut plain = HashMap::new();
        let mut other = HashMap::new();
        #[cfg(not(target_arch = "wasm32"))]
        let source = Cow::Borrowed(include_str!("keys.tsv"));
        #[cfg(target_arch = "wasm32")]
        let source: Cow<'_, str> = Cow::Owned(super::inflate(include_bytes!(concat!(env!("OUT_DIR"), "/i18n/fluent-keys.tsv.deflate"))).unwrap_or_default());
        for line in source.lines().filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let mut parts = line.split('\t');
            let (Some(context), Some(source), Some(id), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let (context, source) = (unescape(context), unescape(source));
            if context.is_empty() {
                plain.insert(source, id.to_owned());
            } else {
                other.insert(format!("{context}\u{1}{source}"), id.to_owned());
            }
        }
        Keys { plain, other }
    })
}

fn unescape(value: &str) -> String {
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
        } else {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
    }
    out
}

fn message_id(context: &str, source: &str) -> Option<&'static str> {
    if context.is_empty() { keys().plain.get(source).map(String::as_str) } else { keys().other.get(&format!("{context}\u{1}{source}")).map(String::as_str) }
}

fn render(bundle: &Bundle, id: &str, args: Option<&FluentArgs<'_>>) -> Option<String> {
    let pattern = bundle.get_message(id)?.value()?;
    let mut errors = Vec::new();
    let result = bundle.format_pattern(pattern, args, &mut errors);
    errors.is_empty().then(|| result.into_owned())
}

pub fn simple(code: &str, context: &str, source: &str) -> Option<&'static str> {
    let id = message_id(context, source)?;
    catalog(code)?.simple().get(id).map(String::as_str)
}

pub fn has(code: &str, source: &str) -> bool {
    code != "en" && simple(code, "", source).is_some()
}

pub fn format(code: &str, template: &str, args: &[(&str, &str)]) -> Option<String> {
    let id = message_id("", template)?;
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, *value);
    }
    render(&catalog(code)?.bundle, id, Some(&fluent_args))
}

pub fn plural(code: &str, n: u64, one: &str, other: &str) -> Option<String> {
    let count = i64::try_from(n).ok()?;
    let id = message_id("@plural", &format!("{one}|{other}"))?;
    let mut args = FluentArgs::new();
    args.set("n", count);
    render(&catalog(code)?.bundle, id, Some(&args))
}

#[cfg(test)]
mod tests {
    #[test]
    fn compressed_fluent_payloads_preserve_all_authored_bytes() {
        macro_rules! check {
            ($locale:literal) => {
                assert_eq!(
                    super::super::inflate(include_bytes!(concat!(env!("OUT_DIR"), "/i18n/fluent-", $locale, ".ftl.deflate"))).unwrap(),
                    include_str!(concat!("locales/", $locale, "/messages.ftl")),
                    "locale {} changed while compressing",
                    $locale
                );
            };
        }
        check!("en");
        check!("ja");
        check!("zh-CN");
        check!("zh-TW");
        check!("es");
        check!("ru");
        check!("cs");
        check!("fr");
        check!("id");
        check!("ko");
        check!("de");
        check!("pt-BR");
        check!("it");
        check!("uk");
        check!("pl");
        check!("el");
        check!("nl");
        assert_eq!(super::super::inflate(include_bytes!(concat!(env!("OUT_DIR"), "/i18n/fluent-keys.tsv.deflate"))).unwrap(), include_str!("keys.tsv"));
    }
}
