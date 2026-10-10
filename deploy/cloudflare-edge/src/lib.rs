//! A stable Cloudflare address for the native Rust playtest room service.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(any(test, target_arch = "wasm32"))]
fn allowed(method: &str, path: &str) -> bool {
    match method {
        "GET" | "HEAD" => matches!(path, "/" | "/health") || path.starts_with("/downloads/"),
        "POST" => matches!(path, "/create" | "/join" | "/signal" | "/poll" | "/leave" | "/kick" | "/ice"),
        _ => false,
    }
}
#[cfg(any(test, target_arch = "wasm32"))]
fn origin_url(origin: &str, path: &str, query: Option<&str>) -> Result<String, &'static str> {
    let mut url = url::Url::parse(origin).map_err(|_| "Invalid room origin")?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err("Room origin must be an HTTPS hostname");
    }
    // set_path preserves the configured origin even for paths beginning with //.
    url.set_path(path);
    url.set_query(query);
    url.set_fragment(None);
    Ok(url.into())
}

#[cfg(target_arch = "wasm32")]
#[worker::event(fetch)]
async fn main(mut req: worker::Request, env: worker::Env, _ctx: worker::Context) -> worker::Result<worker::Response> {
    use futures_util::StreamExt;
    use worker::*;
    let method = req.method();
    let url = req.url()?;
    if !allowed(method.as_ref(), url.path()) {
        return Response::error("Route unavailable", 404);
    }
    let origin = match env.secret("ROOM_ORIGIN") {
        Ok(origin) => origin.to_string(),
        Err(_) => return Response::error("PhotoCraft room service is not configured", 503),
    };
    let upstream = match origin_url(&origin, url.path(), url.query()) {
        Ok(upstream) => upstream,
        Err(_) => return Response::error("PhotoCraft room service is not configured", 503),
    };
    let headers = Headers::new();
    headers.set("User-Agent", "PhotoCraft-dev")?;
    // Pass only headers needed by JSON signaling and resumable artifact downloads.
    for name in ["content-type", "range", "if-range", "if-none-match", "if-modified-since"] {
        if let Some(value) = req.headers().get(name)? {
            headers.set(name, &value)?;
        }
    }
    let mut init = RequestInit::new();
    init.with_method(method.clone()).with_headers(headers).with_redirect(RequestRedirect::Error);
    if method == Method::Post {
        if req.headers().get("content-length")?.and_then(|n| n.parse::<usize>().ok()).is_some_and(|n| n > 96 * 1024) {
            return Response::error("Request too large", 413);
        }
        let mut body = Vec::new();
        let mut stream = match req.stream() {
            Ok(stream) => stream,
            Err(_) => return Response::error("JSON body required", 400),
        };
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.len() > (96 * 1024usize).saturating_sub(body.len()) {
                return Response::error("Request too large", 413);
            }
            body.extend_from_slice(&chunk);
        }
        let text = match String::from_utf8(body) {
            Ok(text) => text,
            Err(_) => return Response::error("UTF-8 JSON required", 400),
        };
        init.with_body(Some(wasm_bindgen::JsValue::from_str(&text)));
    }
    // Preserve streaming responses; never buffer downloadable application bundles.
    match Fetch::Request(Request::new_with_init(&upstream, &init)?).send().await {
        Ok(response) => Ok(response),
        Err(_) => Response::error("PhotoCraft playtest host is offline. Try again when the host is running.", 503),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_room_and_download_routes_are_public() {
        assert!(allowed("POST", "/ice"));
        assert!(allowed("GET", "/downloads/PhotoCraft.zip"));
        assert!(!allowed("GET", "/ice"));
        assert!(!allowed("POST", "/engine.execute"));
        assert!(!allowed("GET", "/private/token"));
    }
    #[test]
    fn untrusted_paths_cannot_replace_the_origin() {
        let url = origin_url("https://room.example", "//attacker.example/ice", None).unwrap();
        assert_eq!(url::Url::parse(&url).unwrap().host_str(), Some("room.example"));
        assert!(origin_url("http://room.example", "/ice", None).is_err());
        assert!(origin_url("https://user:password@room.example", "/ice", None).is_err());
    }
}
