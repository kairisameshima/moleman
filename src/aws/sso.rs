use chrono::{DateTime, Utc};
use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::config;

const EXPIRING_THRESHOLD_SECS: i64 = 15 * 60;

/// Validity of an SSO session's cached token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStatus {
    /// Valid with this many seconds remaining.
    Valid(i64),
    /// Valid but expiring soon (< 15 min), with seconds remaining.
    Expiring(i64),
    /// Token present but past its expiry.
    Expired,
    /// No cached token for this session — never logged in (or logged out).
    NoToken,
}

/// AWS CLI v2 keys the SSO token cache file by `sha1(session_name)`. We read that
/// exact file rather than matching on start URL, because several sso-sessions
/// here share the same start URL and would otherwise be indistinguishable.
pub fn scan_session(session_name: &str) -> TokenStatus {
    let mut hasher = Sha1::new();
    hasher.update(session_name.as_bytes());
    let digest = hasher.finalize();
    let filename: String = digest.iter().map(|b| format!("{b:02x}")).collect();

    let path = config::sso_cache_dir().join(format!("{filename}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return TokenStatus::NoToken;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return TokenStatus::NoToken;
    };

    // A real token file has an accessToken; client-registration files do not.
    if json.get("accessToken").is_none() {
        return TokenStatus::NoToken;
    }
    let Some(expires_at) = json.get("expiresAt").and_then(Value::as_str) else {
        return TokenStatus::NoToken;
    };
    let Ok(expiry) = DateTime::parse_from_rfc3339(expires_at) else {
        return TokenStatus::NoToken;
    };

    let remaining = expiry.with_timezone(&Utc) - Utc::now();
    let secs = remaining.num_seconds();
    if secs <= 0 {
        TokenStatus::Expired
    } else if secs < EXPIRING_THRESHOLD_SECS {
        TokenStatus::Expiring(secs)
    } else {
        TokenStatus::Valid(secs)
    }
}

/// Spawn `aws sso login --sso-session <name>`, which opens a browser. The child
/// is returned so the caller can poll it for completion and then rescan.
pub fn login(session_name: &str) -> std::io::Result<tokio::process::Child> {
    let mut cmd = tokio::process::Command::new("aws");
    cmd.arg("sso")
        .arg("login")
        .arg("--sso-session")
        .arg(session_name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd.spawn()
}
