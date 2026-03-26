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
    /// Named environment overrides. `a3s up --env <name>` merges the matching
    /// block's per-service env on top of the base service env.
    #[serde(default)]
    pub env_override: IndexMap<String, EnvOverride>,
}

#[derive(Debug, Deserialize)]
pub struct GlobalSettings {
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Runtime mode: "local" (default) or "k8s"
    #[serde(default = "default_runtime")]
    pub runtime: String,
    /// Kubernetes context (only used when runtime = "k8s")
    #[serde(default)]
    pub k8s_context: Option<String>,
    /// Kubernetes namespace (only used when runtime = "k8s")
    #[serde(default = "default_k8s_namespace")]
    pub k8s_namespace: String,
    /// Local registry to push images to after build (e.g. "localhost:5000").
    /// When set, images are tagged and pushed before deploying.
    #[serde(default)]
    pub registry: Option<String>,
    /// Enable HTTPS for the reverse proxy (generates self-signed certificate)
    #[serde(default)]
    pub https: bool,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            proxy_port: default_proxy_port(),
            log_level: default_log_level(),
            runtime: default_runtime(),
            k8s_context: None,
            k8s_namespace: default_k8s_namespace(),
            registry: None,
            https: false,
        }
    }
}

fn default_proxy_port() -> u16 {
    7080
}
fn default_log_level() -> String {
    "info".into()
}
fn default_runtime() -> String {
    "box".into()
}
fn default_k8s_namespace() -> String {
    "default".into()
}

#[derive(Debug, Deserialize, Clone)]
pub struct EnvOverride {
    /// Per-service env overrides. Only `env` is supported; other fields are ignored.
    #[serde(default)]
    pub service: IndexMap<String, ServiceEnvOverride>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServiceEnvOverride {
    #[serde(default)]
    pub env: HashMap<String, String>,
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
    /// Rotate `log_file` when it reaches this size (in MB). 0 = no rotation (default).
    #[serde(default)]
    pub log_rotate_mb: u64,
    /// Shell command to run (in the service's working directory) before starting the service.
    /// A non-zero exit code aborts startup.
    #[serde(default)]
    pub pre_start: Option<String>,
    /// Shell command to run (in the service's working directory) after the service stops.
    #[serde(default)]
    pub post_stop: Option<String>,
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
    /// Labels for grouping and filtering services (e.g., ["backend", "critical"]).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Kubernetes-specific configuration (only used when runtime = "k8s").
    #[serde(default)]
    pub k8s: Option<K8sConfig>,
    /// Box-specific configuration (only used when runtime = "box").
    #[serde(default)]
    pub r#box: Option<BoxConfig>,
}

/// Kubernetes-specific configuration for a service.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct K8sConfig {
    /// Container image (e.g., "node:20-alpine", "myapp:latest").
    pub image: String,
    /// Path to Dockerfile for building the image (optional).
    #[serde(default)]
    pub dockerfile: Option<PathBuf>,
    /// Build arguments for docker build (optional).
    #[serde(default)]
    pub build_args: HashMap<String, String>,
    /// Number of replicas (default: 1).
    #[serde(default = "default_replicas")]
    pub replicas: u32,
    /// Resource requests and limits (optional).
    #[serde(default)]
    pub resources: Option<K8sResources>,
    /// Path to Helm chart directory (optional, e.g., "./charts/myapp").
    /// If set, uses `helm template` instead of generating manifests.
    #[serde(default)]
    pub helm_chart: Option<PathBuf>,
    /// Path to Helm values file (optional, e.g., "./values.yaml").
    #[serde(default)]
    pub helm_values: Option<PathBuf>,
    /// Path to Kustomize directory (optional, e.g., "./k8s/overlays/dev").
    /// If set, uses `kubectl kustomize` instead of generating manifests.
    #[serde(default)]
    pub kustomize_dir: Option<PathBuf>,
    /// Path to file containing secret key-value pairs (optional, e.g., ".env.secret").
    /// Secrets are stored in a Kubernetes Secret and injected as environment variables.
    #[serde(default)]
    pub secret_file: Option<PathBuf>,
    /// Secret key-value pairs (optional). Used if secret_file is not set.
    #[serde(default)]
    pub secrets: HashMap<String, String>,
    /// Volume mounts (optional). Supports hostPath, emptyDir, configMap, secret.
    #[serde(default)]
    pub volumes: Vec<K8sVolume>,
}

