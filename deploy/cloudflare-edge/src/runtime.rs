use crate::room::{Auth, Room, Signal, code};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use worker::*;

fn now() -> u64 {
    Date::now().as_millis()
}
fn identity() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| Error::RustError("Secure identity unavailable".into()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn api<T>(r: crate::room::ApiResult<T>) -> Result<T> {
    r.map_err(|(status, message)| Error::RustError(format!("{status}:{message}")))
}
async fn bounded(req: &mut Request, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut stream = req.stream()?;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > limit.saturating_sub(out.len()) {
            return Err(Error::RustError("413:Request too large".into()));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}
fn value<T: serde::de::DeserializeOwned>(v: &Value) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| Error::RustError("400:Invalid JSON parameters".into()))
}
fn error_response(error: Error) -> Result<Response> {
    let text = error.to_string();
    // Only expose our explicit API errors; internal storage/provider details stay private.
    for status in [400, 401, 403, 404, 409, 413, 429, 502, 503] {
        let prefix = format!("{status}:");
        if let Some((_, message)) = text.split_once(&prefix) {
            return Response::from_json(&json!({"error":message})).map(|r| r.with_status(status));
        }
    }
    Response::from_json(&json!({"error":"PhotoCraft service temporarily unavailable"})).map(|r| r.with_status(503))
}
fn cors(mut response: Response) -> Result<Response> {
    let h = Headers::new();
    for (k, v) in response.headers().entries() {
        h.set(&k, &v)?;
    }
    h.set("Access-Control-Allow-Origin", "*")?;
    h.set("Access-Control-Allow-Methods", "POST, OPTIONS")?;
    h.set("Access-Control-Allow-Headers", "Content-Type")?;
    h.set("Access-Control-Max-Age", "600")?;
    h.set("Cache-Control", "no-store")?;
    response = response.with_headers(h);
    Ok(response)
}
pub async fn fetch(mut req: Request, env: Env) -> Result<Response> {
    let path = req.url()?.path().to_owned();
    if crate::room_route(&path) {
        if req.method() == Method::Options {
            return cors(Response::empty()?.with_status(204));
        }
        if req.method() != Method::Post {
            return cors(Response::error("Method unavailable", 405)?);
        }
        let result = route(&mut req, &env, &path).await;
        return cors(match result {
            Ok(r) => r,
            Err(e) => error_response(e)?,
        });
    }
    if matches!(req.method(), Method::Get | Method::Head) {
        if let Some(destination) = crate::join_destination(&path) {
            let response = Response::redirect_with_status(req.url()?.join(&destination)?, 302)?;
            let headers = Headers::new();
            headers.set("Location", req.url()?.join(&destination)?.as_str())?;
            headers.set("Cache-Control", "no-store")?;
            headers.set("Referrer-Policy", "no-referrer")?;
            return Ok(response.with_headers(headers));
        }
        if path == "/app" {
            return Response::redirect_with_status(req.url()?.join("/app/")?, 302);
        }
        if path == "/health" {
            return Response::from_json(&json!({"service":"PhotoCraft","rooms":"durable-objects","origin_required":false}));
        }
        return asset(req, env).await;
    }
    Response::error("Route unavailable", 404)
}
async fn route(req: &mut Request, env: &Env, path: &str) -> Result<Response> {
    // Bound unauthenticated admissions without throttling regular room polling.
    if matches!(path, "/create" | "/join" | "/ice") {
        let ip = req.headers().get("CF-Connecting-IP")?.unwrap_or_else(|| "unknown".into());
        if !env.get_binding::<RateLimiter>("ADMISSION_LIMIT")?.limit(ip).await?.success {
            return Response::error("Too many room attempts", 429);
        }
    }
    let bytes = bounded(req, 96 * 1024).await?;
    let mut body: Value = serde_json::from_slice(&bytes).map_err(|_| Error::RustError("400:Invalid JSON body".into()))?;
    if !body.is_object() {
        return Err(Error::RustError("400:JSON object required".into()));
    }
    if body.get("password").is_some_and(|p| !p.is_null() && !p.is_string()) {
        return Err(Error::RustError("400:Password must be a string".into()));
    }
    let room_code = if path == "/create" {
        // 32-bit join codes match native clients; retry collision without overwriting a room.
        let token = identity()?;
        let c = token.get(..8).ok_or_else(|| Error::RustError("Secure code unavailable".into()))?.to_ascii_uppercase();
        body["code"] = json!(c);
        c
    } else {
        let c = if matches!(path, "/signal" | "/kick") { body.get("auth").and_then(|a| a.get("code")) } else { body.get("code") };
        api(code(c.and_then(Value::as_str).ok_or_else(|| Error::RustError("400:Room code required".into()))?))?
    };
    let namespace = env.durable_object("ROOMS")?;
    let stub = namespace.id_from_name(&room_code)?.get_stub()?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_body(Some(wasm_bindgen::JsValue::from_str(&body.to_string())));
    init.headers.set("Content-Type", "application/json")?;
    stub.fetch_with_request(Request::new_with_init(&format!("https://room.internal{path}"), &init)?).await
}
async fn asset(req: Request, env: Env) -> Result<Response> {
    let path = req.url()?.path().to_owned();
    let key = crate::asset_key(&path).ok_or_else(|| Error::RustError("404:Asset unavailable".into()));
    let key = match key {
        Ok(k) => k,
        Err(e) => return error_response(e),
    };
    let bucket = env.bucket("WEB_ASSETS")?;
    let Some(object) = bucket.get(&key).execute().await? else {
        return Response::error("Asset unavailable", 404);
    };
    let headers = Headers::new();
    headers.set(
        "Content-Type",
        match key.rsplit_once('.').map(|(_, ext)| ext) {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("wasm") => "application/wasm",
            Some("png") => "image/png",
            Some("css") => "text/css; charset=utf-8",
            Some("svg") => "image/svg+xml",
            Some("ico") => "image/x-icon",
            Some("json") => "application/json",
            _ => "application/octet-stream",
        },
    )?;
    headers.set("ETag", &object.http_etag())?;
    headers.set("Cache-Control", if key.ends_with("index.html") || key.ends_with(".json") { "no-cache" } else { "public, max-age=31536000, immutable" })?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Cross-Origin-Resource-Policy", "same-origin")?;
    headers.set("Referrer-Policy", "strict-origin-when-cross-origin")?;
    let response = if req.headers().get("If-None-Match")?.as_deref() == Some(&object.http_etag()) {
        Response::empty()?.with_status(304)
    } else if req.method() == Method::Head {
        Response::empty()?
    } else if let Some(body) = object.body() {
        Response::from_stream(body.stream()?)?
    } else {
        Response::empty()?
    };
    Ok(response.with_headers(headers))
}

