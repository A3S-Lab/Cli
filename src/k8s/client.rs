use crate::error::{DevError, Result};
use crate::log::LogAggregator;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

/// Kubernetes client - wraps kubectl commands.
#[derive(Debug, Clone)]
pub struct K8sClient {
    pub context: Option<String>,
    pub namespace: String,
}

impl K8sClient {
    pub fn new(context: Option<String>, namespace: String) -> Self {
        Self { context, namespace }
    }

    /// Check if kubectl is available on PATH.
    pub async fn check_available() -> Result<bool> {
        let output = Command::new("kubectl")
            .arg("version")
            .arg("--client")
            .arg("--output=json")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        Ok(output.map(|s| s.success()).unwrap_or(false))
    }

    /// Apply a YAML manifest to the cluster.
    pub async fn apply_manifest(&self, yaml: &str) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("apply").arg("-f").arg("-");

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);

        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| DevError::Config(format!("failed to spawn kubectl: {}", e)))?;

        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin
                .write_all(yaml.as_bytes())
                .await
                .map_err(|e| DevError::Config(format!("failed to write manifest: {}", e)))?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl apply failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevError::Config(format!(
                "kubectl apply failed: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Delete a Kubernetes resource.
    pub async fn delete_resource(&self, kind: &str, name: &str) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("delete").arg(kind).arg(name);

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);
        cmd.arg("--ignore-not-found=true");

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl delete failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!("kubectl delete {} {} failed: {}", kind, name, stderr);
        }

        Ok(())
    }

    /// Get pod status by label selector.
    #[allow(dead_code)]
    pub async fn get_pod_status(&self, label: &str) -> Result<PodStatus> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("get")
            .arg("pods")
            .arg("-l")
            .arg(label)
            .arg("--output=jsonpath={.items[0].status.phase}");

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl get pods failed: {}", e)))?;

        if !output.status.success() {
            return Ok(PodStatus::NotFound);
        }

        let phase = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(match phase.as_str() {
            "Running" => PodStatus::Running,
            "Pending" => PodStatus::Pending,
            "Succeeded" => PodStatus::Succeeded,
            "Failed" => PodStatus::Failed,
            _ => PodStatus::Unknown,
        })
    }

    /// Wait for pod to be ready (with timeout).
    pub async fn wait_for_ready(&self, label: &str, timeout_secs: u64) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("wait")
            .arg("pods")
            .arg("-l")
            .arg(label)
            .arg("--for=condition=Ready")
            .arg(format!("--timeout={}s", timeout_secs));

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl wait failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevError::Config(format!("pod not ready: {}", stderr)));
        }

        Ok(())
    }

    /// Stream logs from a pod (returns the first pod matching the label).
    #[allow(dead_code)]
    pub async fn get_logs(&self, label: &str, tail: usize) -> Result<String> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("logs")
            .arg("-l")
            .arg(label)
            .arg(format!("--tail={}", tail));

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl logs failed: {}", e)))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Rollout restart a deployment.
    pub async fn rollout_restart(&self, deployment: &str) -> Result<()> {
        let mut cmd = Command::new("kubectl");
        cmd.arg("rollout")
            .arg("restart")
            .arg("deployment")
            .arg(deployment);

        if let Some(ref ctx) = self.context {
            cmd.arg("--context").arg(ctx);
        }
        cmd.arg("--namespace").arg(&self.namespace);

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl rollout restart failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevError::Config(format!(
                "rollout restart failed: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Build a Docker image from a Dockerfile.
    /// Streams build output to logs in real-time.
    pub async fn build_image(
        &self,
        image: &str,
        dockerfile: &Path,
        context: &Path,
        build_args: &std::collections::HashMap<String, String>,
        service_name: &str,
        log: Option<&std::sync::Arc<LogAggregator>>,
    ) -> Result<()> {
        tracing::info!("[{}] building image: {}", service_name, image);

        let mut cmd = Command::new("docker");
        cmd.arg("build")
            .arg("-t")
            .arg(image)
            .arg("-f")
            .arg(dockerfile)
            .current_dir(context);

        // Add build args
        for (key, value) in build_args {
            cmd.arg("--build-arg").arg(format!("{}={}", key, value));
        }

        cmd.arg(".");

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| DevError::Config(format!("failed to spawn docker build: {}", e)))?;

        // Stream stdout
        if let Some(stdout) = child.stdout.take() {
            let log_clone = log.cloned();
            let service_name = service_name.to_string();
            tokio::spawn(async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(ref log) = log_clone {
                        log.push(&service_name, &line, 0);
                    } else {
                        println!("[{}] {}", service_name, line);
                    }
                }
            });
        }

        // Stream stderr
        if let Some(stderr) = child.stderr.take() {
            let log_clone = log.cloned();
            let service_name = service_name.to_string();
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(ref log) = log_clone {
                        log.push(&service_name, &line, 0);
                    } else {
                        eprintln!("[{}] {}", service_name, line);
                    }
                }
            });
        }

        let status = child
            .wait()
            .await
            .map_err(|e| DevError::Config(format!("docker build failed: {}", e)))?;

        if !status.success() {
            return Err(DevError::Config(format!(
                "docker build failed with exit code: {:?}",
                status.code()
            )));
        }

        tracing::info!("[{}] image built successfully: {}", service_name, image);
        Ok(())
    }

    /// Tag and push an image to a registry.    /// Returns the registry-qualified image name.
    pub async fn push_image(
        &self,
        image: &str,
        registry: &str,
        service_name: &str,
        log: Option<&std::sync::Arc<LogAggregator>>,
    ) -> Result<String> {
        let tagged = format!("{}/{}", registry.trim_end_matches('/'), image);

        // docker tag <image> <tagged>
        let tag_status = tokio::process::Command::new("docker")
            .arg("tag")
            .arg(image)
            .arg(&tagged)
            .status()
            .await
            .map_err(|e| DevError::Config(format!("docker tag failed: {}", e)))?;

        if !tag_status.success() {
            return Err(DevError::Config(format!(
                "docker tag {} {} failed",
                image, tagged
            )));
        }

        // docker push <tagged> — stream output
        let mut cmd = tokio::process::Command::new("docker");
        cmd.arg("push")
            .arg(&tagged)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| DevError::Config(format!("docker push failed: {}", e)))?;

        if let Some(stdout) = child.stdout.take() {
            let log_clone = log.cloned();
            let svc = service_name.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(ref l) = log_clone {
                        l.push(&svc, &line, 0);
                    } else {
                        println!("[{}] {}", svc, line);
                    }
                }
            });
        }
        if let Some(stderr) = child.stderr.take() {
            let log_clone = log.cloned();
            let svc = service_name.to_string();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(ref l) = log_clone {
                        l.push(&svc, &line, 0);
                    } else {
                        eprintln!("[{}] {}", svc, line);
                    }
                }
            });
        }

        let status = child
            .wait()
            .await
            .map_err(|e| DevError::Config(format!("docker push failed: {}", e)))?;

        if !status.success() {
            return Err(DevError::Config(format!("docker push {} failed", tagged)));
        }

        tracing::info!("[{}] pushed: {}", service_name, tagged);
        Ok(tagged)
    }

    /// Generate manifests from a Helm chart using `helm template`.
    pub async fn helm_template(
        &self,
        release_name: &str,
        chart_path: &Path,
        values_file: Option<&Path>,
    ) -> Result<String> {
        let mut cmd = tokio::process::Command::new("helm");
        cmd.arg("template")
            .arg(release_name)
            .arg(chart_path)
            .arg("--namespace")
            .arg(&self.namespace);

        if let Some(values) = values_file {
            cmd.arg("--values").arg(values);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("helm template failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevError::Config(format!(
                "helm template failed: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Generate manifests from a Kustomize directory using `kubectl kustomize`.
    pub async fn kustomize_build(&self, kustomize_dir: &Path) -> Result<String> {
        let mut cmd = tokio::process::Command::new("kubectl");
        cmd.arg("kustomize").arg(kustomize_dir);

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl kustomize failed: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DevError::Config(format!(
                "kubectl kustomize failed: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Check if helm is available.
    pub async fn check_helm_available() -> Result<()> {
        let output = tokio::process::Command::new("helm")
            .arg("version")
            .output()
            .await
            .map_err(|_| DevError::Config("helm not found in PATH".into()))?;

        if !output.status.success() {
            return Err(DevError::Config("helm version check failed".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum PodStatus {
    Running,
    Pending,
    Succeeded,
    Failed,
    Unknown,
    NotFound,
}
