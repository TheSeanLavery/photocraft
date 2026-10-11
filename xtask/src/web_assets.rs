//! Pack static WebAssembly assets separately, preserving the complete executable and memory layout.
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use sha2::{Digest, Sha256, Sha384};

#[path = "web_assets_format.rs"]
mod format;

const MAX_RUNTIME_BYTES: u64 = 24 << 20;
const MANIFEST: &str = "photocraft-static-assets.json";

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn integrity(bytes: &[u8]) -> String {
    format!("sha384-{}", base64::engine::general_purpose::STANDARD.encode(Sha384::digest(bytes)))
}
fn hash_prefix(hash: &str) -> Result<&str, String> {
    hash.get(..16).ok_or_else(|| "Invalid asset digest".into())
}
fn read(path: &Path) -> Result<Vec<u8>, String> {
    let len = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
    if len > 64 << 20 {
        return Err(format!("{} exceeds asset processing limit", path.display()));
    }
    fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}
fn replace_once(source: &str, old: &str, new: &str) -> Result<String, String> {
    if source.matches(old).count() != 1 {
        return Err("Generated wasm-bindgen glue changed: expected exactly one initialization anchor".into());
    }
    Ok(source.replacen(old, new, 1))
}

/// Recognized generated glue only: validate both compiled modules before instantiation/start.
fn rewrite_glue(source: &str, main: &str, assets: &str, bundle: &str, asset_integrity: &str, main_integrity: &str) -> Result<String, String> {
    if source.matches("    wasm.__wbindgen_start();").count() != 1 {
        return Err("Unrecognized wasm-bindgen explicit start sequence".into());
    }
    let start = source.find("function initSync(module) {\n").ok_or("Unrecognized wasm-bindgen sync initializer")?;
    let end = source.find("\nasync function __wbg_init(module_or_path) {").ok_or("Unrecognized wasm-bindgen async initializer")?;
    let sync = source.get(start..end).ok_or("Invalid wasm-bindgen initializer order")?;
    if !sync.contains("return __wbg_finalize_init(instance, module);") {
        return Err("Unrecognized wasm-bindgen sync finalizer".into());
    }
    let helpers = format!(
        r#"function __pc_assert_bundle(module, role) {{
    const sections = WebAssembly.Module.customSections(module, 'photocraft.static-assets');
    const expected = '{version}\n' + role + '\n{bundle}\n';
    if (sections.length !== 1 || new TextDecoder('utf-8', {{fatal:true}}).decode(sections[0]) !== expected) {{
        throw new Error('PhotoCraft executable and static assets do not match. Reload the complete site.');
    }}
}}
async function __pc_compile(input) {{
    if (input instanceof WebAssembly.Module) return input;
    if (typeof Response === 'function' && input instanceof Response) {{
        if (!input.ok) throw new Error(`failed to fetch PhotoCraft static runtime: ${{input.status}} ${{input.statusText}} fetching '${{input.url}}'`);
        if (typeof WebAssembly.compileStreaming === 'function') {{
            try {{ return await WebAssembly.compileStreaming(input.clone()); }}
            catch (error) {{
                if (!['basic', 'cors', 'default'].includes(input.type) || input.headers.get('Content-Type') === 'application/wasm') throw error;
                console.warn('PhotoCraft: Wasm MIME type is not application/wasm; falling back to buffered compilation.', error);
            }}
        }}
        return await WebAssembly.compile(await input.arrayBuffer());
    }}
    return await WebAssembly.compile(input);
}}
function __pc_apply_assets(instance, module) {{
    if (!(instance.exports.memory instanceof WebAssembly.Memory)) throw new Error('PhotoCraft executable has no exported memory.');
    new WebAssembly.Instance(module, {{env: {{memory: instance.exports.memory}}}});
}}
async function __pc_apply_assets_async(instance, module) {{
    if (!(instance.exports.memory instanceof WebAssembly.Memory)) throw new Error('PhotoCraft executable has no exported memory.');
    await WebAssembly.instantiate(module, {{env: {{memory: instance.exports.memory}}}});
}}
function initSync(module) {{
    if (wasm !== undefined) return wasm;
    let static_assets;
    if (module !== undefined && Object.getPrototypeOf(module) === Object.prototype) {{
        ({{module, static_assets}} = module);
    }} else if (module !== undefined) {{
        console.warn('using deprecated parameters for initSync(); pass {{module, static_assets}} instead');
    }}
    if (static_assets === undefined) throw new Error('PhotoCraft packed initSync requires a preloaded static_assets module or bytes. Use asynchronous init to fetch it.');
    if (!(module instanceof WebAssembly.Module)) module = new WebAssembly.Module(module);
    if (!(static_assets instanceof WebAssembly.Module)) static_assets = new WebAssembly.Module(static_assets);
    __pc_assert_bundle(module, 'executable');
    __pc_assert_bundle(static_assets, 'assets');
    const instance = new WebAssembly.Instance(module, __wbg_get_imports());
    __pc_apply_assets(instance, static_assets);
    return __wbg_finalize_init(instance, module);
}}
"#,
        version = format::PACK_VERSION
    );
    let mut result = source.to_owned();
    result.replace_range(start..end, &helpers);
    result = replace_once(&result, "async function __wbg_init(module_or_path) {", "async function __wbg_init(module_or_path) {\n    let static_assets;")?;
    result = replace_once(&result, "({module_or_path} = module_or_path)", "({module_or_path, static_assets} = module_or_path)")?;
    result = replace_once(&result, "new URL('photocraft-web_bg.wasm', import.meta.url)", &format!("new URL('{main}', import.meta.url)"))?;
    result = replace_once(
        &result,
        "module_or_path = fetch(module_or_path);",
        &format!("module_or_path = fetch(module_or_path, {{integrity: '{main_integrity}'}});"),
    )?;
    let initialize = format!(
        r#"    const module = await __pc_compile(await module_or_path);
    __pc_assert_bundle(module, 'executable');
    if (static_assets === undefined) {{
        static_assets = await fetch(new URL('{assets}', import.meta.url), {{integrity: '{asset_integrity}'}});
    }}
    const assets = await __pc_compile(static_assets);
    __pc_assert_bundle(assets, 'assets');
    const instance = await WebAssembly.instantiate(module, imports);
    await __pc_apply_assets_async(instance, assets);
    return __wbg_finalize_init(instance, module);"#
    );
    replace_once(
        &result,
        "    const { instance, module } = await __wbg_load(await module_or_path, imports);\n\n    return __wbg_finalize_init(instance, module);",
        &initialize,
    )
}

fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let metadata = fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err(format!("Runtime site contains a symlink: {}", path.display()));
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(path);
            }
            if files.len().saturating_add(pending.len()) > 100_000 {
                return Err("Too many runtime site assets".into());
            }
        }
    }
    files.sort();
    Ok(files)
}
fn check_caps(root: &Path) -> Result<(), String> {
    for path in files(root)? {
        let len = fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if len > MAX_RUNTIME_BYTES {
            return Err(format!("{}: {len} bytes exceeds unchanged {MAX_RUNTIME_BYTES}-byte runtime asset cap", path.display()));
        }
    }
    Ok(())
}
fn validate_manifest(root: &Path) -> Result<(), String> {
    let manifest: serde_json::Value = serde_json::from_slice(&read(&root.join(MANIFEST))?).map_err(|e| e.to_string())?;
    if manifest.get("schema").and_then(|v| v.as_u64()) != Some(1) || manifest.get("packing").and_then(|v| v.as_str()) != Some(format::PACK_VERSION) {
        return Err("Unsupported static asset packing manifest version".into());
    }
    let bundle = manifest.get("bundle").and_then(|v| v.as_str()).ok_or("Missing static asset bundle binding")?;
    let html = String::from_utf8(read(&root.join("index.html"))?).map_err(|e| e.to_string())?;
    for kind in ["main", "assets", "glue"] {
        let filename = manifest.get(kind).and_then(|v| v.as_str()).ok_or("Invalid asset manifest filename")?;
        if filename.contains(['/', '\\']) || filename.contains("..") {
            return Err("Unsafe asset manifest path".into());
        }
        let bytes = read(&root.join(filename))?;
        let actual = digest(&bytes);
        let expected = manifest.get(format!("{kind}_sha256")).and_then(|v| v.as_str()).ok_or("Invalid asset manifest digest")?;
        if actual != expected {
            return Err(format!("Packed {kind} asset does not match manifest"));
        }
        if kind != "glue" {
            format::validate_marker(&bytes, if kind == "main" { "executable" } else { "assets" }, bundle)?;
        }
        if kind != "assets" && (!html.contains(filename) || !html.contains(&integrity(&bytes))) {
            return Err(format!("HTML preload binding missing for packed {kind}"));
        }
    }
    check_caps(root)
}