/// Kubernetes volume configuration.
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct K8sVolume {
    /// Volume name (must be unique within the service).
    pub name: String,
    /// Volume type: "hostPath", "emptyDir", "configMap", "secret".
    #[serde(rename = "type")]
    pub volume_type: String,
    /// Mount path in the container (required).
    pub mount_path: String,
    /// Host path (required for hostPath type, relative to A3sfile.hcl directory).
    #[serde(default)]
    pub host_path: Option<PathBuf>,
    /// ConfigMap name (required for configMap type).
    #[serde(default)]
    pub config_map: Option<String>,
    /// Secret name (required for secret type).
    #[serde(default)]
    pub secret: Option<String>,
    /// Read-only mount (default: false).
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct K8sResources {
    #[serde(default)]
    pub cpu_request: Option<String>,
    #[serde(default)]
    pub cpu_limit: Option<String>,
    #[serde(default)]
    pub memory_request: Option<String>,
    #[serde(default)]
    pub memory_limit: Option<String>,
}

fn default_replicas() -> u32 {
    1
}

/// Box-specific configuration for a service (used when runtime = "box").
#[derive(Debug, Deserialize, Clone, PartialEq)]
pub struct BoxConfig {
    /// Container image to use (e.g., "python:3.12-slim", "node:20-alpine").
    /// If not set, an image is auto-detected from the service command.
    #[serde(default)]
    pub image: Option<String>,
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

/// Expand `env("VAR_NAME")` and `env("VAR_NAME", "default")` calls in HCL source text.
/// This runs before HCL parsing so the result is a plain string literal the parser can handle.
///
/// - `env("VAR")` → value of `VAR`, or empty string if unset
/// - `env("VAR", "default")` → value of `VAR`, or `"default"` if unset
///
/// The replacement is injected as a quoted HCL string literal so it fits anywhere a string
/// value is expected. Nested quotes in the value are escaped.
pub fn expand_env_func(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Look for `env(` — case-sensitive, matching Terraform/HCL convention.
        if bytes[i..].starts_with(b"env(") {
            i += 4; // skip `env(`
                    // Skip optional whitespace
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            // Expect opening quote for var name
            let quote = bytes[i];
            if quote == b'"' || quote == b'\'' {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                let var_name = &src[start..i];
                i += 1; // closing quote

                // Skip whitespace
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }

                // Optional default argument: , "default_value"
                let default_val = if i < bytes.len() && bytes[i] == b',' {
                    i += 1; // skip ','
                    while i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                    let dq = bytes[i];
                    if dq == b'"' || dq == b'\'' {
                        i += 1;
                        let ds = i;
                        while i < bytes.len() && bytes[i] != dq {
                            i += 1;
                        }
                        let d = src[ds..i].to_string();
                        i += 1; // closing quote
                        d
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                };

                // Skip whitespace then closing ')'
                while i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b')' {
                    i += 1;
                }

                let value = std::env::var(var_name).unwrap_or(default_val);
                // Emit as a quoted HCL string literal with inner quotes escaped.
                out.push('"');
                out.push_str(&value.replace('\\', "\\\\").replace('"', "\\\""));
                out.push('"');
                continue;
            } else {
                // Not a valid env() call — emit as-is and back up.
                out.push_str("env(");
                continue;
            }
        }
        out.push(src[i..].chars().next().unwrap());
        i += src[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
    }
    out
}

/// Replace `${VAR}` placeholders in `s` with OS environment variable values.
/// Unknown variables are left as-is (the `${VAR}` literal is preserved).
pub fn interpolate_env_vars(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let var_name: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if let Ok(val) = std::env::var(&var_name) {
                result.push_str(&val);
            } else {
                result.push_str(&format!("${{{var_name}}}"));
            }
        } else {
            result.push(ch);
        }
    }
    result
}

