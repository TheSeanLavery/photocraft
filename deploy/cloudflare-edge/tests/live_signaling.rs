//! Runtime regression for immutable Fetch response headers (native tests cannot model workerd).
//! Run after deploying: PHOTOCRAFT_SIGNALING_URL=https://your-edge cargo test --test live_signaling -- --ignored
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};
fn request(server: &str, path: &str, method: &str, body: &Value) -> (u16, Value, String) {
    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "20",
            "-i",
            "-X",
            method,
            "-H",
            "Content-Type: application/json",
            "-H",
            "Origin: http://127.0.0.1:5555",
            "--data-binary",
            "@-",
            "-w",
            "\n%{http_code}",
            &format!("{}{path}", server.trim_end_matches('/')),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("curl available");
    child.stdin.take().expect("request stdin").write_all(body.to_string().as_bytes()).expect("send test request");
    let output = child.wait_with_output().expect("request completes");
    assert!(output.status.success(), "curl failed");
    let text = String::from_utf8(output.stdout).expect("UTF-8 response");
    let (response, status) = text.rsplit_once('\n').expect("status suffix");
    let status = status.trim().parse().expect("HTTP status");
    let (headers, body) = response.split_once("\r\n\r\n").expect("HTTP headers");
    let body = if body.trim().is_empty() { Value::Null } else { serde_json::from_str(body).expect("JSON signaling response") };
    (status, body, headers.to_ascii_lowercase())
}
#[test]
#[ignore = "requires a deployed signaling service; creates and leaves an isolated test room"]
fn browser_preflight_and_authenticated_poll_preserve_upstream_response() {
    let server = std::env::var("PHOTOCRAFT_SIGNALING_URL").expect("set PHOTOCRAFT_SIGNALING_URL");
    let (status, _, headers) = request(&server, "/poll", "OPTIONS", &Value::Null);
    assert_eq!(status, 204);
    assert!(headers.contains("access-control-allow-origin: *"));
    let (status, admission, _) = request(&server, "/create", "POST", &json!({"code":null,"password":null}));
    assert_eq!(status, 200);
    let auth = json!({"code":admission["code"],"peer":admission["peer"],"token":admission["token"]});
    let (status, signals, headers) = request(&server, "/poll", "POST", &auth);
    let (leave, _, _) = request(&server, "/leave", "POST", &auth);
    assert_eq!(leave, 204);
    assert_eq!(status, 200);
    assert_eq!(signals, json!([]));
    assert!(headers.contains("access-control-allow-origin: *"));
    assert!(headers.contains("cache-control: no-store"));
}

#[test]
#[ignore = "requires the deployed service; creates an isolated room and temporary TURN credentials"]
fn cloud_room_admission_signaling_and_turn_credentials() {
    let server = std::env::var("PHOTOCRAFT_SIGNALING_URL").expect("set PHOTOCRAFT_SIGNALING_URL");
    let (status, host, _) = request(&server, "/create", "POST", &json!({"password":"synthetic-test"}));
    assert_eq!(status, 200);
    let auth = json!({"code":host["code"],"peer":host["peer"],"token":host["token"]});
    let (status, _, _) = request(&server, "/join", "POST", &json!({"code":host["code"],"password":"wrong"}));
    assert_eq!(status, 401);
    let (status, guest, _) = request(&server, "/join", "POST", &json!({"code":host["code"],"password":"synthetic-test"}));
    assert_eq!(status, 200);
    let guest_auth = json!({"code":guest["code"],"peer":guest["peer"],"token":guest["token"]});
    let forged = json!({"code":host["code"],"peer":host["peer"],"token":"forged"});
    let (status, _, _) = request(&server, "/ice", "POST", &forged);
    assert_eq!(status, 401);
    let (ice_status, ice, headers) = request(&server, "/ice", "POST", &auth);
    let signal = json!({"from":guest["peer"],"to":host["peer"],"sdp":"synthetic-offer","offer":true});
    let (sent, _, _) = request(&server, "/signal", "POST", &json!({"auth":guest_auth,"signal":signal}));
    let (polled, signals, _) = request(&server, "/poll", "POST", &auth);
    let (left, _, _) = request(&server, "/leave", "POST", &guest_auth);
    let (revoked, _, _) = request(&server, "/ice", "POST", &guest_auth);
    let (closed, _, _) = request(&server, "/leave", "POST", &auth);
    assert_eq!(closed, 204);
    assert_eq!(left, 204);
    assert_eq!(revoked, 401);
    assert_eq!(sent, 204);
    assert_eq!(polled, 200);
    assert_eq!(signals, json!([signal]));
    assert_eq!(ice_status, 200, "credential generation failed");
    assert!(headers.contains("cache-control: no-store"));
    assert!(ice["iceServers"].as_array().is_some_and(|s| !s.is_empty()));
    assert!(
        ice["iceServers"]
            .as_array()
            .is_some_and(|s| s.iter().any(|s| s["urls"].as_array().is_some_and(|u| u.iter().any(|u| u.as_str().is_some_and(|u| u.starts_with("turn:"))))))
    );
}
