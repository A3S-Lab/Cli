use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use indexmap::IndexMap;
use serde::Deserialize;

use crate::error::{DevError, Result};

#[derive(Debug, Deserialize)]
pub struct DevConfig {
    #[serde(default)]
    pub dev: GlobalSettings,
    #[serde(default)]
    pub service: IndexMap<String, ServiceDef>,
}

#[derive(Debug, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            proxy_port: default_proxy_port(),
            log_level: default_log_level(),
        }
    }
}

fn default_proxy_port() -> u16 {
    7080
}
fn default_log_level() -> String {
    "info".into()
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct ServiceDef {
    pub cmd: String,
    #[serde(default)]
    pub dir: Option<PathBuf>,
    /// Port to bind. 0 = auto-assign a free port (portless-style).
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub subdomain: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Path to a .env file to load. Relative to the A3sfile.hcl directory.
    /// Variables in `env` take precedence over env_file.
    #[serde(default)]
    pub env_file: Option<PathBuf>,
    /// Write stdout/stderr to this file (append mode). Relative to the A3sfile.hcl directory.
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub watch: Option<WatchConfig>,
    #[serde(default)]
    pub health: Option<HealthConfig>,
    #[serde(default)]
    pub restart: RestartConfig,
    /// How long to wait for SIGTERM before sending SIGKILL (default: 5s).
    #[serde(default = "default_stop_timeout", with = "duration_serde")]
    pub stop_timeout: Duration,
    /// If true, this service is skipped entirely (not started, not validated for deps).
    #[serde(default)]
    pub disabled: bool,
}

/// Crash-recovery restart policy for a service.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct RestartConfig {
    /// Maximum number of restarts before giving up (default: 10).
    #[serde(default = "default_max_restarts")]
    pub max_restarts: u32,
    /// Initial backoff delay between restarts (default: 1s).
    #[serde(default = "default_backoff", with = "duration_serde")]
    pub backoff: Duration,
    /// Maximum backoff delay (default: 30s).
    #[serde(default = "default_max_backoff", with = "duration_serde")]
    pub max_backoff: Duration,
    /// What to do when the service fails: "restart" (default) or "stop".
    #[serde(default)]
    pub on_failure: OnFailure,
}