/// Replace `${name.port}` placeholders in `s` with the runtime-assigned port for that service.
/// Any placeholder that doesn't match a known service name is left unchanged.
/// This is called at service-start time, after OS-env interpolation has already run.
pub fn interpolate_service_ports(s: &str, ports: &HashMap<String, u16>) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let expr: String = chars.by_ref().take_while(|&c| c != '}').collect();
            if let Some(svc_name) = expr.strip_suffix(".port") {
                if let Some(&port) = ports.get(svc_name) {
                    result.push_str(&port.to_string());
                    continue;
                }
            }
            // Not a recognised service-port reference — leave as-is.
            result.push_str(&format!("${{{expr}}}"));
        } else {
            result.push(ch);
        }
    }
    result
}

/// Apply `${name.port}` interpolation to `cmd`, `env`, and hook fields of a `ServiceDef`.
/// `ports` maps service names to their runtime-assigned ports.
pub fn resolve_service_ports(mut svc: ServiceDef, ports: &HashMap<String, u16>) -> ServiceDef {
    svc.cmd = interpolate_service_ports(&svc.cmd, ports);
    for v in svc.env.values_mut() {
        *v = interpolate_service_ports(v, ports);
    }
    if let Some(h) = svc.pre_start.take() {
        svc.pre_start = Some(interpolate_service_ports(&h, ports));
    }
    if let Some(h) = svc.post_stop.take() {
        svc.post_stop = Some(interpolate_service_ports(&h, ports));
    }
    svc
}

/// Parse a `.env`-style file and return a map of key → value.
/// Lines starting with `#` and blank lines are ignored.
fn parse_env_file(contents: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            map.insert(key, val);
        }
    }
    map
}

impl DevConfig {
    pub fn from_file(path: &std::path::Path) -> Result<Self> {
        Self::from_file_with_env(path, None)
    }

