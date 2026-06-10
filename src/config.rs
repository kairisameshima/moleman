use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Template configuration written on first run. Placeholders only — fill in
/// your own bastions, services, and ssh tunnels at ~/.config/moleman/config.toml.
const DEFAULT_CONFIG: &str = r#"# moleman configuration
# Region used for all AWS calls and tunnels.
region = "us-east-1"

# Directory where ssh PEM keys live (gitignored, never committed). Relative
# paths sit next to this config file; bare filenames in `pem = ...` entries
# resolve against it. Absolute or ~/ paths are used as-is.
pem_dir = "pems"

[services]
# ECS/Cloud Map services are discovered live from Cloud Map; this profile +
# bastion are what the discovery and port-forward calls run against.
profile = "default"
bastion = "i-REPLACE_WITH_YOUR_BASTION"
# Local ports discovered services not listed below are assigned from here, upward.
auto_port_base = 8009
# If Cloud Map discovery returns nothing (e.g. expired SSO), these names are
# shown so the group is never empty.
fallback = []

# Conventional local port per known service, e.g.:
#   my-service = 8000
[services.ports]

[rds]
# Local ports for RDS tunnels are auto-assigned from here, upward.
local_port_base = 5433

# Bastion to tunnel through, keyed by the profile's sso-session name. RDS
# instances are discovered live per profile; the chosen one is tunneled through
# the bastion that matches that profile's account, e.g.:
#   dev = "i-REPLACE_WITH_YOUR_DEV_BASTION"
#   prod = "i-REPLACE_WITH_YOUR_PROD_BASTION"
[rds.bastions]

# Hosts reached over plain ssh -L (not an AWS resource type we can enumerate)
# are listed explicitly, e.g.:
#   [[temporal]]
#   name = "temporal-dev"
#   local_port = 8081
#   pem = "your-bastion-key.pem"   # resolved against pem_dir
#   elb_host = "internal-your-elb.us-east-1.elb.amazonaws.com"
#   remote_port = 80
#   ec2_host = "203.0.113.10"
#   ec2_user = "ec2-user"
"#;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub region: String,
    #[serde(default = "default_pem_dir")]
    pub pem_dir: String,
    /// Directory the config file was loaded from; anchors relative `pem_dir`.
    #[serde(skip)]
    pub config_dir: PathBuf,
    pub services: ServicesCfg,
    pub rds: RdsCfg,
    #[serde(default)]
    pub temporal: Vec<TemporalCfg>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServicesCfg {
    pub profile: String,
    pub bastion: String,
    #[serde(default = "default_service_port_base")]
    pub auto_port_base: u16,
    #[serde(default)]
    pub ports: BTreeMap<String, u16>,
    #[serde(default)]
    pub fallback: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RdsCfg {
    #[serde(default = "default_rds_port_base")]
    pub local_port_base: u16,
    #[serde(default)]
    pub bastions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TemporalCfg {
    pub name: String,
    pub local_port: u16,
    pub pem: String,
    pub elb_host: String,
    pub remote_port: u16,
    pub ec2_host: String,
    #[serde(default = "default_ec2_user")]
    pub ec2_user: String,
}

fn default_service_port_base() -> u16 {
    8009
}
fn default_rds_port_base() -> u16 {
    5433
}
fn default_ec2_user() -> String {
    "ec2-user".to_string()
}
fn default_pem_dir() -> String {
    "pems".to_string()
}

impl Config {
    /// Load the config, scaffolding a default one on first run. Returns the
    /// parsed config and the path it was read from.
    pub fn load() -> Result<(Config, PathBuf)> {
        let path = config_path();
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating config dir {}", parent.display()))?;
            }
            std::fs::write(&path, DEFAULT_CONFIG)
                .with_context(|| format!("writing default config to {}", path.display()))?;
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))?;
        cfg.config_dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => std::env::current_dir().context("resolving current dir for config")?,
        };
        Ok((cfg, path))
    }

    /// Resolve a `pem` config value to a path: bare filenames land in `pem_dir`
    /// (the untracked key folder); anything starting with `~` or `/` is a path
    /// of its own. `pem_dir` itself, when relative, sits next to the config
    /// file — so a repo-local `config.toml` gets a repo-local `pems/`.
    pub fn resolve_pem(&self, pem: &str) -> PathBuf {
        if pem.starts_with('~') || pem.starts_with('/') {
            expand_tilde(pem)
        } else {
            self.pem_dir_path().join(pem)
        }
    }

    /// `pem_dir` as an absolute path (relative values anchor at the config dir).
    pub fn pem_dir_path(&self) -> PathBuf {
        let dir = expand_tilde(&self.pem_dir);
        if dir.is_absolute() {
            dir
        } else {
            self.config_dir.join(dir)
        }
    }
}

/// Config file location: a `config.toml` in the current directory wins (the
/// repo-local, untracked setup); otherwise `~/.config/moleman/config.toml`,
/// honoring `XDG_CONFIG_HOME`.
pub fn config_path() -> PathBuf {
    let local = PathBuf::from("config.toml");
    if local.exists() {
        return local;
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    base.join("moleman").join("config.toml")
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Expand a leading `~/` to the user's home directory.
pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else if path == "~" {
        home_dir()
    } else {
        PathBuf::from(path)
    }
}

/// Path to the AWS SSO token cache directory.
pub fn sso_cache_dir() -> PathBuf {
    home_dir().join(".aws").join("sso").join("cache")
}

/// Path to the AWS shared config file (`~/.aws/config`), honoring `AWS_CONFIG_FILE`.
pub fn aws_config_path() -> PathBuf {
    std::env::var_os("AWS_CONFIG_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".aws").join("config"))
}

/// Render a path home-relative (`~/…`) for compact display.
pub fn display_path(path: &Path) -> String {
    let home = home_dir();
    if let Ok(rest) = path.strip_prefix(&home) {
        format!("~/{}", rest.display())
    } else {
        path.display().to_string()
    }
}
