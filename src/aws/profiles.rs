use std::collections::BTreeMap;

use anyhow::{Context, Result};
use configparser::ini::Ini;

use crate::config;

/// One `[profile X]` (or `[default]`) entry from `~/.aws/config`. Some fields
/// model the file faithfully but aren't all surfaced in the UI yet.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub sso_session: Option<String>,
    #[allow(dead_code)]
    pub account_id: Option<String>,
    pub role: Option<String>,
    #[allow(dead_code)]
    pub region: Option<String>,
}

/// One `[sso-session Y]` entry. Matched to a token cache file by name (sha1), so
/// the URL/region here are kept for completeness rather than for lookup.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SsoSession {
    pub name: String,
    pub start_url: Option<String>,
    pub region: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AwsConfig {
    pub profiles: Vec<Profile>,
    pub sessions: BTreeMap<String, SsoSession>,
}

/// Parse `~/.aws/config` into profiles and sso-sessions. Case-sensitive so
/// profile names like `AWSPowerUserAccess-123` survive verbatim for `--profile`.
pub fn load() -> Result<AwsConfig> {
    let path = config::aws_config_path();
    if !path.exists() {
        return Ok(AwsConfig::default());
    }

    let mut ini = Ini::new_cs();
    ini.load(&path)
        .map_err(|e| anyhow::anyhow!("parsing {}: {}", path.display(), e))
        .context("reading AWS config")?;

    let mut cfg = AwsConfig::default();

    for section in ini.sections() {
        let get = |key: &str| ini.get(&section, key);

        if section == "default" {
            cfg.profiles.push(Profile {
                name: "default".to_string(),
                sso_session: get("sso_session"),
                account_id: get("sso_account_id"),
                role: get("sso_role_name"),
                region: get("region"),
            });
        } else if let Some(name) = section.strip_prefix("profile ") {
            cfg.profiles.push(Profile {
                name: name.to_string(),
                sso_session: get("sso_session"),
                account_id: get("sso_account_id"),
                role: get("sso_role_name"),
                region: get("region"),
            });
        } else if let Some(name) = section.strip_prefix("sso-session ") {
            cfg.sessions.insert(
                name.to_string(),
                SsoSession {
                    name: name.to_string(),
                    start_url: get("sso_start_url"),
                    region: get("sso_region"),
                },
            );
        }
    }

    Ok(cfg)
}
