use anyhow::{bail, Result};
use serde_json::Value;

use super::run_json;

/// List Cloud Map service names visible to the profile.
pub async fn list_services(profile: &str, region: &str) -> Result<Vec<String>> {
    let json = run_json(profile, region, &["servicediscovery", "list-services"]).await?;
    let names = json
        .get("Services")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.get("Name").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(names)
}

/// Resolve a Cloud Map service name to a concrete `(ip, port)` of a healthy
/// registered instance — mirrors the lookup the existing service-tunnel.sh does.
pub async fn resolve(profile: &str, region: &str, service_name: &str) -> Result<(String, u16)> {
    let services = run_json(profile, region, &["servicediscovery", "list-services"]).await?;
    let service_id = services
        .get("Services")
        .and_then(Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .find(|s| s.get("Name").and_then(Value::as_str) == Some(service_name))
        })
        .and_then(|s| s.get("Id").and_then(Value::as_str))
        .map(str::to_string);

    let Some(service_id) = service_id else {
        bail!("Cloud Map service '{service_name}' not found");
    };

    let instances = run_json(
        profile,
        region,
        &[
            "servicediscovery",
            "list-instances",
            "--service-id",
            &service_id,
        ],
    )
    .await?;

    let attrs = instances
        .get("Instances")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .and_then(|i| i.get("Attributes"));

    let Some(attrs) = attrs else {
        bail!("no healthy instances registered for '{service_name}'");
    };

    let ip = attrs
        .get("AWS_INSTANCE_IPV4")
        .and_then(Value::as_str)
        .map(str::to_string);
    let port = attrs
        .get("AWS_INSTANCE_PORT")
        .and_then(Value::as_str)
        .and_then(|p| p.parse::<u16>().ok());

    match (ip, port) {
        (Some(ip), Some(port)) => Ok((ip, port)),
        _ => bail!("instance for '{service_name}' missing IPv4/port attributes"),
    }
}
