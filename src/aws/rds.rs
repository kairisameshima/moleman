use anyhow::Result;
use serde_json::Value;

use super::run_json;

/// A discoverable RDS endpoint the user can choose to tunnel to.
#[derive(Debug, Clone)]
pub struct RdsInstance {
    pub identifier: String,
    pub endpoint: String,
    pub port: u16,
    pub engine: String,
}

/// Discover RDS/Aurora endpoints visible to the profile. Aurora clusters expose
/// a writer endpoint (preferred); standalone instances that aren't part of a
/// cluster contribute their own endpoint.
pub async fn list(profile: &str, region: &str) -> Result<Vec<RdsInstance>> {
    let mut out = Vec::new();

    // Aurora clusters → writer endpoint.
    if let Ok(clusters) = run_json(profile, region, &["rds", "describe-db-clusters"]).await {
        if let Some(arr) = clusters.get("DBClusters").and_then(Value::as_array) {
            for c in arr {
                let identifier = string_field(c, "DBClusterIdentifier");
                let endpoint = string_field(c, "Endpoint");
                if endpoint.is_empty() {
                    continue;
                }
                let port = c.get("Port").and_then(Value::as_u64).unwrap_or(5432) as u16;
                let engine = string_field(c, "Engine");
                out.push(RdsInstance {
                    identifier,
                    endpoint,
                    port,
                    engine,
                });
            }
        }
    }

    // Standalone instances not belonging to a cluster.
    let instances = run_json(profile, region, &["rds", "describe-db-instances"]).await?;
    if let Some(arr) = instances.get("DBInstances").and_then(Value::as_array) {
        for i in arr {
            if !string_field(i, "DBClusterIdentifier").is_empty() {
                continue; // covered by its cluster endpoint above
            }
            let addr = i
                .get("Endpoint")
                .map(|e| string_field(e, "Address"))
                .unwrap_or_default();
            if addr.is_empty() {
                continue;
            }
            let port = i
                .get("Endpoint")
                .and_then(|e| e.get("Port"))
                .and_then(Value::as_u64)
                .unwrap_or(5432) as u16;
            out.push(RdsInstance {
                identifier: string_field(i, "DBInstanceIdentifier"),
                endpoint: addr,
                port,
                engine: string_field(i, "Engine"),
            });
        }
    }

    out.sort_by(|a, b| a.identifier.cmp(&b.identifier));
    Ok(out)
}

fn string_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}
