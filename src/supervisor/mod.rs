use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::process::Child;
use tokio::sync::{broadcast, RwLock};

use crate::config::{DevConfig, ServiceDef};
use crate::error::{DevError, Result};
use crate::graph::DependencyGraph;
use crate::health::HealthChecker;
use crate::ipc::StatusRow;
use crate::log::LogAggregator;
use crate::proxy::ProxyRouter;
use crate::state::ServiceState;
use crate::watcher::spawn_watcher;
use colored::Colorize;

use spawn::{free_port, spawn_process, SpawnSpec};

pub mod ipc;
mod spawn;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SupervisorEvent {
    StateChanged { service: String, state: String },
    HealthChange { service: String, healthy: bool },
}

struct ServiceHandle {
    child: Child,
    state: ServiceState,
    color_idx: usize,
    port: u16,
    /// Stops the file watcher OS thread for this service, if any.
    watcher_stop: Option<std::sync::mpsc::SyncSender<()>>,
    /// Number of crash-recovery restarts since the service was first started.
    restart_count: u32,
}

/// Number of consecutive health check failures before transitioning to `Unhealthy`
/// and triggering a restart via SIGTERM.
const HEALTH_FAILURE_THRESHOLD: u32 = 3;

/// Spawn a background task that continuously monitors the health of a running service.
/// On `HEALTH_FAILURE_THRESHOLD` consecutive failures the service is transitioned to
/// `Unhealthy` and SIGTERM'd — crash recovery picks it up and restarts.
/// The task exits once the service leaves the Running/Unhealthy state (e.g. stopped).
fn run_health_monitor(
    svc_name: String,
    checker: Arc<HealthChecker>,
    svc: ServiceDef,
    handles: Arc<RwLock<HashMap<String, ServiceHandle>>>,
    events: broadcast::Sender<SupervisorEvent>,
) {
    tokio::spawn(async move {
        let mut consecutive_failures: u32 = 0;

        loop {
            tokio::time::sleep(checker.config.interval).await;

            // Exit if service is no longer running.
            let port = {
                let map = handles.read().await;
                match map.get(&svc_name) {
                    Some(h) => match &h.state {
                        ServiceState::Running { .. } | ServiceState::Unhealthy { .. } => h.port,
                        _ => break,
                    },
                    None => break,
                }
            };

            if checker.check_once(port, &svc).await {
                if consecutive_failures > 0 {
                    tracing::info!("[{svc_name}] health check recovered");
                    // Restore Running state if currently Unhealthy.
                    let mut map = handles.write().await;
                    if let Some(h) = map.get_mut(&svc_name) {
                        if let ServiceState::Unhealthy { pid, .. } = h.state {
                            h.state = ServiceState::Running {
                                pid,
                                since: Instant::now(),
                            };
                            let _ = events.send(SupervisorEvent::StateChanged {
                                service: svc_name.clone(),
                                state: "running".into(),
                            });
                        }
                    }
                }
                consecutive_failures = 0;
            } else {
                consecutive_failures += 1;
                tracing::warn!(
                    "[{svc_name}] health check failed ({consecutive_failures}/{})",
                    HEALTH_FAILURE_THRESHOLD
                );

                if consecutive_failures >= HEALTH_FAILURE_THRESHOLD {
                    // Transition to Unhealthy, then kill — crash recovery will restart.
                    let pid = {
                        let mut map = handles.write().await;
                        if let Some(h) = map.get_mut(&svc_name) {
                            let pid = h.state.pid();
                            if let Some(p) = pid {
                                h.state = ServiceState::Unhealthy {
                                    pid: p,
                                    failures: consecutive_failures,
                                };
                            }
                            pid
                        } else {
                            break;
                        }
                    };
                    let _ = events.send(SupervisorEvent::StateChanged {
                        service: svc_name.clone(),
                        state: "unhealthy".into(),
                    });
                    tracing::error!(
                        "[{svc_name}] unhealthy after {consecutive_failures} failures — restarting"
                    );
                    #[cfg(unix)]
                    if let Some(p) = pid {
                        use nix::sys::signal::{kill, Signal};
                        use nix::unistd::Pid;
                        // Kill the process group so child processes die too
                        let _ = kill(Pid::from_raw(-(p as i32)), Signal::SIGTERM);
                    }
                    // Exit — crash recovery owns the restart and will re-arm a new monitor.
                    break;
                }
            }
        }
    });
}

