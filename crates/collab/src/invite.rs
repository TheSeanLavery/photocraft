//! Bounded room invitations. Links never carry passwords or silently change servers.
use crate::DEFAULT_SIGNALING_URL;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub code: String,
    pub signaling_url: String,
}

pub fn normalize_code(input: &str) -> Result<String, String> {
    let input = input.trim();
    if !(4..=12).contains(&input.len()) || !input.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err("Room codes contain 4–12 letters or numbers".into());
    }
    Ok(input.to_ascii_uppercase())
}

pub fn parse(input: &str) -> Result<Invite, String> {
    parse_with_server(input, DEFAULT_SIGNALING_URL)
}

/// The caller explicitly selects a server; pasted links must match that origin exactly.
pub fn parse_with_server(input: &str, server: &str) -> Result<Invite, String> {
    if input.len() > 2048 || server.len() > 1024 {
        return Err("Invite is too long".into());
    }
    let server = server.trim_end_matches('/');
    if !(server.starts_with("https://") || server.starts_with("http://127.0.0.1:") || server.starts_with("http://localhost:"))
        || server.bytes().any(|b| b.is_ascii_whitespace() || b < 32)
        || server.contains(['?', '#', '@'])
    {
        return Err("Choose a valid HTTPS room service or an explicit local test server".into());
    }
    let authority = server.split_once("://").map(|(_, authority)| authority).unwrap_or_default();
    let (host, port) = authority.split_once(':').map_or((authority, None), |(host, port)| (host, Some(port)));
    if host.is_empty()
        || !host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        || port.is_some_and(|port| port.parse::<u16>().map_or(true, |port| port == 0))
    {
        return Err("Room service must be a valid origin without a path".into());
    }
    let input = input.trim();
    let code = if let Some(code) = input.strip_prefix("photocraft://join/") {
        code
    } else if let Some(code) = input.strip_prefix(&format!("{server}/join/")) {
        code
    } else {
        input
    };
    Ok(Invite { code: normalize_code(code)?, signaling_url: server.to_owned() })
}

pub fn share_url(code: &str) -> Result<String, String> {
    share_url_with_server(code, DEFAULT_SIGNALING_URL)
}

pub fn share_url_with_server(code: &str, server: &str) -> Result<String, String> {
    let invite = parse_with_server(code, server)?;
    Ok(format!("{}/join/{}", invite.signaling_url, invite.code))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_and_both_links_round_trip() {
        for input in [" ab12cd ".to_owned(), "photocraft://join/ab12cd".to_owned(), share_url("ab12cd").unwrap()] {
            assert_eq!(parse(&input).unwrap().code, "AB12CD");
        }
    }
    #[test]
    fn rejects_credentials_host_switches_paths_and_hostile_sizes() {
        for input in [
            "ABC",
            "ABCD/",
            "photocraft://join/ABCD?password=secret",
            "photocraft://join/ABCD#x",
            "https://evil.example/join/ABCD",
            "https://user@photocraft-mp.theseanlavery.workers.dev/join/ABCD",
            "photocraft://join/%41BCD",
            "ＡＢＣＤ",
            "ABCD\nEFGH",
        ] {
            assert!(parse(input).is_err(), "{input}");
        }
        assert!(parse(&"A".repeat(2049)).is_err());
    }
    #[test]
    fn explicit_local_service_is_not_inferred_from_pasted_link() {
        let link = "http://127.0.0.1:5548/join/TEST01";
        assert!(parse(link).is_err());
        assert_eq!(parse_with_server(link, "http://127.0.0.1:5548").unwrap().code, "TEST01");
        assert!(parse_with_server("TEST01", "http://evil.example").is_err());
        for server in ["https://", "https://example.com/path", "https://example.com:bad", "http://127.0.0.1:0"] {
            assert!(parse_with_server("TEST01", server).is_err());
        }
    }
}
