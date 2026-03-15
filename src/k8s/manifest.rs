use crate::config::{ServiceDef, HealthConfig, HealthKind};
use std::collections::HashMap;
use indexmap::IndexMap;

/// Generates Kubernetes manifests from ServiceDef.
pub struct ManifestGenerator;

impl ManifestGenerator {
    /// Generate a Deployment manifest for a service.
    pub fn generate_deployment(name: &str, svc: &ServiceDef, namespace: &str, config_dir: &std::path::Path) -> String {
        let k8s_config = svc.k8s.as_ref();
        let image = k8s_config.map(|k| k.image.clone()).unwrap_or_else(|| "alpine:latest".to_string());
        let replicas = k8s_config.map(|k| k.replicas).unwrap_or(1);

        // Parse cmd into command and args
        let (command, args) = Self::parse_command(&svc.cmd);

        // Generate environment variables (ConfigMap refs)
        let env_vars = Self::generate_env_vars(name, svc);

        // Generate probes
        let probes = Self::generate_probes(&svc.health, svc.port);

        // Generate resource limits
        let resources = Self::generate_resources(k8s_config.and_then(|k| k.resources.as_ref()));

        // Generate init containers for dependencies
        let init_containers = Self::generate_init_containers(&svc.depends_on);

        // Generate volumes and volumeMounts
        let (volumes, volume_mounts) = Self::generate_volumes(k8s_config, config_dir);

        format!(r#"apiVersion: apps/v1
kind: Deployment
metadata:
  name: {name}
  namespace: {namespace}
  labels:
    app: {name}
    managed-by: a3s
spec:
  replicas: {replicas}
  selector:
    matchLabels:
      app: {name}
  template:
    metadata:
      labels:
        app: {name}
    spec:{init_containers}
      containers:
      - name: {name}
        image: {image}
        {command}
        {args}
        ports:
        - containerPort: {port}
          protocol: TCP
        {env_vars}
        {volume_mounts}
        {probes}
        {resources}
{volumes}
"#,
            name = name,
            namespace = namespace,
            replicas = replicas,
            image = image,
            command = command,
            args = args,
            port = if svc.port > 0 { svc.port } else { 8080 },
            env_vars = env_vars,
            volume_mounts = volume_mounts,
            probes = probes,
            resources = resources,
            init_containers = init_containers,
            volumes = volumes,
        )
    }

    /// Generate a Service manifest.
    pub fn generate_service(name: &str, svc: &ServiceDef, namespace: &str) -> String {
        let port = if svc.port > 0 { svc.port } else { 8080 };

        format!(r#"apiVersion: v1
kind: Service
metadata:
  name: {name}
  namespace: {namespace}
  labels:
    app: {name}
    managed-by: a3s
spec:
  selector:
    app: {name}
  ports:
  - port: {port}
    targetPort: {port}
    protocol: TCP
    name: http
  type: ClusterIP
"#,
            name = name,
            namespace = namespace,
            port = port,
        )
    }

    /// Generate a ConfigMap from environment variables.
    pub fn generate_configmap(name: &str, svc: &ServiceDef, namespace: &str) -> Option<String> {
        if svc.env.is_empty() {
            return None;
        }

        let data = svc.env.iter()
            .map(|(k, v)| format!("  {}: \"{}\"", k, v.replace("\"", "\\\"")))
            .collect::<Vec<_>>()
            .join("\n");

        Some(format!(r#"apiVersion: v1
kind: ConfigMap
metadata:
  name: {name}-config
  namespace: {namespace}
  labels:
    app: {name}
    managed-by: a3s
data:
{data}
"#,
            name = name,
            namespace = namespace,
            data = data,
        ))
    }

    /// Generate a Secret from secret key-value pairs.
    /// Secrets are base64-encoded in the manifest.
    pub fn generate_secret(name: &str, secrets: &HashMap<String, String>, namespace: &str) -> Option<String> {
        if secrets.is_empty() {
            return None;
        }

        let data = secrets.iter()
            .map(|(k, v)| {
                let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, v.as_bytes());
                format!("  {}: {}", k, encoded)
            })
            .collect::<Vec<_>>()
            .join("\n");