/// A shared, hot-swappable config cell. Wrapping `Arc<DevConfig>` in a std `RwLock` allows
/// `reload()` to atomically replace the config without touching any async state.
type ConfigCell = Arc<std::sync::RwLock<Arc<DevConfig>>>;

pub struct Supervisor {
    config: ConfigCell,
    /// Path to A3sfile.hcl — used by `reload_from_disk` and `socket_path`.
    pub config_path: std::path::PathBuf,
    /// The env_override name used at startup (if any). Preserved across hot-reloads.
    env_name: Arc<std::sync::RwLock<Option<String>>>,
    handles: Arc<RwLock<HashMap<String, ServiceHandle>>>,
    events: broadcast::Sender<SupervisorEvent>,
    log: Arc<LogAggregator>,
    proxy: Arc<ProxyRouter>,
}

/// Summary of what changed during a hot-reload.
pub struct ReloadSummary {
    pub started: Vec<String>,
    pub stopped: Vec<String>,
    pub restarted: Vec<String>,
}

impl Supervisor {
    pub fn new(
        config: Arc<DevConfig>,
        proxy: Arc<ProxyRouter>,
        config_path: std::path::PathBuf,
        env_name: Option<String>,
    ) -> (Self, broadcast::Receiver<SupervisorEvent>) {
        let (events, rx) = broadcast::channel(4096);
        let (log, log_rx) = LogAggregator::new();
        let log = Arc::new(log);
        tokio::spawn(LogAggregator::print_loop(log_rx));
        LogAggregator::spawn_history_recorder(log.clone());
        (
            Self {
                config: Arc::new(std::sync::RwLock::new(config)),
                config_path,
                env_name: Arc::new(std::sync::RwLock::new(env_name)),
                handles: Arc::new(RwLock::new(HashMap::new())),
                events,
                log,
                proxy,
            },
            rx,
        )
    }

    /// Return a snapshot of the current config. Cheap — only clones the Arc.
    fn cfg(&self) -> Arc<DevConfig> {
        Arc::clone(&self.config.read().unwrap())
    }

    pub fn subscribe_logs(&self) -> broadcast::Receiver<crate::log::LogLine> {
        self.log.subscribe()
    }

    pub fn log_history(&self, services: &[String], lines: usize) -> Vec<crate::log::LogLine> {
        self.log.recent(services, lines)
    }

    /// Start all non-disabled services, launching each wave concurrently.
    /// Services within a wave have no inter-dependencies, so they can start in parallel.
    pub async fn start_all(self: &Arc<Self>) -> Result<()> {
        let cfg = self.cfg();
        let graph = DependencyGraph::from_config(&cfg)?;

        // Global color index by position in topological order
        let color: HashMap<String, usize> = graph
            .start_order()
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();

        let waves = graph.start_waves().to_vec();
        let total_waves = waves.len();
        for (wave_idx, wave) in waves.iter().enumerate() {
            let active: Vec<&str> = wave
                .iter()
                .filter(|n| !cfg.service.get(*n).is_some_and(|s| s.disabled))
                .map(|s| s.as_str())
                .collect();
            if !active.is_empty() {
                println!(
                    "{} wave {}/{}: {}",
                    "→".cyan(),
                    wave_idx + 1,
                    total_waves,
                    active.join(", ")
                );
            }
            let mut set = tokio::task::JoinSet::new();
            for name in wave {
                if cfg.service.get(name).is_some_and(|s| s.disabled) {
                    tracing::info!("[{name}] skipped (disabled)");
                    continue;
                }
                let sup = Arc::clone(self);
                let name = name.clone();
                let idx = color.get(&name).copied().unwrap_or(0);
                set.spawn(async move { sup.start_service(&name, idx).await });
            }
            while let Some(res) = set.join_next().await {
                res.map_err(|e| DevError::Config(e.to_string()))??;
            }
        }
        Ok(())
    }

    /// Start only the named services (and their transitive deps), in dependency order.
    pub async fn start_named(self: &Arc<Self>, names: &[String]) -> Result<()> {
        let cfg = self.cfg();
        let graph = DependencyGraph::from_config(&cfg)?;
        let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let to_start = graph.transitive_start_order(&name_refs);

        let color: HashMap<String, usize> = graph
            .start_order()
            .iter()
            .enumerate()
            .map(|(i, n)| (n.clone(), i))
            .collect();

        for name in &to_start {
            if cfg.service.get(name).is_some_and(|s| s.disabled) {
                tracing::info!("[{name}] skipped (disabled)");
                continue;
            }
            let idx = color.get(name).copied().unwrap_or(0);
            self.start_service(name, idx).await?;
        }
        Ok(())
    }