/// Generate a complete site in a sibling staging directory; publish only after every check passes.
pub fn run(args: &[&str]) -> Result<(), String> {
    let root = match args {
        ["--dir", path] => PathBuf::from(path),
        [] => std::env::var_os("TRUNK_STAGING_DIR").map(PathBuf::from).ok_or("web-assets needs --dir or TRUNK_STAGING_DIR")?,
        _ => return Err("usage: cargo xtask web-assets [--dir <Trunk site directory>]".into()),
    };
    if !root.is_dir() {
        return Err(format!("{} is not a site directory", root.display()));
    }
    if root.join(MANIFEST).is_file() {
        validate_manifest(&root)?;
        println!("static assets already packed and validated");
        return Ok(());
    }
    let candidates = files(&root)?
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("photocraft-web-") && n.ends_with("_bg.wasm")))
        .collect::<Vec<_>>();
    if candidates.len() != 1 {
        return Err("Expected exactly one Trunk main executable for static asset packing".into());
    }
    let main_path = candidates.first().ok_or("Missing Trunk main executable")?;
    let old_main = main_path.file_name().and_then(|n| n.to_str()).ok_or("Invalid main executable filename")?;
    let old_glue = format!("{}.js", old_main.strip_suffix("_bg.wasm").ok_or("Invalid main executable suffix")?);
    let original = read(main_path)?;
    let glue_bytes = read(&root.join(&old_glue))?;
    let glue = std::str::from_utf8(&glue_bytes).map_err(|e| e.to_string())?;
    let original_html = read(&root.join("index.html"))?;
    let html = std::str::from_utf8(&original_html).map_err(|e| e.to_string())?;
    let bundle = digest(&original);
    let packed = format::pack(&original, &bundle)?;
    let main_hash = digest(&packed.executable);
    let asset_hash = digest(&packed.assets);
    let main_name = format!("photocraft-web-{}_bg.wasm", hash_prefix(&main_hash)?);
    let asset_name = format!("photocraft-static-assets-{}.wasm", hash_prefix(&asset_hash)?);
    let asset_sri = integrity(&packed.assets);
    let new_glue = rewrite_glue(glue, &main_name, &asset_name, &bundle, &asset_sri, &integrity(&packed.executable))?;
    let glue_hash = digest(new_glue.as_bytes());
    let glue_name = format!("photocraft-web-{}.js", hash_prefix(&glue_hash)?);
    if !html.contains(old_main) || !html.contains(&old_glue) {
        return Err("Trunk HTML does not reference the expected executable/glue".into());
    }
    let old_main_sri = integrity(&original);
    let old_glue_sri = integrity(&glue_bytes);
    if html.matches(&old_main_sri).count() != 1 || html.matches(&old_glue_sri).count() != 1 {
        return Err("Trunk HTML SRI differs from original executable/glue".into());
    }
    let new_html = html
        .replace(old_main, &main_name)
        .replace(&old_glue, &glue_name)
        .replace(&old_main_sri, &integrity(&packed.executable))
        .replace(&old_glue_sri, &integrity(new_glue.as_bytes()));
    let parent = root.parent().ok_or("Site has no parent directory")?;
    let leaf = root.file_name().and_then(|n| n.to_str()).ok_or("Invalid site directory name")?;
    let next = parent.join(format!(".{leaf}-assets-next-{}", std::process::id()));
    let backup = parent.join(format!(".{leaf}-assets-previous-{}", std::process::id()));
    if next.exists() || backup.exists() {
        return Err("Static asset staging directory already exists".into());
    }
    fs::create_dir(&next).map_err(|e| e.to_string())?;
    let staged = (|| -> Result<(), String> {
        for file in files(&root)? {
            let relative = file.strip_prefix(&root).map_err(|e| e.to_string())?;
            if relative == Path::new(old_main) || relative == Path::new(&old_glue) || relative == Path::new("index.html") {
                continue;
            }
            let destination = next.join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(file, destination).map_err(|e| e.to_string())?;
        }
        for (filename, bytes) in [(&main_name, packed.executable.as_slice()), (&asset_name, packed.assets.as_slice()), (&glue_name, new_glue.as_bytes())] {
            fs::write(next.join(filename), bytes).map_err(|e| e.to_string())?;
        }
        fs::write(next.join("index.html"), new_html).map_err(|e| e.to_string())?;
        let manifest = serde_json::json!({"schema":1,"packing":format::PACK_VERSION,"bundle":bundle,"main":main_name,"main_sha256":main_hash,"assets":asset_name,"assets_sha256":asset_hash,"glue":glue_name,"glue_sha256":glue_hash,"original_bytes":original.len(),"main_bytes":packed.executable.len(),"asset_bytes":packed.assets.len(),"static_payload_bytes":packed.segments.iter().map(|s|s.bytes.len()).sum::<usize>(),"active_segments":packed.segments.len(),"memory_bytes":packed.memory_bytes});
        fs::write(next.join(MANIFEST), serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        validate_manifest(&next)
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&next);
        return Err(error);
    }
    fs::rename(&root, &backup).map_err(|e| e.to_string())?;
    if let Err(error) = fs::rename(&next, &root) {
        let restoration = fs::rename(&backup, &root);
        return Err(format!("Publishing packed site: {error}; original restore: {restoration:?}"));
    }
    fs::remove_dir_all(&backup).map_err(|e| format!("Packed site published; old staging cleanup: {e}"))?;
    println!(
        "static assets packed: executable {} bytes, assets {} bytes, {} active segments; bundle {}",
        packed.executable.len(),
        packed.assets.len(),
        packed.segments.len(),
        bundle
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn glue() -> &'static str {
        "let wasm;\nfunction __wbg_finalize_init(instance, module) {\n    wasm.__wbindgen_start();\n}\nfunction initSync(module) {\n    return __wbg_finalize_init(instance, module);\n}\nasync function __wbg_init(module_or_path) {\n    ({module_or_path} = module_or_path)\n    module_or_path = new URL('photocraft-web_bg.wasm', import.meta.url);\n    module_or_path = fetch(module_or_path);\n    const { instance, module } = await __wbg_load(await module_or_path, imports);\n\n    return __wbg_finalize_init(instance, module);\n}\n"
    }
    #[test]
    fn generated_loader_validates_pair_before_instantiation_and_start() {
        let rewritten = rewrite_glue(glue(), "main.wasm", "assets.wasm", &"a".repeat(64), "sha384-digest", "sha384-main").unwrap();
        let async_init = rewritten.split("async function __wbg_init").nth(1).unwrap();
        assert!(async_init.find("__pc_assert_bundle(assets, 'assets')").unwrap() < async_init.find("await WebAssembly.instantiate(module").unwrap());
        assert!(async_init.find("await __pc_apply_assets_async(instance, assets)").unwrap() < async_init.find("__wbg_finalize_init").unwrap());
        assert!(rewritten.contains("new URL('assets.wasm', import.meta.url), {integrity: 'sha384-digest'}"));
        assert!(rewritten.contains("input.clone()"));
        assert!(rewritten.contains("fetch(module_or_path, {integrity: 'sha384-main'})"));
        assert!(
            rewrite_glue(
                &glue().replace("module_or_path = fetch(module_or_path);", "different compiler fetch"),
                "main.wasm",
                "assets.wasm",
                "digest",
                "integrity",
                "main-integrity"
            )
            .is_err()
        );
        assert!(rewritten.contains("packed initSync requires a preloaded static_assets"));
    }
    #[test]
    fn changed_or_duplicated_generated_glue_is_rejected() {
        assert!(rewrite_glue("unknown compiler glue", "main.wasm", "assets.wasm", "digest", "integrity", "main-integrity").is_err());
        assert!(rewrite_glue(&glue().repeat(2), "main.wasm", "assets.wasm", "digest", "integrity", "main-integrity").is_err());
        assert!(
            rewrite_glue(
                &glue().replace("({module_or_path} = module_or_path)", "different compiler parameter"),
                "main.wasm",
                "assets.wasm",
                "digest",
                "integrity",
                "main-integrity"
            )
            .is_err()
        );
    }
}
