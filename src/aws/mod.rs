pub mod discovery;
pub mod profiles;
pub mod rds;
pub mod sso;

use anyhow::{bail, Context, Result};
use serde_json::Value;

/// Run an `aws` CLI subcommand with `--region`, `--profile`, and `--output json`
/// appended, returning the parsed JSON. Errors carry the command's stderr so the
/// UI can surface a meaningful reason (expired SSO, missing permission, etc.).
pub async fn run_json(profile: &str, region: &str, args: &[&str]) -> Result<Value> {
    let mut cmd = tokio::process::Command::new("aws");
    cmd.args(args)
        .arg("--region")
        .arg(region)
        .arg("--profile")
        .arg(profile)
        .arg("--output")
        .arg("json");

    let out = cmd
        .output()
        .await
        .context("failed to run the `aws` CLI — is it installed and on PATH?")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("aws {}: {}", args.join(" "), first_meaningful_line(&stderr));
    }

    serde_json::from_slice(&out.stdout).context("aws returned output that was not valid JSON")
}

/// AWS errors are often multi-line; pick the most informative single line.
fn first_meaningful_line(stderr: &str) -> String {
    let trimmed = stderr.trim();
    trimmed
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(trimmed)
        .trim()
        .to_string()
}