    pub async fn start_service(&self, name: &str, color_idx: usize) -> Result<()> {
        let cfg = self.cfg();
        let svc = cfg
            .service
            .get(name)
            .ok_or_else(|| DevError::UnknownService(name.to_string()))?
            .clone();

        self.emit(SupervisorEvent::StateChanged {
            service: name.to_string(),
            state: "starting".into(),
        });

        // Resolve port: 0 = auto-assign a free port (portless-style)
        let port = if svc.port == 0 {
            free_port()
                .ok_or_else(|| DevError::Config(format!("[{name}] no free port available")))?
        } else {
            svc.port
        };

        // Register proxy route now that the real port is known
        if let Some(sub) = &svc.subdomain {
            self.proxy.update(sub.clone(), port).await;
            tracing::info!("[{name}] starting on :{port} → http://{sub}.localhost");
        } else {
            tracing::info!("[{name}] starting on :{port}");
        }

        // Resolve ${other.port} references using the currently-assigned ports of all
        // running services.  This must happen after `port` is known for this service.
        let svc = {
            let mut runtime_ports: HashMap<String, u16> = self
                .handles
                .read()
                .await
                .iter()
                .map(|(n, h)| (n.clone(), h.port))
                .collect();
            runtime_ports.insert(name.to_string(), port);
            crate::config::resolve_service_ports(svc, &runtime_ports)
        };

        let spec = SpawnSpec {
            name,
            svc: &svc,
            port,
            color_idx,
            config_dir: self
                .config_path
                .parent()
                .unwrap_or(std::path::Path::new(".")),
            runtime: &cfg.dev.runtime,
        };
        let result = spawn_process(&spec, &self.log).await?;

        self.handles.write().await.insert(
            name.to_string(),
            ServiceHandle {
                child: result.child,
                state: ServiceState::Running {
                    pid: result.pid,
                    since: Instant::now(),
                },
                color_idx,
                port,
                watcher_stop: None,
                restart_count: 0,
            },
        );

        self.emit(SupervisorEvent::StateChanged {
            service: name.to_string(),
            state: "running".into(),
        });

        // Build health checker once so both startup wait and ongoing monitor share it.
        let health_info: Option<(Arc<HealthChecker>, ServiceDef)> =
            HealthChecker::for_service(&svc).map(|c| (Arc::new(c), svc.clone()));

        // Crash recovery — monitor process and auto-restart on unexpected exit.
        // Pass health_info so recovery can re-arm the monitor after each restart.
        self.spawn_crash_recovery(name.to_string(), color_idx, health_info.clone());

        // Wait for health before unblocking dependents, then start ongoing monitor.
        if let Some((checker, svc_def)) = health_info {
            let healthy = checker.wait_healthy(&svc_def, port).await;
            self.emit(SupervisorEvent::HealthChange {
                service: name.to_string(),
                healthy,
            });
            if !healthy {
                tracing::warn!(
                    "[{name}] health check failed after {} retries",
                    checker.config.retries
                );
            }
            // Start ongoing health monitor regardless of startup result.
            run_health_monitor(
                name.to_string(),
                checker,
                svc_def,
                self.handles.clone(),
                self.events.clone(),
            );
        }

        // File watcher → auto-restart on change
        if let Some(watch) = &svc.watch {
            if watch.restart {
                let stop_tx = self.spawn_file_watcher(
                    name.to_string(),
                    watch.paths.clone(),
                    watch.ignore.clone(),
                );
                if let Some(h) = self.handles.write().await.get_mut(name) {
                    h.watcher_stop = Some(stop_tx);
                }
            }
        }

        Ok(())
    }

    /// Stop all running services in reverse wave order, stopping each wave concurrently.
    /// Returns the names of services that were actually running and got stopped.
    pub async fn stop_all(self: &Arc<Self>) -> Vec<String> {
        let graph = match DependencyGraph::from_config(&self.cfg()) {
            Ok(g) => g,
            Err(_) => return vec![],
        };
        let running: Vec<String> = {
            let map = self.handles.read().await;
            map.iter()
                .filter(|(_, h)| !matches!(h.state, ServiceState::Stopped))
                .map(|(n, _)| n.clone())
                .collect()
        };
        for wave in graph.start_waves().iter().rev() {
            let mut set = tokio::task::JoinSet::new();
            for name in wave {
                let sup = Arc::clone(self);
                let name = name.clone();
                set.spawn(async move { sup.stop_service(&name).await });
            }
            while set.join_next().await.is_some() {}
        }
        running
    }

