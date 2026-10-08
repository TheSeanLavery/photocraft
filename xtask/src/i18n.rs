//! Rust-aware UI inventory and deterministic TSV-to-Fluent catalog generator.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};

const LANGS: &[&str] = &["en", "ja", "zh-hans", "zh-hant", "es", "ru", "cs", "fr", "id", "ko", "de", "pt-br", "it"];

fn catalog_path(dir: &Path, lang: &str) -> PathBuf {
    let locale = match lang {
        "zh-hans" => "zh-CN",
        "zh-hant" => "zh-TW",
        "pt-br" => "pt-BR",
        _ => lang,
    };
    dir.join("locales").join(locale).join("messages.ftl")
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Key {
    context: String,
    source: String,
    id: String,
}

type Entry = (String, String, String);

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let dir = root.join("crates/ui-egui/src/i18n");
    match args.first().copied().unwrap_or("audit") {
        "generate" => generate(&root.join("crates/ui-egui/src"), &dir, args.contains(&"--force")),
        "check" => {
            check_catalogs(&dir)?;
            check_calls(&root.join("crates/ui-egui/src"), &dir)
        }
        "audit" => audit(&root.join("crates/ui-egui/src")),
        "migrate" => migrate(&root.join("crates/ui-egui/src"), &dir, args.get(1).copied().unwrap_or("--dry-run")),
        other => Err(format!("unknown i18n command: {other}")),
    }
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some(next) => {
                out.push('\\');
                out.push(next);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_tsv(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

fn parse_tsv(path: &Path) -> Result<Vec<Entry>, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (line, row) in content.lines().enumerate() {
        if row.trim().is_empty() || row.starts_with('#') {
            continue;
        }
        let cols: Vec<_> = row.split('\t').collect();
        if cols.len() != 3 || cols[1].is_empty() || cols[2].is_empty() {
            return Err(format!("{}:{}: expected three nonempty source/translation columns", path.display(), line + 1));
        }
        let entry = (unescape(cols[0]), unescape(cols[1]), unescape(cols[2]));
        if !seen.insert((entry.0.clone(), entry.1.clone())) {
            return Err(format!("{}:{}: duplicate key", path.display(), line + 1));
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn slug(source: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else {
            dash = true;
        }
        if out.len() >= 40 {
            break;
        }
    }
    if out.is_empty() { "text".to_string() } else { out.trim_end_matches('-').to_string() }
}

fn id(context: &str, source: &str) -> String {
    if context == "@id" {
        return format!("cmd-{}", slug(source));
    }
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in context.bytes().chain([0]).chain(source.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let stem = if context == "@plural" { source.split('|').next().unwrap_or(source) } else { source };
    format!("ui-{}-{hash:016x}", slug(stem))
}

fn make_keys(catalogs: &BTreeMap<&str, Vec<Entry>>) -> Result<Vec<Key>, String> {
    let mut by_key: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut by_id = BTreeMap::new();
    for entries in catalogs.values() {
        for (ctx, source, _) in entries {
            let generated = id(ctx, source);
            if let Some(previous) = by_id.insert(generated.clone(), (ctx.clone(), source.clone()))
                && previous != (ctx.clone(), source.clone())
            {
                return Err(format!("Fluent id collision `{generated}`: {previous:?} and ({ctx:?}, {source:?})"));
            }
            by_key.insert((ctx.clone(), source.clone()), generated);
        }
    }
    Ok(by_key.into_iter().map(|((context, source), id)| Key { context, source, id }).collect())
}

fn variables(input: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = input;
    while let Some(start) = rest.find('{') {
        let following = &rest[start + 1..];
        let Some(end) = following.find('}') else { break };
        let name = &following[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            out.insert(name.to_string());
        }
        rest = &following[end + 1..];
    }
    out
}

fn fluent_text(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('}') {
            let name = &after[..end];
            if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                out.push_str("{ $");
                out.push_str(name);
                out.push_str(" }");
                rest = &after[end + 1..];
                continue;
            }
        }
        out.push_str("{ \"{\" }");
        rest = after;
    }
    out.push_str(rest);
    out
}

fn plural_categories(lang: &str) -> &'static [&'static str] {
    match lang {
        "ja" | "zh-hans" | "zh-hant" | "id" | "ko" => &["other"],
        "ru" => &["one", "few", "many"],
        "cs" => &["one", "few", "other"],
        _ => &["one", "other"],
    }
}

fn message(lang: &str, context: &str, source: &str, translation: &str) -> Result<String, String> {
    if context == "@plural" {
        let source_forms: Vec<_> = source.split('|').collect();
        let forms: Vec<_> = translation.split('|').collect();
        let categories = plural_categories(lang);
        if source_forms.len() != 2 || forms.len() != categories.len() {
            return Err(format!("{lang}: invalid plural `{source}`: {} forms, expected {}", forms.len(), categories.len()));
        }
        let mut out = String::from("{ $n ->\n");
        for (category, form) in categories.iter().zip(forms) {
            let marker = if *category == "other" || *category == "many" { "*" } else { "" };
            out.push_str(&format!("    {marker}[{category}] {}\n", fluent_text(form)));
        }
        out.push_str("  }");
        return Ok(out);
    }
    let source_vars = variables(source);
    let translation_vars = variables(translation);
    if source_vars != translation_vars {
        return Err(format!("{lang}: variable mismatch for `{source}`: {source_vars:?} versus {translation_vars:?}"));
    }
    let lines: Vec<_> = translation.split('\n').collect();
    Ok(lines
        .iter()
        .enumerate()
        .map(|(index, line)| if index == 0 { fluent_text(line) } else { format!("    {}", fluent_text(line)) })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn render_keys(keys: &[Key]) -> String {
    let mut out = String::from("# context<TAB>source<TAB>Fluent ID; generated by cargo xtask i18n generate\n");
    for key in keys {
        out.push_str(&format!("{}\t{}\t{}\n", escape_tsv(&key.context), escape_tsv(&key.source), key.id));
    }
    out
}

fn render_ftl(lang: &str, entries: &[Entry], ids: &BTreeMap<(String, String), String>) -> Result<String, String> {
    let mut out = format!(
        "# PhotoCraft {lang} messages; initially converted from {lang}.tsv.\n# Edit this FTL catalog for translations. Stable IDs are recorded in keys.tsv.\n\n"
    );
    let mut rows = Vec::new();
    for (ctx, source, translation) in entries {
        // Command IDs are opaque identifiers; the English fallback is the command's label.
        if lang == "en" && ctx == "@id" {
            continue;
        }
        let Some(id) = ids.get(&(ctx.clone(), source.clone())) else {
            return Err(format!("missing id: {ctx}/{source}"));
        };
        rows.push((id, message(lang, ctx, source, translation)?));
    }
    rows.sort_by(|a, b| a.0.cmp(b.0));
    for (id, value) in rows {
        out.push_str(&format!("{id} = {value}\n\n"));
    }
    if let Err((_, errors)) = fluent_syntax::parser::parse(out.as_str()) {
        return Err(format!("{lang}: invalid generated Fluent catalog: {errors:?}"));
    }
    Ok(format!("{}\n", out.trim_end()))
}

fn generate(src: &Path, dir: &Path, force: bool) -> Result<(), String> {
    let mut catalogs = BTreeMap::new();
    for &lang in LANGS.iter().filter(|&&lang| lang != "en") {
        catalogs.insert(lang, parse_tsv(&dir.join(format!("{lang}.tsv")))?);
    }
    let mut keys = make_keys(&catalogs)?;
    let existing: BTreeSet<_> = keys.iter().filter(|key| key.context.is_empty()).map(|key| key.source.clone()).collect();
    let mut files = Vec::new();
    rust_files(src, &mut files)?;
    let mut inventory = Inventory::default();
    for file in files {
        let source = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        inventory.visit_file(&syn::parse_file(&source).map_err(|e| format!("{}: {e}", file.display()))?);
    }
    for source in inventory.literals.difference(&existing) {
        if variables(source).is_empty() {
            keys.push(Key { context: String::new(), source: source.clone(), id: id("", source) });
        }
    }
    keys.sort();
    let ids: BTreeMap<_, _> = keys.iter().map(|key| ((key.context.clone(), key.source.clone()), key.id.clone())).collect();
    let mut outputs = Vec::new();
    outputs.push((dir.join("keys.tsv"), render_keys(&keys)));
    let english: Vec<Entry> = keys.iter().map(|key| (key.context.clone(), key.source.clone(), key.source.clone())).collect();
    for &lang in LANGS {
        let entries = if lang == "en" { &english } else { catalogs.get(lang).ok_or_else(|| format!("missing {lang}"))? };
        outputs.push((catalog_path(dir, lang), render_ftl(lang, entries, &ids)?));
    }
    if !force && outputs.iter().any(|(path, _)| path.exists()) {
        return Err("Fluent catalogs already exist; use `i18n generate --force` only to repeat the one-time TSV migration".into());
    }
    for (path, content) in &outputs {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!("i18n: {} stable IDs, {} FTL catalogs generated", keys.len(), LANGS.len());
    Ok(())
}

#[derive(Default)]
struct Inventory {
    literal_tl: usize,
    dynamic_tl: usize,
    literals: BTreeSet<String>,
    dynamic_examples: Vec<String>,
}

impl<'ast> Visit<'ast> for Inventory {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("tl") {
            match syn::parse2::<syn::Expr>(node.tokens.clone()) {
                Ok(syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(value), .. })) => {
                    self.literal_tl += 1;
                    self.literals.insert(value.value());
                }
                _ => {
                    self.dynamic_tl += 1;
                    if self.dynamic_examples.len() < 20 {
                        self.dynamic_examples.push(node.tokens.to_string());
                    }
                }
            }
        }
        visit::visit_macro(self, node);
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn audit(src: &Path) -> Result<(), String> {
    let mut files = Vec::new();
    rust_files(src, &mut files)?;
    let mut inventory = Inventory::default();
    for file in files {
        let source = fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let parsed = syn::parse_file(&source).map_err(|e| format!("{}: {e}", file.display()))?;
        inventory.visit_file(&parsed);
    }
    println!("literal tl! calls: {}; unique strings: {}; dynamic tl! calls: {}", inventory.literal_tl, inventory.literals.len(), inventory.dynamic_tl);
    for example in &inventory.dynamic_examples {
        println!("  review dynamic: {example}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_contextual() {
        assert_eq!(id("", "Save"), id("", "Save"));
        assert_ne!(id("", "Save"), id("button", "Save"));
        assert_eq!(id("@id", "file.saveAs"), "cmd-file-saveas");
    }

    #[test]
    fn ast_inventory_ignores_comments_and_finds_dynamic_calls() {
        let file = syn::parse_file("// tl!(\"ignored\")\nfn f() { tl!(\"Hello\"); tl!(label); }").unwrap();
        let mut inventory = Inventory::default();
        inventory.visit_file(&file);
        assert_eq!((inventory.literal_tl, inventory.dynamic_tl), (1, 1));
    }

    #[test]
    fn unicode_span_columns_map_to_utf8_bytes() {
        let source = "fn f() { let _ = \"日本語\"; tl!(\"Hello\"); }";
        let parsed = syn::parse_file(source).unwrap();
        let mut rewrites = Rewrites::default();
        rewrites.visit_file(&parsed);
        let (span, literal) = rewrites.calls.first().unwrap();
        assert_eq!(literal, "Hello");
        let start = byte_position(source, span.start().line, span.start().column).unwrap();
        let end = byte_position(source, span.end().line, span.end().column).unwrap();
        assert_eq!(source.get(start..end), Some("tl!(\"Hello\")"));
    }

    #[test]
    fn plural_rules_and_variables() {
        assert!(message("ru", "@plural", "{n} item|{n} items", "{n} предмет|{n} предмета|{n} предметов").unwrap().contains("*[many]"));
        assert!(message("ja", "@plural", "{n} item|{n} items", "{n} 件").unwrap().contains("*[other]"));
        assert!(message("es", "", "Open {name}", "Abrir {other}").is_err());
    }
}

#[derive(Default)]
struct Rewrites {
    calls: Vec<(proc_macro2::Span, String)>,
    variable_literals: usize,
    dynamic: usize,
}

impl<'ast> Visit<'ast> for Rewrites {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if node.attrs.iter().any(|attr| attr.path().is_ident("cfg") && attr.parse_args::<syn::Ident>().is_ok_and(|id| id == "test")) {
            return;
        }
        visit::visit_item_mod(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("tl") {
            match syn::parse2::<syn::Expr>(node.tokens.clone()) {
                Ok(syn::Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(value), .. })) => {
                    if variables(&value.value()).is_empty() {
                        self.calls.push((node.span(), value.value()));
                    } else {
                        self.variable_literals += 1;
                    }
                }
                _ => self.dynamic += 1,
            }
        }
        visit::visit_macro(self, node);
    }
}

fn byte_position(text: &str, line: usize, column: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let start = text.split_inclusive('\n').take(line - 1).map(str::len).sum::<usize>();
    let line_text = text.get(start..)?.split('\n').next()?;
    let line_offset = if column == line_text.chars().count() { line_text.len() } else { line_text.char_indices().nth(column)?.0 };
    let end = start.checked_add(line_offset)?;
    (end <= text.len() && text.is_char_boundary(end)).then_some(end)
}

fn migrate(src: &Path, catalog_dir: &Path, mode: &str) -> Result<(), String> {
    if !matches!(mode, "--dry-run" | "--apply") {
        return Err(format!("unknown migrate mode `{mode}`; use --dry-run or --apply"));
    }
    let keys_text = fs::read_to_string(catalog_dir.join("keys.tsv")).map_err(|e| e.to_string())?;
    let ids: BTreeMap<String, String> = keys_text
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let (Some(context), Some(source), Some(id), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else { return None };
            (context.is_empty()).then(|| (unescape(source), id.to_string()))
        })
        .collect();
    let mut files = Vec::new();
    rust_files(src, &mut files)?;
    let mut count = 0;
    let mut missing = BTreeSet::new();
    let mut variable_literals = 0;
    let mut dynamic = 0;
    for path in files {
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = syn::parse_file(&source).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut rewrites = Rewrites::default();
        rewrites.visit_file(&parsed);
        variable_literals += rewrites.variable_literals;
        dynamic += rewrites.dynamic;
        let mut edits = Vec::new();
        for (span, literal) in rewrites.calls {
            let Some(id) = ids.get(&literal) else {
                missing.insert(literal);
                continue;
            };
            let (Some(start), Some(end)) =
                (byte_position(&source, span.start().line, span.start().column), byte_position(&source, span.end().line, span.end().column))
            else {
                return Err(format!("{}: invalid macro span", path.display()));
            };
            let Some(original) = source.get(start..end) else {
                return Err(format!("{}: invalid macro offset", path.display()));
            };
            if !original.starts_with("tl!") || !original.ends_with(')') {
                return Err(format!("{}: unexpected macro span `{original}`", path.display()));
            }
            edits.push((start, end, format!("tl_id!(\"{id}\")")));
        }
        if !edits.is_empty() {
            edits.sort_by_key(|edit| std::cmp::Reverse(edit.0));
            let mut updated = source.clone();
            for (start, end, replacement) in &edits {
                updated.replace_range(*start..*end, replacement);
            }
            syn::parse_file(&updated).map_err(|e| format!("{}: migrated syntax: {e}", path.display()))?;
            count += edits.len();
            println!("{}: {} static calls", path.display(), edits.len());
            if mode == "--apply" {
                fs::write(&path, updated).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    println!(
        "i18n migrate {mode}: {count} static calls; {variable_literals} variable templates; {dynamic} dynamic calls; {} literal keys missing",
        missing.len()
    );
    for item in missing.iter().take(20) {
        println!("  missing: {item}");
    }
    Ok(())
}

#[derive(Default)]
struct CallCheck {
    ids: BTreeSet<String>,
    plain: BTreeSet<String>,
    invalid: usize,
}

impl<'ast> Visit<'ast> for CallCheck {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if node.path.is_ident("tl_id") || node.path.is_ident("tl") {
            let is_id = node.path.is_ident("tl_id");
            match syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                Ok(value) => {
                    if is_id {
                        self.ids.insert(value.value());
                    } else {
                        self.plain.insert(value.value());
                    }
                }
                Err(_) if is_id => self.invalid += 1,
                Err(_) => (), // Dynamic tl! calls remain on the compatibility bridge.
            }
        }
        visit::visit_macro(self, node);
    }
}

fn check_calls(src: &Path, catalog_dir: &Path) -> Result<(), String> {
    let key_text = fs::read_to_string(catalog_dir.join("keys.tsv")).map_err(|e| e.to_string())?;
    let mut ids = BTreeSet::new();
    let mut plain = BTreeSet::new();
    for line in key_text.lines().filter(|line| !line.starts_with('#')) {
        let mut fields = line.split('\t');
        let (Some(context), Some(source), Some(id), None) = (fields.next(), fields.next(), fields.next(), fields.next()) else {
            return Err("malformed keys.tsv row".into());
        };
        ids.insert(id.to_string());
        if context.is_empty() {
            plain.insert(unescape(source));
        }
    }
    let mut files = Vec::new();
    rust_files(src, &mut files)?;
    let mut used = CallCheck::default();
    for path in files {
        let source = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        used.visit_file(&syn::parse_file(&source).map_err(|e| format!("{}: {e}", path.display()))?);
    }
    let unknown_ids: Vec<_> = used.ids.difference(&ids).collect();
    let unknown_plain: Vec<_> = used.plain.difference(&plain).filter(|s| variables(s).is_empty()).collect();
    if used.invalid > 0 || !unknown_ids.is_empty() || !unknown_plain.is_empty() {
        return Err(format!(
            "i18n call audit: {} nonliteral tl_id calls; unknown ids: {unknown_ids:?}; unknown plain strings: {unknown_plain:?}",
            used.invalid
        ));
    }
    println!("i18n AST audit: {} static IDs and {} compatibility literals valid", used.ids.len(), used.plain.len());
    Ok(())
}

fn ftl_variables(value: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = value;
    while let Some(at) = rest.find('$') {
        let after = &rest[at + 1..];
        let name: String = after.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        let consumed = name.len();
        if !name.is_empty() {
            out.insert(name);
        }
        rest = &after[consumed..];
    }
    out
}

fn check_catalogs(dir: &Path) -> Result<(), String> {
    let ledger = fs::read_to_string(dir.join("keys.tsv")).map_err(|e| e.to_string())?;
    let mut keys = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for (line_no, line) in ledger.lines().enumerate() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 3 {
            return Err(format!("keys.tsv:{}: expected three columns", line_no + 1));
        }
        let ctx = unescape(fields[0]);
        let source = unescape(fields[1]);
        let stable_id = fields[2].to_string();
        if !ids.insert(stable_id.clone()) {
            return Err(format!("duplicate key ID: {stable_id}"));
        }
        if keys.insert(stable_id.clone(), (ctx, source)).is_some() {
            return Err(format!("duplicate key ID: {stable_id}"));
        }
    }
    if keys.is_empty() {
        return Err("empty i18n key ledger".into());
    }
    for &lang in LANGS {
        let path = catalog_path(dir, lang);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let resource = fluent_syntax::parser::parse(text.as_str()).map_err(|(_, errors)| format!("{}: {errors:?}", path.display()))?;
        let mut seen = BTreeSet::new();
        for entry in resource.body {
            if let fluent_syntax::ast::Entry::Message(message) = entry {
                let message_id = message.id.name;
                if !seen.insert(message_id.to_string()) {
                    return Err(format!("{lang}: duplicate FTL ID `{message_id}`"));
                }
                let Some((_, source)) = keys.get(message_id) else {
                    return Err(format!("{lang}: unknown FTL ID `{message_id}`"));
                };
                let expected = variables(source);
                // Inspect the exact message block, including selector variants. Fluent syntax itself is
                // validated by the parser above; this checks translation variable names.
                let prefix = format!("{message_id} = ");
                let Some(block) = text.split("\n\n").find(|block| block.starts_with(&prefix)) else {
                    return Err(format!("{lang}: missing message block `{message_id}`"));
                };
                let actual = ftl_variables(block);
                if actual != expected {
                    return Err(format!("{lang}: `{message_id}` variables {actual:?}, expected {expected:?}"));
                }
            }
        }
        if lang == "en" {
            let required: BTreeSet<_> = keys.iter().filter(|(_, (ctx, _))| ctx != "@id").map(|(id, _)| id.clone()).collect();
            let missing: Vec<_> = required.difference(&seen).take(10).collect();
            if !missing.is_empty() {
                return Err(format!("English FTL missing IDs: {missing:?}"));
            }
        }
        println!("{lang}: {} Fluent messages validated", seen.len());
    }
    Ok(())
}