        Some(format!(r#"apiVersion: v1
kind: Secret
metadata:
  name: {name}-secret
  namespace: {namespace}
  labels:
    app: {name}
    managed-by: a3s
type: Opaque
data:
{data}
"#,
            name = name,
            namespace = namespace,
            data = data,
        ))
    }

    /// Generate an Ingress manifest for services with subdomains.
    pub fn generate_ingress(services: &IndexMap<String, ServiceDef>, namespace: &str) -> Option<String> {
        let rules: Vec<String> = services.iter()
            .filter_map(|(name, svc)| {
                svc.subdomain.as_ref().map(|subdomain| {
                    let port = if svc.port > 0 { svc.port } else { 8080 };
                    format!(r#"  - host: {subdomain}.localhost
    http:
      paths:
      - path: /
        pathType: Prefix
        backend:
          service:
            name: {name}
            port:
              number: {port}"#,
                        subdomain = subdomain,
                        name = name,
                        port = port,
                    )
                })
            })
            .collect();

        if rules.is_empty() {
            return None;
        }

        Some(format!(r#"apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: a3s-ingress
  namespace: {namespace}
  labels:
    managed-by: a3s
spec:
  rules:
{rules}
"#,
            namespace = namespace,
            rules = rules.join("\n"),
        ))
    }

    // Helper functions

    fn parse_command(cmd: &str) -> (String, String) {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.is_empty() {
            return ("".to_string(), "".to_string());
        }

        let command = format!("command: [\"{}\"]\n        ", parts[0]);
        let args = if parts.len() > 1 {
            let args_list = parts[1..].iter()
                .map(|a| format!("\"{}\"", a))
                .collect::<Vec<_>>()
                .join(", ");
            format!("args: [{}]\n        ", args_list)
        } else {
            "".to_string()
        };

        (command, args)
    }

    fn generate_env_vars(name: &str, svc: &ServiceDef) -> String {
        let has_configmap = !svc.env.is_empty();
        let has_secret = svc.k8s.as_ref()
            .map(|k| k.secret_file.is_some() || !k.secrets.is_empty())
            .unwrap_or(false);

        if !has_configmap && !has_secret {
            return "".to_string();
        }

        let mut result = String::from("envFrom:\n");

        if has_configmap {
            result.push_str(&format!("        - configMapRef:\n            name: {}-config\n", name));
        }

        if has_secret {
            result.push_str(&format!("        - secretRef:\n            name: {}-secret\n", name));
        }

        result.push_str("        ");
        result
    }

    fn generate_probes(health: &Option<HealthConfig>, port: u16) -> String {
        let Some(health) = health else {
            return "".to_string();
        };

        let port = if port > 0 { port } else { 8080 };
        let interval = health.interval.as_secs();
        let timeout = health.timeout.as_secs();
        let retries = health.retries;

        match health.kind {
            HealthKind::Http => {
                let path = health.path.as_deref().unwrap_or("/health");
                format!(r#"livenessProbe:
          httpGet:
            path: {path}
            port: {port}
          initialDelaySeconds: 10
          periodSeconds: {interval}
          timeoutSeconds: {timeout}
          failureThreshold: {retries}
        readinessProbe:
          httpGet:
            path: {path}
            port: {port}
          initialDelaySeconds: 5
          periodSeconds: {interval}
          timeoutSeconds: {timeout}
          failureThreshold: {retries}
        "#,
                    path = path,
                    port = port,
                    interval = interval,
                    timeout = timeout,
                    retries = retries,
                )
            }
            HealthKind::Tcp => {
                format!(r#"livenessProbe:
          tcpSocket:
            port: {port}
          initialDelaySeconds: 10
          periodSeconds: {interval}
          timeoutSeconds: {timeout}
          failureThreshold: {retries}
        readinessProbe:
          tcpSocket:
            port: {port}
          initialDelaySeconds: 5
          periodSeconds: {interval}
        "#,
                    port = port,
                    interval = interval,
                    timeout = timeout,
                    retries = retries,
                )
            }
        }
    }

    fn generate_resources(resources: Option<&crate::config::K8sResources>) -> String {
        let Some(res) = resources else {
            return "".to_string();
        };

        let mut requests = vec![];
        let mut limits = vec![];

        if let Some(ref cpu) = res.cpu_request {
            requests.push(format!("          cpu: {}", cpu));
        }
        if let Some(ref mem) = res.memory_request {
            requests.push(format!("          memory: {}", mem));
        }
        if let Some(ref cpu) = res.cpu_limit {
            limits.push(format!("          cpu: {}", cpu));
        }
        if let Some(ref mem) = res.memory_limit {
            limits.push(format!("          memory: {}", mem));
        }

        if requests.is_empty() && limits.is_empty() {
            return "".to_string();
        }

        let mut result = String::from("resources:\n");
        if !requests.is_empty() {
            result.push_str("        requests:\n");
            result.push_str(&requests.join("\n"));
            result.push('\n');
        }
        if !limits.is_empty() {
            result.push_str("        limits:\n");
            result.push_str(&limits.join("\n"));
            result.push('\n');
        }
        result.push_str("        ");
        result
    }

    fn generate_init_containers(depends_on: &[String]) -> String {
        if depends_on.is_empty() {
            return "".to_string();
        }

        let containers = depends_on.iter()
            .map(|dep| format!(r#"      - name: wait-for-{dep}
        image: busybox:1.36
        command: ['sh', '-c', 'until nslookup {dep}; do echo waiting for {dep}; sleep 2; done']"#,
                dep = dep))
            .collect::<Vec<_>>()
            .join("\n");

        format!("\n      initContainers:\n{}\n", containers)
    }

    fn generate_volumes(k8s_config: Option<&crate::config::K8sConfig>, config_dir: &std::path::Path) -> (String, String) {
        let Some(k8s_cfg) = k8s_config else {
            return ("".to_string(), "".to_string());
        };

        if k8s_cfg.volumes.is_empty() {
            return ("".to_string(), "".to_string());
        }

        // Generate volumeMounts for container
        let mounts = k8s_cfg.volumes.iter()
            .map(|v| {
                let read_only = if v.read_only { "\n          readOnly: true" } else { "" };
                format!("        - name: {}\n          mountPath: {}{}", v.name, v.mount_path, read_only)
            })
            .collect::<Vec<_>>()
            .join("\n");

        let volume_mounts = if !mounts.is_empty() {
            format!("volumeMounts:\n{}\n        ", mounts)
        } else {
            "".to_string()
        };

        // Generate volumes for pod spec
        let vols = k8s_cfg.volumes.iter()
            .map(|v| {
                match v.volume_type.as_str() {
                    "hostPath" => {
                        let host_path = v.host_path.as_ref()
                            .map(|p| {
                                if p.is_absolute() {
                                    p.display().to_string()
                                } else {
                                    config_dir.join(p).display().to_string()
                                }
                            })
                            .unwrap_or_else(|| "/tmp".to_string());
                        format!("      - name: {}\n        hostPath:\n          path: {}\n          type: DirectoryOrCreate", v.name, host_path)
                    }
                    "emptyDir" => {
                        format!("      - name: {}\n        emptyDir: {{}}", v.name)
                    }
                    "configMap" => {
                        let cm_name = v.config_map.as_deref().unwrap_or("missing-configmap");
                        format!("      - name: {}\n        configMap:\n          name: {}", v.name, cm_name)
                    }
                    "secret" => {
                        let secret_name = v.secret.as_deref().unwrap_or("missing-secret");
                        format!("      - name: {}\n        secret:\n          secretName: {}", v.name, secret_name)
                    }
                    _ => {
                        format!("      - name: {}\n        emptyDir: {{}}  # unsupported type: {}", v.name, v.volume_type)
                    }
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let volumes = if !vols.is_empty() {
            format!("      volumes:\n{}\n", vols)
        } else {
            "".to_string()
        };

        (volumes, volume_mounts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ServiceDef, K8sConfig, K8sVolume, HealthConfig, HealthKind};
    use std::collections::HashMap;

    fn test_service() -> ServiceDef {
        ServiceDef {
            cmd: "npm start".to_string(),
            dir: None,
            port: 3000,
            subdomain: Some("api".to_string()),
            env: {
                let mut map = HashMap::new();
                map.insert("NODE_ENV".to_string(), "development".to_string());
                map
            },
            env_file: None,
            log_file: None,
            log_rotate_mb: 0,
            pre_start: None,
            post_stop: None,
            depends_on: vec![],
            watch: None,
            health: None,
            restart: Default::default(),
            stop_timeout: std::time::Duration::from_secs(5),
            disabled: false,
            labels: vec![],
            k8s: Some(K8sConfig {
                image: "node:20".to_string(),
                dockerfile: None,
                build_args: HashMap::new(),
                replicas: 2,
                resources: None,
                helm_chart: None,
                helm_values: None,
                kustomize_dir: None,
                secret_file: None,
                secrets: HashMap::new(),
                volumes: vec![],
            }),
        }
    }

    #[test]
    fn test_generate_deployment() {
        let svc = test_service();
        let config_dir = std::path::Path::new("/tmp");
        let manifest = ManifestGenerator::generate_deployment("api", &svc, "default", config_dir);

        assert!(manifest.contains("kind: Deployment"));
        assert!(manifest.contains("name: api"));
        assert!(manifest.contains("namespace: default"));
        assert!(manifest.contains("replicas: 2"));
        assert!(manifest.contains("image: node:20"));
        assert!(manifest.contains("containerPort: 3000"));
        assert!(manifest.contains("app: api"));
        assert!(manifest.contains("managed-by: a3s"));
    }

    #[test]
    fn test_generate_service() {
        let svc = test_service();
        let manifest = ManifestGenerator::generate_service("api", &svc, "default");

        assert!(manifest.contains("kind: Service"));
        assert!(manifest.contains("name: api"));
        assert!(manifest.contains("namespace: default"));
        assert!(manifest.contains("port: 3000"));
        assert!(manifest.contains("targetPort: 3000"));
        assert!(manifest.contains("type: ClusterIP"));
    }

    #[test]
    fn test_generate_configmap() {
        let svc = test_service();
        let manifest = ManifestGenerator::generate_configmap("api", &svc, "default");

        assert!(manifest.is_some());
        let manifest = manifest.unwrap();
        assert!(manifest.contains("kind: ConfigMap"));
        assert!(manifest.contains("name: api-config"));
        assert!(manifest.contains("NODE_ENV: \"development\""));
    }

    #[test]
    fn test_generate_configmap_empty() {
        let mut svc = test_service();
        svc.env.clear();
        let manifest = ManifestGenerator::generate_configmap("api", &svc, "default");

        assert!(manifest.is_none());
    }

    #[test]
    fn test_generate_secret() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), "secret123".to_string());
        secrets.insert("DB_PASSWORD".to_string(), "hunter2".to_string());

        let manifest = ManifestGenerator::generate_secret("api", &secrets, "default");

        assert!(manifest.is_some());
        let manifest = manifest.unwrap();
        assert!(manifest.contains("kind: Secret"));
        assert!(manifest.contains("name: api-secret"));
        assert!(manifest.contains("type: Opaque"));
        // Check base64 encoding
        assert!(manifest.contains("API_KEY:"));
        assert!(manifest.contains("DB_PASSWORD:"));
    }

    #[test]
    fn test_generate_volumes_hostpath() {
        let mut svc = test_service();
        if let Some(ref mut k8s) = svc.k8s {
            k8s.volumes = vec![
                K8sVolume {
                    name: "code".to_string(),
                    volume_type: "hostPath".to_string(),
                    mount_path: "/app/src".to_string(),
                    host_path: Some(std::path::PathBuf::from("./src")),
                    config_map: None,
                    secret: None,
                    read_only: false,
                }
            ];
        }

        let config_dir = std::path::Path::new("/tmp");
        let manifest = ManifestGenerator::generate_deployment("api", &svc, "default", config_dir);

        assert!(manifest.contains("volumeMounts:"));
        assert!(manifest.contains("- name: code"));
        assert!(manifest.contains("mountPath: /app/src"));
        assert!(manifest.contains("volumes:"));
        assert!(manifest.contains("hostPath:"));
        // Path should be resolved relative to config_dir
        assert!(manifest.contains("path:"));
        assert!(manifest.contains("/src"));
    }

    #[test]
    fn test_generate_volumes_emptydir() {
        let mut svc = test_service();
        if let Some(ref mut k8s) = svc.k8s {
            k8s.volumes = vec![
                K8sVolume {
                    name: "cache".to_string(),
                    volume_type: "emptyDir".to_string(),
                    mount_path: "/cache".to_string(),
                    host_path: None,
                    config_map: None,
                    secret: None,
                    read_only: false,
                }
            ];
        }

        let config_dir = std::path::Path::new("/tmp");
        let manifest = ManifestGenerator::generate_deployment("api", &svc, "default", config_dir);

        assert!(manifest.contains("- name: cache"));
        assert!(manifest.contains("mountPath: /cache"));
        assert!(manifest.contains("emptyDir: {}"));
    }

    #[test]
    fn test_generate_ingress() {
        let mut services = IndexMap::new();
        services.insert("api".to_string(), test_service());

        let manifest = ManifestGenerator::generate_ingress(&services, "default");

        assert!(manifest.is_some());
        let manifest = manifest.unwrap();
        assert!(manifest.contains("kind: Ingress"));
        assert!(manifest.contains("name: a3s-ingress"));
        assert!(manifest.contains("host: api.localhost"));
        assert!(manifest.contains("name: api"));
        assert!(manifest.contains("number: 3000"));
    }

    #[test]
    fn test_generate_ingress_no_subdomains() {
        let mut svc = test_service();
        svc.subdomain = None;
        let mut services = IndexMap::new();
        services.insert("api".to_string(), svc);

        let manifest = ManifestGenerator::generate_ingress(&services, "default");
        assert!(manifest.is_none());
    }

    #[test]
    fn test_generate_probes_http() {
        let mut svc = test_service();
        svc.health = Some(HealthConfig {
            kind: HealthKind::Http,
            path: Some("/health".to_string()),
            interval: std::time::Duration::from_secs(5),
            timeout: std::time::Duration::from_secs(2),
            retries: 3,
        });

        let config_dir = std::path::Path::new("/tmp");
        let manifest = ManifestGenerator::generate_deployment("api", &svc, "default", config_dir);

        assert!(manifest.contains("livenessProbe:"));
        assert!(manifest.contains("readinessProbe:"));
        assert!(manifest.contains("httpGet:"));
        assert!(manifest.contains("path: /health"));
        assert!(manifest.contains("port: 3000"));
        assert!(manifest.contains("periodSeconds: 5"));
        assert!(manifest.contains("timeoutSeconds: 2"));
        assert!(manifest.contains("failureThreshold: 3"));
    }

    #[test]
    fn test_generate_init_containers() {
        let mut svc = test_service();
        svc.depends_on = vec!["db".to_string(), "redis".to_string()];

        let config_dir = std::path::Path::new("/tmp");
        let manifest = ManifestGenerator::generate_deployment("api", &svc, "default", config_dir);

        assert!(manifest.contains("initContainers:"));
        assert!(manifest.contains("wait-for-db"));
        assert!(manifest.contains("wait-for-redis"));
        assert!(manifest.contains("nslookup db"));
        assert!(manifest.contains("nslookup redis"));
    }
}