    pub async fn stop_service(&self, name: &str) {
        let cfg = self.cfg();
        let svc_def = cfg.service.get(name).cloned();
        let stop_timeout = svc_def
            .as_ref()
            .map(|s| s.stop_timeout)
            .unwrap_or(std::time::Duration::from_secs(5));

        // Extract what we need and mark stopped — all under a brief write lock.
        // We do NOT hold the lock across the async wait so parallel stop calls
        // (e.g. from stop_all) can run concurrently without blocking each other.
        let extracted = {
            let mut map = self.handles.write().await;
            if let Some(h) = map.get_mut(name) {
                if let Some(ref stop_tx) = h.watcher_stop {
                    let _ = stop_tx.send(());
                }
                let pid = h.state.pid();
                // Swap child out so we can await it without holding the lock.
                let child = std::mem::replace(
                    &mut h.child,
                    tokio::process::Command::new("true").spawn().unwrap(),
                );
                h.state = ServiceState::Stopped;
                Some((pid, child))
            } else {
                None
            }
        }; // write lock dropped here

        self.emit(SupervisorEvent::StateChanged {
            service: name.to_string(),
            state: "stopped".into(),
        });

        let Some((pid_opt, mut child)) = extracted else {
            return;
        };

        #[cfg(unix)]
        if let Some(pid) = pid_opt {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            // Negative PID kills the entire process group (pgid = pid since we
            // spawned with process_group(0)).
            let pgid = Pid::from_raw(-(pid as i32));
            let _ = kill(pgid, Signal::SIGTERM);
            let _ = tokio::time::timeout(stop_timeout, child.wait()).await;
            let _ = kill(pgid, Signal::SIGKILL);
        }
        let _ = child.kill().await;

        // For box runtime, force-remove the named container so the port is freed.
        if self.cfg().dev.runtime == "box" {
            let container_name = format!("a3s-{name}");
            tokio::process::Command::new("a3s-box")
                .args(["rm", "-f", &container_name])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .await
                .ok();
        }

        // Run post_stop hook after the process is gone.
        if let Some(svc) = svc_def {
            if let Some(ref hook) = svc.post_stop {
                tracing::info!("[{name}] running post_stop: {hook}");
                if let Err(e) = spawn::run_hook(hook, &svc, name).await {
                    tracing::warn!("[{name}] post_stop hook failed: {e}");
                }
            }
        }
    }

    /// Stop only the named services and any services that transitively depend on them,
    /// in safe order (dependents first, then targets).
    /// Returns the names of services that were stopped.
    pub async fn stop_named(&self, names: &[String]) -> Vec<String> {
        let cfg = self.cfg();
        let stop_order = match crate::graph::DependencyGraph::from_config(&cfg) {
            Ok(g) => {
                let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
                g.transitive_dependents_stop_order(&refs)
            }
            Err(_) => names.to_vec(),
        };
        for name in &stop_order {
            self.stop_service(name).await;
        }
        stop_order
    }

    /// Reload A3sfile.hcl from disk and apply changes without restarting unchanged services.
    pub async fn reload_from_disk(&self) -> Result<ReloadSummary> {
        let env_name = self.env_name.read().unwrap().clone();
        let new_cfg = DevConfig::from_file_with_env(&self.config_path, env_name.as_deref())?;
        self.reload(Arc::new(new_cfg)).await
    }

    pub async fn restart_service(self: &Arc<Self>, name: &str) -> Result<()> {
        let cfg = self.cfg();
        let graph = DependencyGraph::from_config(&cfg)?;

        // Dependents must stop before the target; stop_order = [dependents..., target].
        let stop_order = graph.transitive_dependents_stop_order(&[name]);

        // Snapshot color indices before stopping (handles will be removed).
        let colors: HashMap<String, usize> = {
            let map = self.handles.read().await;
            stop_order
                .iter()
                .map(|n| (n.clone(), map.get(n).map(|h| h.color_idx).unwrap_or(0)))
                .collect()
        };

        for svc_name in &stop_order {
            self.stop_service(svc_name).await;
        }

        // Restart in topological order: target first, then dependents.
        for svc_name in stop_order.iter().rev() {
            let idx = colors.get(svc_name).copied().unwrap_or(0);
            self.start_service(svc_name, idx).await?;
        }

        Ok(())
    }