#[durable_object]
pub struct PhotoCraftRoom {
    state: State,
    env: Env,
}
#[derive(Deserialize)]
struct Stored {
    data: String,
}
impl PhotoCraftRoom {
    fn load(&self) -> Result<Option<Room>> {
        let sql = self.state.storage().sql();
        sql.exec("CREATE TABLE IF NOT EXISTS room (id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL)", None)?;
        let rows = sql.exec("SELECT data FROM room WHERE id=1", None)?.to_array::<Stored>()?;
        rows.first().map(|r| serde_json::from_str(&r.data).map_err(Error::from)).transpose()
    }
    fn save(&self, room: &Room) -> Result<()> {
        // Synchronous SQLite commits before the response; no await can interleave read/mutate/write.
        self.state
            .storage()
            .sql()
            .exec("INSERT INTO room(id,data) VALUES(1,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data", Some(vec![api(room.encoded())?.into()]))?;
        Ok(())
    }
    async fn handle(&self, mut req: Request) -> Result<Response> {
        let path = req.url()?.path().to_owned();
        let bytes = bounded(&mut req, 96 * 1024).await?;
        let body: Value = serde_json::from_slice(&bytes).map_err(|_| Error::RustError("400:Invalid JSON body".into()))?;
        let t = now();
        let stored = self.load()?;
        if path == "/create" {
            if stored.as_ref().is_some_and(|r| r.active(t)) {
                return Response::from_json(&json!({"error":"Room code collision; retry create"})).map(|r| r.with_status(409));
            }
            let c = api(code(body.get("code").and_then(Value::as_str).unwrap_or("")))?;
            let room = api(Room::create(c, body.get("password").and_then(Value::as_str), identity()?, identity()?, t))?;
            let admission = api(room.admission(&room.host))?;
            self.save(&room)?;
            self.state.storage().set_alarm(std::time::Duration::from_secs(60)).await?;
            return Response::from_json(&admission);
        }
        let mut room = stored.ok_or_else(|| Error::RustError("404:Room unavailable".into()))?;
        let before = api(room.encoded())?;
        let mut deleted = false;
        let response = match path.as_str() {
            "/join" => Response::from_json(&api(room.join(body.get("password").and_then(Value::as_str), identity()?, identity()?, t))?)?,
            "/poll" => Response::from_json(&api(room.poll(&value::<Auth>(&body)?, t))?)?,
            "/signal" => {
                api(room.send(&value(body.get("auth").unwrap_or(&Value::Null))?, value::<Signal>(body.get("signal").unwrap_or(&Value::Null))?, t))?;
                Response::empty()?.with_status(204)
            }
            "/kick" => {
                api(room.kick(&value(body.get("auth").unwrap_or(&Value::Null))?, body.get("peer").and_then(Value::as_str).unwrap_or(""), t))?;
                Response::empty()?.with_status(204)
            }
            "/leave" => {
                deleted = api(room.leave(&value(&body)?, t))?;
                Response::empty()?.with_status(204)
            }
            "/ice" => {
                let auth: Auth = value(&body)?;
                api(room.authorize(&auth, t))?;
                if api(room.encoded())? != before {
                    self.save(&room)?;
                }
                if let Some((created, ice)) = room.members.get(&auth.peer).and_then(|m| m.ice.as_ref())
                    && t.saturating_sub(*created) < 3_600_000
                {
                    return Response::from_json(ice);
                }
                let ice = self.generate_ice().await?;
                // Provider fetch yields: reload and reauthorize so leave/kick cannot be undone.
                let mut latest = self.load()?.ok_or_else(|| Error::RustError("404:Room unavailable".into()))?;
                api(latest.authorize(&auth, now()))?;
                if let Some(m) = latest.members.get_mut(&auth.peer) {
                    m.ice = Some((now(), ice.clone()));
                }
                self.save(&latest)?;
                return Response::from_json(&ice);
            }
            _ => return Response::error("Route unavailable", 404),
        };
        if deleted {
            // Remove membership synchronously; the existing alarm cleans storage later.
            // Avoid an await between clearing state and canceling an alarm that
            // could belong to a newly created room after a collision retry.
            self.state.storage().sql().exec("DELETE FROM room WHERE id=1", None)?;
        } else if api(room.encoded())? != before {
            self.save(&room)?;
        }
        Ok(response)
    }
    async fn generate_ice(&self) -> Result<Value> {
        let key = self.env.secret("TURN_KEY_ID")?.to_string();
        let token = self.env.secret("TURN_API_TOKEN")?.to_string();
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::RustError("503:TURN service not configured".into()));
        }
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        headers.set("Authorization", &format!("Bearer {token}"))?;
        headers.set("User-Agent", "PhotoCraft-dev")?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post).with_headers(headers).with_body(Some(wasm_bindgen::JsValue::from_str("{\"ttl\":86400}")));
        let mut response =
            Fetch::Request(Request::new_with_init(&format!("https://rtc.live.cloudflare.com/v1/turn/keys/{key}/credentials/generate-ice-servers"), &init)?)
                .send()
                .await?;
        if !(200..300).contains(&response.status_code()) {
            return Err(Error::RustError("502:TURN provider rejected credentials".into()));
        }
        let mut bytes = Vec::new();
        let mut stream = response.stream()?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.len() > 65536usize.saturating_sub(bytes.len()) {
                return Err(Error::RustError("502:TURN response too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&bytes)?;
        crate::normalize_ice(v).map_err(|m| Error::RustError(format!("502:{m}")))
    }
}
impl DurableObject for PhotoCraftRoom {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }
    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.handle(req).await {
            Ok(r) => Ok(r),
            Err(e) => error_response(e),
        }
    }
    async fn alarm(&self) -> Result<Response> {
        if self.load()?.is_some_and(|r| r.active(now())) {
            self.state.storage().set_alarm(std::time::Duration::from_secs(60)).await?;
        } else {
            self.state.storage().delete_all().await?;
        }
        Response::empty()
    }
}
