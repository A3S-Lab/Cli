use std::sync::Arc;
use tokio::sync::RwLock;
use indexmap::IndexMap;
use crate::config::ServiceDef;
use crate::error::Result;
use crate::log::LogAggregator;
use super::client::{K8sClient, PodStatus};
use super::manifest::ManifestGenerator;

/// Kubernetes runtime - manages services as Kubernetes resources.
pub struct K8sRuntime {
    client: K8sClient,
    log: Arc<LogAggregator>,
    /// Optional local registry to push images to after build.
    registry: Option<String>,
    manifests: Arc<RwLock<IndexMap<String, Vec<String>>>>,
}

impl K8sRuntime {
    pub fn new(client: K8sClient, log: Arc<LogAggregator>, registry: Option<String>) -> Self {
        Self {
            client,
            log,
            registry,
            manifests: Arc::new(RwLock::new(IndexMap::new())),
        }
    }

    /// Build image and optionally push to registry. Returns the final image name to use.
    async fn build_and_push(&self, name: &str, svc: &ServiceDef, config_dir: &std::path::Path) -> Result<Option<String>> {
        let Some(ref k8s_cfg) = svc.k8s else { return Ok(None); };
        let Some(ref dockerfile) = k8s_cfg.dockerfile else { return Ok(None); };

        let dockerfile = if dockerfile.is_absolute() {
            dockerfile.clone()
        } else {
            config_dir.join(dockerfile)
        };
        let context = dockerfile.parent().unwrap_or(config_dir);

        self.client.build_image(
            &k8s_cfg.image,
            &dockerfile,
            context,
            &k8s_cfg.build_args,
            name,
            Some(&self.log),
        ).await?;

        // Push to registry if configured
        if let Some(ref registry) = self.registry {
            let tagged = self.client.push_image(
                &k8s_cfg.image,
                registry,
                name,
                Some(&self.log),
            ).await?;
            return Ok(Some(tagged));
        }

        Ok(Some(k8s_cfg.image.clone()))
    }

    /// Build image if dockerfile is configured, then deploy to Kubernetes.
    pub async fn start_service(&self, name: &str, svc: &ServiceDef, config_dir: &std::path::Path) -> Result<()> {
        tracing::info!("[{}] deploying to kubernetes", name);

        self.build_and_push(name, svc, config_dir).await?;

        let namespace = &self.client.namespace;
        let mut manifests = vec![];

        // Check if Helm or Kustomize is configured
        if let Some(ref k8s_cfg) = svc.k8s {
            if let Some(ref helm_chart) = k8s_cfg.helm_chart {
                // Use Helm template
                tracing::info!("[{}] generating manifests from Helm chart: {}", name, helm_chart.display());
                let chart_path = config_dir.join(helm_chart);
                let values_path = k8s_cfg.helm_values.as_ref().map(|v| config_dir.join(v));

                let manifest_yaml = self.client.helm_template(
                    name,
                    &chart_path,
                    values_path.as_deref(),
                ).await?;

                self.client.apply_manifest(&manifest_yaml).await?;
                manifests.push(manifest_yaml);
                self.manifests.write().await.insert(name.to_string(), manifests);

                let label = format!("app={}", name);
                tracing::info!("[{}] waiting for pod to be ready...", name);
                self.client.wait_for_ready(&label, 60).await?;
                tracing::info!("[{}] deployed successfully (via Helm)", name);
                return Ok(());
            }

            if let Some(ref kustomize_dir) = k8s_cfg.kustomize_dir {
                // Use Kustomize
                tracing::info!("[{}] generating manifests from Kustomize: {}", name, kustomize_dir.display());
                let kustomize_path = config_dir.join(kustomize_dir);

                let manifest_yaml = self.client.kustomize_build(&kustomize_path).await?;

                self.client.apply_manifest(&manifest_yaml).await?;
                manifests.push(manifest_yaml);
                self.manifests.write().await.insert(name.to_string(), manifests);

                let label = format!("app={}", name);
                tracing::info!("[{}] waiting for pod to be ready...", name);
                self.client.wait_for_ready(&label, 60).await?;
                tracing::info!("[{}] deployed successfully (via Kustomize)", name);
                return Ok(());
            }
        }

        // Default: generate manifests from A3sfile.hcl
        // Load secrets (from secret_file or inline secrets map)
        let secrets = self.load_secrets(svc, config_dir).await?;
        if let Some(secret_manifest) = ManifestGenerator::generate_secret(name, &secrets, namespace) {
            self.client.apply_manifest(&secret_manifest).await?;
            manifests.push(secret_manifest);
        }

        if let Some(configmap) = ManifestGenerator::generate_configmap(name, svc, namespace) {
            self.client.apply_manifest(&configmap).await?;
            manifests.push(configmap);
        }

        let deployment = ManifestGenerator::generate_deployment(name, svc, namespace, config_dir);
        self.client.apply_manifest(&deployment).await?;
        manifests.push(deployment);

        let service = ManifestGenerator::generate_service(name, svc, namespace);
        self.client.apply_manifest(&service).await?;
        manifests.push(service);

        self.manifests.write().await.insert(name.to_string(), manifests);

        let label = format!("app={}", name);
        tracing::info!("[{}] waiting for pod to be ready...", name);
        self.client.wait_for_ready(&label, 60).await?;

        tracing::info!("[{}] deployed successfully", name);
        Ok(())
    }