    pub async fn status_rows(&self) -> Vec<StatusRow> {
        let cfg = self.cfg();
        let map = self.handles.read().await;
        cfg.service
            .iter()
            .map(|(name, svc)| {
                let handle = map.get(name);
                let state = handle
                    .map(|h| h.state.label().to_string())
                    .unwrap_or_else(|| "pending".into());
                let pid = handle.and_then(|h| h.state.pid());
                let uptime_secs = handle.and_then(|h| {
                    if let ServiceState::Running { since, .. } = h.state {
                        Some(since.elapsed().as_secs())
                    } else {
                        None
                    }
                });
                StatusRow {
                    name: name.clone(),
                    state,
                    pid,
                    port: handle.map(|h| h.port).unwrap_or(svc.port),
                    subdomain: svc.subdomain.clone(),
                    uptime_secs,
                    proxy_port: cfg.dev.proxy_port,
                    restart_count: handle.map(|h| h.restart_count).unwrap_or(0),
                    healthy: if svc.health.is_some() {
                        Some(!matches!(
                            handle.map(|h| &h.state),
                            Some(ServiceState::Unhealthy { .. })
                        ))
                    } else {
                        None
                    },
                }
            })
            .collect()
    }

    fn emit(&self, event: SupervisorEvent) {
        let _ = self.events.send(event);
    }

