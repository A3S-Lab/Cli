use serde::{Deserialize, Serialize};

use crate::error::{DevError, Result};

const BOX_BIN: &str = "a3s-box";

// ── Types ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BoxContainer {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Names", default)]
    pub name: String,
    #[serde(rename = "Image", default)]
    pub image: String,
    #[serde(rename = "Status", default)]
    pub status: String,
    #[serde(rename = "Created", default)]
    pub created: String,
    #[serde(rename = "Ports", default)]
    pub ports: String,
    #[serde(rename = "Command", default)]
    pub command: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct BoxImage {
    #[serde(rename = "Repository", default)]
    pub repository: String,
    #[serde(rename = "Tag", default)]
    pub tag: String,
    #[serde(rename = "Digest", default)]
    pub digest: String,
    #[serde(rename = "Size", default)]
    pub size: String,
    #[serde(rename = "Pulled", default)]
    pub pulled: String,
    #[serde(rename = "Reference", default)]
    pub reference: String,
}

#[derive(Debug, Serialize, Default)]
pub struct BoxNetwork {
    pub name: String,
    pub driver: String,
    pub subnet: String,
    pub gateway: String,
    pub isolation: String,
    pub endpoints: String,
}

#[derive(Debug, Serialize, Default)]
pub struct BoxVolume {
    pub driver: String,
    pub name: String,
    pub mount_point: String,
    pub in_use_by: String,
}

#[derive(Debug, Serialize, Default)]
pub struct BoxInfo {
    pub version: String,
    pub virtualization: String,
    pub home: String,
    pub boxes_total: u32,
    pub boxes_running: u32,
    pub images_cached: String,
}

// ── Queries ───────────────────────────────────────────────────────────────────

pub async fn list_containers(all: bool) -> Result<Vec<BoxContainer>> {
    let mut args = vec!["ps", "--format", "json"];
    if all {
        args.push("-a");
    }
    let out = run(&args).await?;
    if out.trim().is_empty() {
        return Ok(vec![]);
    }
    // Output is one JSON object per line
    let mut result = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(c) = serde_json::from_str::<BoxContainer>(line) {
            result.push(c);
        }
    }
    Ok(result)
}

pub async fn list_images() -> Result<Vec<BoxImage>> {
    let out = run(&["images", "--format", "json"]).await?;
    if out.trim().is_empty() {
        return Ok(vec![]);
    }
    let mut result = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(img) = serde_json::from_str::<BoxImage>(line) {
            result.push(img);
        }
    }
    Ok(result)
}

pub async fn list_networks() -> Result<Vec<BoxNetwork>> {
    let out = run(&["network", "ls"]).await?;
    Ok(parse_table(&out)
        .into_iter()
        .map(|cols| BoxNetwork {
            name: cols.first().cloned().unwrap_or_default(),
            driver: cols.get(1).cloned().unwrap_or_default(),
            subnet: cols.get(2).cloned().unwrap_or_default(),
            gateway: cols.get(3).cloned().unwrap_or_default(),
            isolation: cols.get(4).cloned().unwrap_or_default(),
            endpoints: cols.get(5).cloned().unwrap_or_default(),
        })
        .collect())
}

pub async fn list_volumes() -> Result<Vec<BoxVolume>> {
    let out = run(&["volume", "ls"]).await?;
    Ok(parse_table(&out)
        .into_iter()
        .map(|cols| BoxVolume {
            driver: cols.first().cloned().unwrap_or_default(),
            name: cols.get(1).cloned().unwrap_or_default(),
            mount_point: cols.get(2).cloned().unwrap_or_default(),
            in_use_by: cols.get(3).cloned().unwrap_or_default(),
        })
        .collect())
}

pub async fn get_info() -> Result<BoxInfo> {
    let out = run(&["info"]).await?;
    let mut info = BoxInfo::default();
    for line in out.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("a3s-box version ") {
            info.version = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Virtualization: ") {
            info.virtualization = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Home directory: ") {
            info.home = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("Boxes: ") {
            // "1 total, 0 running"
            let parts: Vec<&str> = v.split(',').collect();
            if let Some(t) = parts.first() {
                info.boxes_total = t
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
            if let Some(r) = parts.get(1) {
                info.boxes_running = r
                    .split_whitespace()
                    .next()
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0);
            }
        } else if let Some(v) = line.strip_prefix("Images: ") {
            info.images_cached = v.trim().to_string();
        }
    }
    Ok(info)
}

pub async fn container_logs(id: &str, tail: usize) -> Result<String> {
    let tail_s = tail.to_string();
    let out = run(&["logs", id, "--tail", &tail_s]).await?;
    Ok(out)
}

pub async fn stop_container(id: &str) -> Result<()> {
    run(&["stop", id]).await?;
    Ok(())
}

pub async fn remove_container(id: &str) -> Result<()> {
    run(&["rm", "-f", id]).await?;
    Ok(())
}

pub async fn remove_image(reference: &str) -> Result<()> {
    run(&["rmi", reference]).await?;
    Ok(())
}

pub async fn remove_network(name: &str) -> Result<()> {
    run(&["network", "rm", name]).await?;
    Ok(())
}

pub async fn remove_volume(name: &str) -> Result<()> {
    run(&["volume", "rm", name]).await?;
    Ok(())
}

pub async fn pull_image(reference: &str) -> Result<()> {
    run(&["pull", reference]).await?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

async fn run(args: &[&str]) -> Result<String> {
    let out = tokio::process::Command::new(BOX_BIN)
        .args(args)
        .output()
        .await
        .map_err(|e| DevError::Config(format!("failed to run a3s-box: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() && stdout.trim().is_empty() {
        return Err(DevError::Config(format!("a3s-box error: {stderr}")));
    }
    Ok(stdout)
}

/// Parse a fixed-width table (skip header row, split on 2+ spaces).
fn parse_table(text: &str) -> Vec<Vec<String>> {
    text.lines()
        .skip(1) // skip header
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            // Split on 2+ consecutive spaces to handle column values with single spaces
            let re_split: Vec<&str> = line.trim().splitn(20, "  ").collect();
            re_split
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .collect()
}