impl Default for RestartConfig {
    fn default() -> Self {
        Self {
            max_restarts: default_max_restarts(),
            backoff: default_backoff(),
            max_backoff: default_max_backoff(),
            on_failure: OnFailure::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum OnFailure {
    /// Restart the service with exponential backoff (default).
    #[default]
    Restart,
    /// Leave the service stopped after it fails.
    Stop,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct WatchConfig {
    pub paths: Vec<PathBuf>,
    #[serde(default)]
    pub ignore: Vec<String>,
    #[serde(default = "default_true")]
    pub restart: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct HealthConfig {
    #[serde(rename = "type")]
    pub kind: HealthKind,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default = "default_interval", with = "duration_serde")]
    pub interval: Duration,
    #[serde(default = "default_timeout", with = "duration_serde")]
    pub timeout: Duration,
    #[serde(default = "default_retries")]
    pub retries: u32,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HealthKind {
    Http,
    Tcp,
}

fn default_interval() -> Duration {
    Duration::from_secs(2)
}
fn default_timeout() -> Duration {
    Duration::from_secs(1)
}
fn default_retries() -> u32 {
    3
}
fn default_max_restarts() -> u32 {
    10
}
fn default_backoff() -> Duration {
    Duration::from_secs(1)
}
fn default_max_backoff() -> Duration {
    Duration::from_secs(30)
}
fn default_stop_timeout() -> Duration {
    Duration::from_secs(5)
}

mod duration_serde {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let s = String::deserialize(d)?;
        parse_duration(&s).map_err(serde::de::Error::custom)
    }

    fn parse_duration(s: &str) -> Result<Duration, String> {
        if let Some(v) = s.strip_suffix("ms") {
            return v
                .trim()
                .parse::<u64>()
                .map(Duration::from_millis)
                .map_err(|e| e.to_string());
        }
        if let Some(v) = s.strip_suffix('s') {
            return v
                .trim()
                .parse::<u64>()
                .map(Duration::from_secs)
                .map_err(|e| e.to_string());
        }
        Err(format!(
            "unknown duration format: '{s}' (use '2s' or '500ms')"
        ))
    }
}

impl DevConfig {
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        let src = std::fs::read_to_string(path)
            .map_err(|e| DevError::Config(format!("cannot read {}: {e}", path.display())))?;
        let mut cfg: DevConfig = hcl::from_str(&src)
            .map_err(|e| DevError::Config(format!("parse error in {}: {e}", path.display())))?;
        let base_dir = path.parent().unwrap_or(std::path::Path::new("."));
        cfg.resolve_env_files(base_dir)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// For each service with an `env_file`, parse the file and merge its variables.
    /// Variables already present in `env` take precedence (env_file provides defaults).
    fn resolve_env_files(&mut self, base_dir: &std::path::Path) -> Result<()> {
        for (name, svc) in &mut self.service {
            let Some(ref env_file) = svc.env_file else {
                continue;
            };
            let path = if env_file.is_absolute() {
                env_file.clone()
            } else {
                base_dir.join(env_file)
            };
            let contents = std::fs::read_to_string(&path).map_err(|e| {
                DevError::Config(format!(
                    "service '{name}': cannot read env_file {}: {e}",
                    path.display()
                ))
            })?;
            for line in contents.lines() {
                let line = line.trim();
                // Skip blank lines and comments
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    let key = k.trim().to_string();
                    let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
                    // env takes precedence over env_file
                    svc.env.entry(key).or_insert(val);
                }
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        // Port conflict check — skip port 0 (auto-assigned at runtime) and disabled services
        let mut seen: HashMap<u16, &str> = HashMap::new();
        for (name, svc) in &self.service {
            if svc.disabled || svc.port == 0 {
                continue;
            }
            if let Some(other) = seen.insert(svc.port, name.as_str()) {
                return Err(DevError::PortConflict {
                    a: other.to_string(),
                    b: name.clone(),
                    port: svc.port,
                });
            }
        }
        // Unknown depends_on references — skip disabled services
        for (name, svc) in &self.service {
            if svc.disabled {
                continue;
            }
            for dep in &svc.depends_on {
                let dep_svc = self.service.get(dep);
                if dep_svc.is_none() || dep_svc.is_some_and(|d| d.disabled) {
                    return Err(DevError::Config(format!(
                        "service '{name}' depends_on unknown or disabled service '{dep}'"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_svc(port: u16, depends_on: Vec<&str>) -> ServiceDef {
        ServiceDef {
            cmd: "echo ok".into(),
            dir: None,
            port,
            subdomain: None,
            env: Default::default(),
            env_file: None,
            log_file: None,
            depends_on: depends_on.into_iter().map(|s| s.to_string()).collect(),
            watch: None,
            health: None,
            restart: Default::default(),
            stop_timeout: std::time::Duration::from_secs(5),
            disabled: false,
        }
    }

    fn make_config(services: Vec<(&str, ServiceDef)>) -> DevConfig {
        let mut map = IndexMap::new();
        for (name, svc) in services {
            map.insert(name.to_string(), svc);
        }
        DevConfig {
            dev: GlobalSettings::default(),
            service: map,
        }
    }

    #[test]
    fn test_validate_ok() {
        let cfg = make_config(vec![
            ("a", make_svc(3000, vec![])),
            ("b", make_svc(3001, vec!["a"])),
        ]);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_port_conflict() {
        let cfg = make_config(vec![
            ("a", make_svc(3000, vec![])),
            ("b", make_svc(3000, vec![])),
        ]);
        assert!(matches!(cfg.validate(), Err(DevError::PortConflict { .. })));
    }

    #[test]
    fn test_validate_port_zero_no_conflict() {
        // Two services with port=0 should not conflict
        let cfg = make_config(vec![("a", make_svc(0, vec![])), ("b", make_svc(0, vec![]))]);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_validate_unknown_depends_on() {
        let cfg = make_config(vec![("a", make_svc(3000, vec!["nonexistent"]))]);
        assert!(matches!(cfg.validate(), Err(DevError::Config(_))));
    }

    #[test]
    fn test_disabled_skips_port_conflict() {
        let mut svc_b = make_svc(3000, vec![]);
        svc_b.disabled = true;
        let cfg = make_config(vec![("a", make_svc(3000, vec![])), ("b", svc_b)]);
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn test_depends_on_disabled_service_is_error() {
        let mut svc_b = make_svc(3001, vec![]);
        svc_b.disabled = true;
        let cfg = make_config(vec![
            ("a", make_svc(3000, vec!["b"])),
            ("b", svc_b),
        ]);
        assert!(matches!(cfg.validate(), Err(DevError::Config(_))));
    }

    #[test]
    fn test_env_file_loaded() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        writeln!(
            std::fs::File::create(&env_path).unwrap(),
            "FOO=bar\n# comment\nBAZ=qux"
        )
        .unwrap();

        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            "service \"web\" {\n  cmd = \"echo ok\"\n  env_file = \".env\"\n}\n",
        )
        .unwrap();

        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        let svc = &cfg.service["web"];
        assert_eq!(svc.env.get("FOO").map(|s| s.as_str()), Some("bar"));
        assert_eq!(svc.env.get("BAZ").map(|s| s.as_str()), Some("qux"));
    }

    #[test]
    fn test_env_overrides_env_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env");
        writeln!(std::fs::File::create(&env_path).unwrap(), "FOO=from_file").unwrap();

        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            "service \"web\" {\n  cmd = \"echo ok\"\n  env_file = \".env\"\n  env = {\n    FOO = \"from_env\"\n  }\n}\n",
        )
        .unwrap();

        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        assert_eq!(
            cfg.service["web"].env.get("FOO").map(|s| s.as_str()),
            Some("from_env")
        );
    }

    #[test]
    fn test_parse_hcl() {
        let src = r#"
service "web" {
  cmd  = "node server.js"
  port = 3000
}
"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(cfg.service.len(), 1);
        assert_eq!(cfg.service["web"].port, 3000);
        assert_eq!(cfg.service["web"].cmd, "node server.js");
    }

    #[test]
    fn test_default_proxy_port() {
        let cfg: DevConfig = hcl::from_str("").unwrap();
        assert_eq!(cfg.dev.proxy_port, 7080);
    }

    #[test]
    fn test_stop_timeout_default() {
        let src = r#"service "api" { cmd = "echo" }"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(
            cfg.service["api"].stop_timeout,
            std::time::Duration::from_secs(5)
        );
    }

    #[test]
    fn test_stop_timeout_custom() {
        let src = r#"
service "api" {
  cmd          = "echo"
  stop_timeout = "10s"
}"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(
            cfg.service["api"].stop_timeout,
            std::time::Duration::from_secs(10)
        );
    }

    #[test]
    fn test_restart_config_defaults() {
        let src = r#"service "api" { cmd = "echo" }"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        let r = &cfg.service["api"].restart;
        assert_eq!(r.max_restarts, 10);
        assert_eq!(r.backoff, std::time::Duration::from_secs(1));
        assert_eq!(r.max_backoff, std::time::Duration::from_secs(30));
        assert_eq!(r.on_failure, OnFailure::Restart);
    }

    #[test]
    fn test_restart_config_custom() {
        let src = r#"
service "api" {
  cmd = "echo"
  restart {
    max_restarts = 3
    backoff      = "2s"
    max_backoff  = "60s"
    on_failure   = "stop"
  }
}"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        let r = &cfg.service["api"].restart;
        assert_eq!(r.max_restarts, 3);
        assert_eq!(r.backoff, std::time::Duration::from_secs(2));
        assert_eq!(r.max_backoff, std::time::Duration::from_secs(60));
        assert_eq!(r.on_failure, OnFailure::Stop);
    }

    #[test]
    fn test_service_def_partial_eq_same() {
        let a = make_svc(3000, vec![]);
        let b = make_svc(3000, vec![]);
        assert_eq!(a, b);
    }

    #[test]
    fn test_service_def_partial_eq_different_port() {
        let a = make_svc(3000, vec![]);
        let b = make_svc(3001, vec![]);
        assert_ne!(a, b);
    }
}