    /// Spawn a task that monitors the process and auto-restarts on unexpected exit.
    /// `health_info` is cloned and used to re-arm the ongoing health monitor after each restart.
    fn spawn_crash_recovery(
        &self,
        svc_name: String,
        color_idx: usize,
        health_info: Option<(Arc<HealthChecker>, ServiceDef)>,
    ) {
        let handles = self.handles.clone();
        let events = self.events.clone();
        let config_cell = self.config.clone();
        let log = self.log.clone();
        let proxy = self.proxy.clone();
        let config_dir = self
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();

        tokio::spawn(async move {
            // Capture assigned port once — preserves auto-assigned port across restarts
            let assigned_port = handles
                .read()
                .await
                .get(&svc_name)
                .map(|h| h.port)
                .unwrap_or(0);
            let mut restart_count = 0u32;

            loop {
                // Wait for the process to exit — take the child out first so we
                // don't hold the write lock across an async wait.
                let child_done = {
                    let mut map = handles.write().await;
                    if let Some(h) = map.get_mut(&svc_name) {
                        if !matches!(
                            h.state,
                            ServiceState::Running { .. } | ServiceState::Unhealthy { .. }
                        ) {
                            break;
                        }
                        // Replace child with a dummy so we can await outside the lock.
                        // Safety: we immediately await the real child below.
                        Some(std::mem::replace(
                            &mut h.child,
                            tokio::process::Command::new("true").spawn().unwrap(),
                        ))
                    } else {
                        break;
                    }
                };

                let exit_status = if let Some(mut child) = child_done {
                    child.wait().await.ok()
                } else {
                    break;
                };

                // Check if we were intentionally stopped
                {
                    let map = handles.read().await;
                    match map.get(&svc_name) {
                        Some(h) if matches!(h.state, ServiceState::Stopped) => break,
                        None => break,
                        _ => {}
                    }
                }

                // Read restart policy from current service config (reflects reloads).
                let restart_policy = config_cell
                    .read()
                    .unwrap()
                    .service
                    .get(&svc_name)
                    .map(|s| s.restart.clone())
                    .unwrap_or_default();

                if matches!(restart_policy.on_failure, crate::config::OnFailure::Stop) {
                    let code = exit_status.and_then(|s| s.code());
                    tracing::warn!(
                        "[{svc_name}] exited (code={}) — on_failure=stop, not restarting",
                        code.map(|c| c.to_string()).unwrap_or_else(|| "?".into())
                    );
                    let _ = events.send(SupervisorEvent::StateChanged {
                        service: svc_name.clone(),
                        state: "failed".into(),
                    });
                    break;
                }

                restart_count += 1;
                if restart_count > restart_policy.max_restarts {
                    tracing::error!(
                        "[{svc_name}] crashed {} times — giving up",
                        restart_policy.max_restarts
                    );
                    let _ = events.send(SupervisorEvent::StateChanged {
                        service: svc_name.clone(),
                        state: "failed".into(),
                    });
                    break;
                }

                let backoff = {
                    let base = restart_policy.backoff.as_secs().max(1);
                    let exp = base
                        .saturating_pow(restart_count)
                        .min(restart_policy.max_backoff.as_secs());
                    std::time::Duration::from_secs(exp)
                };

                let code = exit_status.and_then(|s| s.code());
                tracing::warn!(
                    "[{svc_name}] exited (code={}) — restarting in {}s ({restart_count}/{})",
                    code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                    backoff.as_secs(),
                    restart_policy.max_restarts,
                );
                let _ = events.send(SupervisorEvent::StateChanged {
                    service: svc_name.clone(),
                    state: "restarting".into(),
                });

                tokio::time::sleep(backoff).await;

                // Check that the service wasn't stopped during the backoff sleep.
                {
                    let map = handles.read().await;
                    match map.get(&svc_name) {
                        Some(h)
                            if matches!(
                                h.state,
                                ServiceState::Stopped | ServiceState::Failed { .. }
                            ) =>
                        {
                            break;
                        }
                        None => break,
                        _ => {}
                    }
                }

                let (svc_def, runtime) = {
                    let cfg = config_cell.read().unwrap();
                    let s = match cfg.service.get(&svc_name) {
                        Some(s) => s.clone(),
                        None => break,
                    };
                    (s, cfg.dev.runtime.clone())
                };
                // Use originally assigned port — avoids re-assigning a new port for port=0 services
                let port = if assigned_port > 0 {
                    assigned_port
                } else {
                    svc_def.port
                };

                // Resolve ${other.port} references at restart time.
                let resolved_def = {
                    let runtime_ports: HashMap<String, u16> = handles
                        .read()
                        .await
                        .iter()
                        .map(|(n, h)| (n.clone(), h.port))
                        .collect();
                    crate::config::resolve_service_ports(svc_def.clone(), &runtime_ports)
                };

                let spec = SpawnSpec {
                    name: &svc_name,
                    svc: &resolved_def,
                    port,
                    color_idx,
                    config_dir: &config_dir,
                    runtime: &runtime,
                };
                match spawn_process(&spec, &log).await {
                    Ok(result) => {
                        if let Some(sub) = &svc_def.subdomain {
                            proxy.update(sub.clone(), port).await;
                        }
                        let mut map = handles.write().await;
                        let prev_restart_count =
                            map.get(&svc_name).map(|h| h.restart_count).unwrap_or(0);
                        map.insert(
                            svc_name.clone(),
                            ServiceHandle {
                                child: result.child,
                                state: ServiceState::Running {
                                    pid: result.pid,
                                    since: Instant::now(),
                                },
                                color_idx,
                                port,
                                watcher_stop: None,
                                restart_count: prev_restart_count + 1,
                            },
                        );
                        let _ = events.send(SupervisorEvent::StateChanged {
                            service: svc_name.clone(),
                            state: "running".into(),
                        });
                        // Re-arm health monitor for the restarted process.
                        if let Some((ref checker, ref svc_def_h)) = health_info {
                            run_health_monitor(
                                svc_name.clone(),
                                checker.clone(),
                                svc_def_h.clone(),
                                handles.clone(),
                                events.clone(),
                            );
                        }
                        restart_count = 0;
                    }
                    Err(e) => {
                        tracing::error!("[{svc_name}] restart failed: {e}");
                        break;
                    }
                }
            }
        });
    }