    /// Load config, optionally applying a named env_override block on top.
    pub fn from_file_with_env(path: &std::path::Path, env_name: Option<&str>) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| DevError::Config(format!("cannot read {}: {e}", path.display())))?;
        // Expand env("VAR") calls before HCL parsing.
        let src = expand_env_func(&raw);
        let mut cfg: DevConfig = hcl::from_str(&src)
            .map_err(|e| DevError::Config(format!("parse error in {}: {e}", path.display())))?;
        let base_dir = path.parent().unwrap_or(std::path::Path::new("."));
        cfg.resolve_env_files(base_dir)?;
        cfg.apply_global_dotenv(base_dir);
        cfg.apply_interpolation();
        if let Some(name) = env_name {
            cfg.apply_env_override(name)?;
        }
        cfg.validate()?;
        Ok(cfg)
    }

    /// Merge env variables from a named `env_override` block into matching services.
    /// Override values take precedence over the base service env.
    fn apply_env_override(&mut self, name: &str) -> Result<()> {
        let overrides = self.env_override.get(name).cloned().ok_or_else(|| {
            DevError::Config(format!(
                "env_override '{name}' not found in A3sfile.hcl — available: {}",
                self.env_override
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        for (svc_name, svc_override) in &overrides.service {
            if let Some(svc) = self.service.get_mut(svc_name) {
                for (k, v) in &svc_override.env {
                    svc.env.insert(k.clone(), v.clone());
                }
            }
        }
        Ok(())
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
            for (k, v) in parse_env_file(&contents) {
                svc.env.entry(k).or_insert(v);
            }
        }
        Ok(())
    }

    /// Load a project-level `.env` from the A3sfile.hcl directory and apply it as the
    /// lowest-priority env source for every service (below `env` and `env_file`).
    fn apply_global_dotenv(&mut self, base_dir: &std::path::Path) {
        let path = base_dir.join(".env");
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return,
        };
        let global = parse_env_file(&contents);
        for svc in self.service.values_mut() {
            for (k, v) in &global {
                svc.env.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    /// Interpolate `${VAR}` placeholders in `cmd` and `env` values using OS environment variables.
    fn apply_interpolation(&mut self) {
        for svc in self.service.values_mut() {
            svc.cmd = interpolate_env_vars(&svc.cmd);
            for v in svc.env.values_mut() {
                *v = interpolate_env_vars(v);
            }
            if let Some(ref h) = svc.pre_start.clone() {
                svc.pre_start = Some(interpolate_env_vars(h));
            }
            if let Some(ref h) = svc.post_stop.clone() {
                svc.post_stop = Some(interpolate_env_vars(h));
            }
        }
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
            log_rotate_mb: 0,
            pre_start: None,
            post_stop: None,
            depends_on: depends_on.into_iter().map(|s| s.to_string()).collect(),
            watch: None,
            health: None,
            restart: Default::default(),
            stop_timeout: std::time::Duration::from_secs(5),
            disabled: false,
            labels: vec![],
            k8s: None,
            r#box: None,
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
            env_override: Default::default(),
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
        let cfg = make_config(vec![("a", make_svc(3000, vec!["b"])), ("b", svc_b)]);
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

    // ── Global .env auto-discovery ─────────────────────────────────────────────

    #[test]
    fn test_global_dotenv_applied_to_all_services() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "GLOBAL_VAR=hello\n").unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            r#"
service "a" { cmd = "echo" }
service "b" { cmd = "echo" }
"#,
        )
        .unwrap();
        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        assert_eq!(
            cfg.service["a"].env.get("GLOBAL_VAR").map(|s| s.as_str()),
            Some("hello")
        );
        assert_eq!(
            cfg.service["b"].env.get("GLOBAL_VAR").map(|s| s.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn test_service_env_overrides_global_dotenv() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "FOO=global\n").unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            "service \"a\" {\n  cmd = \"echo\"\n  env = { FOO = \"local\" }\n}\n",
        )
        .unwrap();
        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        assert_eq!(
            cfg.service["a"].env.get("FOO").map(|s| s.as_str()),
            Some("local")
        );
    }

    #[test]
    fn test_no_global_dotenv_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(&hcl_path, "service \"a\" { cmd = \"echo\" }\n").unwrap();
        assert!(DevConfig::from_file(&hcl_path).is_ok());
    }

    // ── Env var interpolation ─────────────────────────────────────────────────

    #[test]
    fn test_interpolate_known_var() {
        std::env::set_var("_A3S_TEST_INTERP", "world");
        let result = interpolate_env_vars("hello ${_A3S_TEST_INTERP}");
        assert_eq!(result, "hello world");
        std::env::remove_var("_A3S_TEST_INTERP");
    }

    #[test]
    fn test_interpolate_unknown_var_preserved() {
        let result = interpolate_env_vars("${_A3S_DEFINITELY_NOT_SET_XYZ}");
        assert_eq!(result, "${_A3S_DEFINITELY_NOT_SET_XYZ}");
    }

    #[test]
    fn test_interpolate_no_placeholders() {
        assert_eq!(interpolate_env_vars("plain string"), "plain string");
    }

    #[test]
    fn test_interpolate_multiple_vars() {
        std::env::set_var("_A3S_X", "foo");
        std::env::set_var("_A3S_Y", "bar");
        let result = interpolate_env_vars("${_A3S_X}-${_A3S_Y}");
        assert_eq!(result, "foo-bar");
        std::env::remove_var("_A3S_X");
        std::env::remove_var("_A3S_Y");
    }

    #[test]
    fn test_interpolation_in_cmd_and_env_via_from_file() {
        std::env::set_var("_A3S_HOST", "db.local");
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            r#"
service "api" {
  cmd = "echo ${_A3S_HOST}"
  env = { DB_HOST = "${_A3S_HOST}" }
}
"#,
        )
        .unwrap();
        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        assert_eq!(cfg.service["api"].cmd, "echo db.local");
        assert_eq!(
            cfg.service["api"].env.get("DB_HOST").map(|s| s.as_str()),
            Some("db.local")
        );
        std::env::remove_var("_A3S_HOST");
    }

    // ── pre_start / post_stop hooks ───────────────────────────────────────────

    #[test]
    fn test_pre_start_post_stop_parsed() {
        let src = r#"
service "api" {
  cmd       = "echo"
  pre_start = "echo starting"
  post_stop = "echo stopped"
}
"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(
            cfg.service["api"].pre_start.as_deref(),
            Some("echo starting")
        );
        assert_eq!(
            cfg.service["api"].post_stop.as_deref(),
            Some("echo stopped")
        );
    }

    #[test]
    fn test_pre_start_default_none() {
        let src = r#"service "api" { cmd = "echo" }"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert!(cfg.service["api"].pre_start.is_none());
        assert!(cfg.service["api"].post_stop.is_none());
    }

    // ── Inter-service port interpolation ─────────────────────────────────────

    #[test]
    fn test_interpolate_service_ports_known() {
        let mut ports = HashMap::new();
        ports.insert("db".to_string(), 5432u16);
        let result = interpolate_service_ports("postgres://localhost:${db.port}/dev", &ports);
        assert_eq!(result, "postgres://localhost:5432/dev");
    }

    #[test]
    fn test_interpolate_service_ports_unknown_preserved() {
        let ports = HashMap::new();
        let result = interpolate_service_ports("${missing.port}", &ports);
        assert_eq!(result, "${missing.port}");
    }

    #[test]
    fn test_interpolate_service_ports_non_port_field_preserved() {
        let ports = HashMap::new();
        let result = interpolate_service_ports("${db.host}", &ports);
        assert_eq!(result, "${db.host}");
    }

    #[test]
    fn test_resolve_service_ports_cmd_and_env() {
        let mut ports = HashMap::new();
        ports.insert("db".to_string(), 5432u16);
        let mut svc = make_svc(3000, vec![]);
        svc.cmd = "myapp --db ${db.port}".into();
        svc.env.insert(
            "DB_URL".into(),
            "postgres://localhost:${db.port}/app".into(),
        );
        let resolved = resolve_service_ports(svc, &ports);
        assert_eq!(resolved.cmd, "myapp --db 5432");
        assert_eq!(resolved.env["DB_URL"], "postgres://localhost:5432/app");
    }

    #[test]
    fn test_log_rotate_mb_default_zero() {
        let src = r#"service "api" { cmd = "echo" }"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(cfg.service["api"].log_rotate_mb, 0);
    }

    #[test]
    fn test_log_rotate_mb_parsed() {
        let src = r#"service "api" { cmd = "echo"\n  log_rotate_mb = 50 }"#;
        // Use hcl file roundtrip
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            "service \"api\" {\n  cmd = \"echo\"\n  log_rotate_mb = 50\n}\n",
        )
        .unwrap();
        let cfg = DevConfig::from_file(&hcl_path).unwrap();
        assert_eq!(cfg.service["api"].log_rotate_mb, 50);
        let _ = src;
    }

    // ── Labels ─────────────────────────────────────────────────────────────

    #[test]
    fn test_labels_parsed() {
        let src = r#"
service "api" {
  cmd = "echo"
  labels = ["backend", "critical"]
}
"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert_eq!(cfg.service["api"].labels, vec!["backend", "critical"]);
    }

    #[test]
    fn test_labels_default_empty() {
        let src = r#"service "api" { cmd = "echo" }"#;
        let cfg: DevConfig = hcl::from_str(src).unwrap();
        assert!(cfg.service["api"].labels.is_empty());
    }

    // ── env() function expansion ───────────────────────────────────────────

    #[test]
    fn test_expand_env_func_known_var() {
        std::env::set_var("_A3S_EF_TEST", "hello");
        let result = expand_env_func(r#"cmd = env("_A3S_EF_TEST")"#);
        assert_eq!(result, r#"cmd = "hello""#);
        std::env::remove_var("_A3S_EF_TEST");
    }

    #[test]
    fn test_expand_env_func_unknown_var_empty() {
        let result = expand_env_func(r#"cmd = env("_A3S_EF_DEFINITELY_NOT_SET_XYZ")"#);
        assert_eq!(result, r#"cmd = """#);
    }

    #[test]
    fn test_expand_env_func_default_used_when_unset() {
        let result = expand_env_func(r#"cmd = env("_A3S_EF_UNSET_XYZ", "fallback")"#);
        assert_eq!(result, r#"cmd = "fallback""#);
    }

    #[test]
    fn test_expand_env_func_default_ignored_when_set() {
        std::env::set_var("_A3S_EF_SET", "real");
        let result = expand_env_func(r#"cmd = env("_A3S_EF_SET", "fallback")"#);
        assert_eq!(result, r#"cmd = "real""#);
        std::env::remove_var("_A3S_EF_SET");
    }

    #[test]
    fn test_expand_env_func_no_calls_passthrough() {
        let src = r#"service "api" { cmd = "echo" }"#;
        assert_eq!(expand_env_func(src), src);
    }

    #[test]
    fn test_expand_env_func_in_hcl_roundtrip() {
        std::env::set_var("_A3S_EF_CMD", "node server.js");
        let src = r#"service "api" { cmd = env("_A3S_EF_CMD") }"#;
        let expanded = expand_env_func(src);
        let cfg: DevConfig = hcl::from_str(&expanded).unwrap();
        assert_eq!(cfg.service["api"].cmd, "node server.js");
        std::env::remove_var("_A3S_EF_CMD");
    }

    #[test]
    fn test_expand_env_func_escapes_inner_quotes() {
        std::env::set_var("_A3S_EF_QUOTE", r#"say "hi""#);
        let result = expand_env_func(r#"x = env("_A3S_EF_QUOTE")"#);
        assert_eq!(result, r#"x = "say \"hi\"""#);
        std::env::remove_var("_A3S_EF_QUOTE");
    }

    // ── env_override ──────────────────────────────────────────────────────

    #[test]
    fn test_env_override_applied() {
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            r#"
service "api" {
  cmd = "echo"
  env = { DB_URL = "localhost" }
}

env_override "staging" {
  service "api" {
    env = { DB_URL = "staging-db" }
  }
}
"#,
        )
        .unwrap();
        let cfg = DevConfig::from_file_with_env(&hcl_path, Some("staging")).unwrap();
        assert_eq!(
            cfg.service["api"].env.get("DB_URL").map(|s| s.as_str()),
            Some("staging-db")
        );
    }

    #[test]
    fn test_env_override_unknown_name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(&hcl_path, r#"service "api" { cmd = "echo" }"#).unwrap();
        assert!(DevConfig::from_file_with_env(&hcl_path, Some("nonexistent")).is_err());
    }

    #[test]
    fn test_env_override_none_leaves_base_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let hcl_path = dir.path().join("A3sfile.hcl");
        std::fs::write(
            &hcl_path,
            "service \"api\" {\n  cmd = \"echo\"\n  env = { DB_URL = \"localhost\" }\n}\n",
        )
        .unwrap();
        let cfg = DevConfig::from_file_with_env(&hcl_path, None).unwrap();
        assert_eq!(
            cfg.service["api"].env.get("DB_URL").map(|s| s.as_str()),
            Some("localhost")
        );
    }
}