    /// Load secrets from secret_file or inline secrets map.
    async fn load_secrets(&self, svc: &ServiceDef, config_dir: &std::path::Path) -> Result<std::collections::HashMap<String, String>> {
        let k8s_cfg = match &svc.k8s {
            Some(k) => k,
            None => return Ok(std::collections::HashMap::new()),
        };

        // Start with inline secrets
        let mut secrets = k8s_cfg.secrets.clone();

        // Load from secret_file if specified (overrides inline)
        if let Some(ref secret_file) = k8s_cfg.secret_file {
            let path = if secret_file.is_absolute() {
                secret_file.clone()
            } else {
                config_dir.join(secret_file)
            };

            let content = tokio::fs::read_to_string(&path).await
                .map_err(|e| crate::error::DevError::Config(format!(
                    "failed to read secret_file {}: {}", path.display(), e
                )))?;

            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    secrets.insert(key.trim().to_string(), value.trim().to_string());
                }
            }
        }

        Ok(secrets)
    }

    /// Rebuild image (if dockerfile configured) then rollout restart.
    pub async fn rebuild_and_restart(&self, name: &str, svc: &ServiceDef, config_dir: &std::path::Path) -> Result<()> {
        tracing::info!("[{}] rebuilding image and restarting", name);
        self.build_and_push(name, svc, config_dir).await?;
        self.client.rollout_restart(name).await?;
        tracing::info!("[{}] rollout restart triggered", name);
        Ok(())
    }

    /// Stop a service by deleting its Kubernetes resources.
    pub async fn stop_service(&self, name: &str) -> Result<()> {
        tracing::info!("[{}] deleting from kubernetes", name);
        self.client.delete_resource("service", name).await?;
        self.client.delete_resource("deployment", name).await?;
        self.client.delete_resource("configmap", &format!("{}-config", name)).await?;
        self.client.delete_resource("secret", &format!("{}-secret", name)).await?;
        self.manifests.write().await.shift_remove(name);
        tracing::info!("[{}] deleted successfully", name);
        Ok(())
    }

    /// Get the status of a service's pod.
    #[allow(dead_code)]
    pub async fn get_status(&self, name: &str) -> Result<PodStatus> {
        let label = format!("app={}", name);
        self.client.get_pod_status(&label).await
    }

    /// Get logs from a service's pod.
    #[allow(dead_code)]
    pub async fn get_logs(&self, name: &str, tail: usize) -> Result<String> {
        let label = format!("app={}", name);
        self.client.get_logs(&label, tail).await
    }

    /// Deploy Ingress for all services with subdomains.
    pub async fn deploy_ingress(&self, services: &IndexMap<String, ServiceDef>) -> Result<()> {
        if let Some(ingress) = ManifestGenerator::generate_ingress(services, &self.client.namespace) {
            tracing::info!("deploying ingress");
            self.client.apply_manifest(&ingress).await?;
            tracing::info!("ingress deployed");
        }
        Ok(())
    }
}