    /// Spawn a task that watches files and restarts the service on change.
    /// Returns a sender that stops the watcher when any value is sent.
    fn spawn_file_watcher(
        &self,
        svc_name: String,
        paths: Vec<std::path::PathBuf>,
        ignore: Vec<String>,
    ) -> std::sync::mpsc::SyncSender<()> {
        let handles = self.handles.clone();
        let events = self.events.clone();
        let config_cell = self.config.clone();
        let log = self.log.clone();
        let config_dir = self
            .config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();

        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        let stop_tx = spawn_watcher(svc_name.clone(), paths, ignore, tx);
        // Clone so the task can propagate watcher_stop to restarted service handles.
        let task_stop_tx = stop_tx.clone();

        tokio::spawn(async move {
            while let Some(changed_svc) = rx.recv().await {
                tracing::info!("[{changed_svc}] file change — restarting");

                let (color_idx, port) = {
                    let mut map = handles.write().await;
                    if let Some(h) = map.get_mut(&changed_svc) {
                        let idx = h.color_idx;
                        let p = h.port;
                        let _ = h.child.kill().await;
                        h.state = ServiceState::Stopped;
                        (idx, p)
                    } else {
                        continue;
                    }
                };

                let _ = events.send(SupervisorEvent::StateChanged {
                    service: changed_svc.clone(),
                    state: "restarting".into(),
                });

                let (svc_def, runtime) = {
                    let cfg = config_cell.read().unwrap();
                    let s = match cfg.service.get(&changed_svc) {
                        Some(s) => s.clone(),
                        None => continue,
                    };
                    (s, cfg.dev.runtime.clone())
                };

                let spec = SpawnSpec {
                    name: &changed_svc,
                    svc: &svc_def,
                    port,
                    color_idx,
                    config_dir: &config_dir,
                    runtime: &runtime,
                };
                match spawn_process(&spec, &log).await {
                    Ok(result) => {
                        handles.write().await.insert(
                            changed_svc.clone(),
                            ServiceHandle {
                                child: result.child,
                                state: ServiceState::Running {
                                    pid: result.pid,
                                    since: Instant::now(),
                                },
                                color_idx,
                                port,
                                // Propagate watcher_stop so stop_service() can cancel the
                                // watcher even after a file-watcher-triggered restart.
                                watcher_stop: Some(task_stop_tx.clone()),
                                restart_count: 0,
                            },
                        );
                        let _ = events.send(SupervisorEvent::StateChanged {
                            service: changed_svc.clone(),
                            state: "running".into(),
                        });
                    }
                    Err(e) => {
                        tracing::error!("[{changed_svc}] restart failed: {e}");
                    }
                }
            }
        });

        stop_tx
    }

    /// Hot-reload: apply a new config without a full restart.
    ///
    /// - Services removed from the new config (or newly `disabled`) are stopped.
    /// - Services whose config changed are restarted.
    /// - Services newly added (and not `disabled`) are started.
    /// - Unchanged running services are left alone.
    pub async fn reload(&self, new_config: Arc<DevConfig>) -> Result<ReloadSummary> {
        let old_config = self.cfg();
        let mut summary = ReloadSummary {
            started: vec![],
            stopped: vec![],
            restarted: vec![],
        };

        // 1. Stop removed / newly-disabled services.
        for name in old_config.service.keys() {
            let gone = !new_config.service.contains_key(name);
            let disabled = new_config.service.get(name).is_some_and(|s| s.disabled);
            if gone || disabled {
                tracing::info!("[{name}] stopping — removed or disabled in reloaded config");
                self.stop_service(name).await;
                summary.stopped.push(name.clone());
            }
        }

        // 2. Swap in the new config so start_service sees it.
        *self.config.write().unwrap() = Arc::clone(&new_config);

        // 3. Restart changed services and start new ones in dependency order.
        let graph = DependencyGraph::from_config(&new_config)?;
        for (idx, name) in graph.start_order().iter().enumerate() {
            let Some(new_svc) = new_config.service.get(name) else {
                continue;
            };
            if new_svc.disabled {
                continue;
            }
            match old_config.service.get(name) {
                Some(old_svc) if old_svc == new_svc => {
                    // Unchanged — leave running.
                }
                Some(_) => {
                    tracing::info!("[{name}] config changed — restarting");
                    self.stop_service(name).await;
                    self.start_service(name, idx).await?;
                    summary.restarted.push(name.clone());
                }
                None => {
                    tracing::info!("[{name}] new service — starting");
                    self.start_service(name, idx).await?;
                    summary.started.push(name.clone());
                }
            }
        }

        tracing::info!("config reloaded ({} services)", new_config.service.len());
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DevConfig, GlobalSettings, ServiceDef};
    use indexmap::IndexMap;

    fn svc(cmd: &str, deps: Vec<&str>) -> ServiceDef {
        ServiceDef {
            cmd: cmd.to_string(),
            dir: None,
            port: 0, // auto-assign — avoids port conflicts across parallel tests
            subdomain: None,
            env: Default::default(),
            env_file: None,
            log_file: None,
            log_rotate_mb: 0,
            pre_start: None,
            post_stop: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            watch: None,
            health: None,
            restart: Default::default(),
            stop_timeout: std::time::Duration::from_secs(1),
            disabled: false,
            labels: vec![],
            k8s: None,
            r#box: None,
        }
    }

    fn make_config(services: Vec<(&str, ServiceDef)>) -> Arc<DevConfig> {
        let mut map = IndexMap::new();
        for (name, def) in services {
            map.insert(name.to_string(), def);
        }
        Arc::new(DevConfig {
            dev: GlobalSettings::default(),
            service: map,
            env_override: Default::default(),
        })
    }

    fn make_supervisor(cfg: Arc<DevConfig>) -> Arc<Supervisor> {
        let proxy = Arc::new(crate::proxy::ProxyRouter::new(0));
        let (sup, _) = Supervisor::new(cfg, proxy, std::path::PathBuf::from(""), None);
        Arc::new(sup)
    }

    #[tokio::test]
    async fn test_status_rows_empty_config() {
        let sup = make_supervisor(make_config(vec![]));
        assert!(sup.status_rows().await.is_empty());
    }

    #[tokio::test]
    async fn test_status_rows_pending_before_start() {
        let sup = make_supervisor(make_config(vec![("web", svc("sleep 60", vec![]))]));
        let rows = sup.status_rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "web");
        assert_eq!(rows[0].state, "pending");
    }

    #[tokio::test]
    async fn test_start_and_stop_service() {
        let sup = make_supervisor(make_config(vec![("web", svc("sleep 60", vec![]))]));
        sup.start_service("web", 0).await.unwrap();

        let rows = sup.status_rows().await;
        assert_eq!(rows[0].state, "running", "expected running after start");

        sup.stop_service("web").await;
        let rows = sup.status_rows().await;
        assert_eq!(rows[0].state, "stopped", "expected stopped after stop");
    }

    #[tokio::test]
    async fn test_start_unknown_service_errors() {
        let sup = make_supervisor(make_config(vec![]));
        assert!(sup.start_service("nope", 0).await.is_err());
    }

    #[tokio::test]
    async fn test_start_all_starts_all_services() {
        let sup = make_supervisor(make_config(vec![
            ("a", svc("sleep 60", vec![])),
            ("b", svc("sleep 60", vec![])),
        ]));
        sup.clone().start_all().await.unwrap();

        let rows = sup.status_rows().await;
        assert!(
            rows.iter().all(|r| r.state == "running"),
            "not all running: {rows:?}"
        );

        sup.stop_all().await;
    }

    #[tokio::test]
    async fn test_start_named_resolves_deps() {
        // b depends on a → start_named(["b"]) must also start a
        let sup = make_supervisor(make_config(vec![
            ("a", svc("sleep 60", vec![])),
            ("b", svc("sleep 60", vec!["a"])),
        ]));
        sup.clone().start_named(&["b".to_string()]).await.unwrap();

        let rows = sup.status_rows().await;
        assert!(
            rows.iter().all(|r| r.state == "running"),
            "expected both a and b running: {rows:?}"
        );

        sup.stop_all().await;
    }

    #[tokio::test]
    async fn test_start_named_skips_unrequested_services() {
        // a and c are independent; requesting only a should not start c
        let sup = make_supervisor(make_config(vec![
            ("a", svc("sleep 60", vec![])),
            ("c", svc("sleep 60", vec![])),
        ]));
        sup.clone().start_named(&["a".to_string()]).await.unwrap();

        let rows = sup.status_rows().await;
        let a = rows.iter().find(|r| r.name == "a").unwrap();
        let c = rows.iter().find(|r| r.name == "c").unwrap();
        assert_eq!(a.state, "running");
        assert_eq!(c.state, "pending");

        sup.stop_all().await;
    }

    #[tokio::test]
    async fn test_restart_service() {
        let sup = make_supervisor(make_config(vec![("web", svc("sleep 60", vec![]))]));
        sup.start_service("web", 0).await.unwrap();

        let pid_before = sup.status_rows().await[0].pid;
        sup.restart_service("web").await.unwrap();
        let pid_after = sup.status_rows().await[0].pid;

        assert_eq!(sup.status_rows().await[0].state, "running");
        // PID should change after restart
        assert_ne!(pid_before, pid_after, "expected new PID after restart");

        sup.stop_all().await;
    }
}
