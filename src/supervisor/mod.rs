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
                            h.state = ServiceState::Running { pid, since: Instant::now() };
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
                                h.state =
                                    ServiceState::Unhealthy { pid: p, failures: consecutive_failures };
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
    /// Path to A3sfile.hcl — used by `reload_from_disk`.
    config_path: std::path::PathBuf,
    handles: Arc<RwLock<HashMap<String, ServiceHandle>>>,
    events: broadcast::Sender<SupervisorEvent>,
    log: Arc<LogAggregator>,
    proxy: Arc<ProxyRouter>,
}

impl Supervisor {
    pub fn new(
        config: Arc<DevConfig>,
        proxy: Arc<ProxyRouter>,
        config_path: std::path::PathBuf,
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

    pub fn log_history(&self, service: Option<&str>, lines: usize) -> Vec<crate::log::LogLine> {
        self.log.recent(service, lines)
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

        for wave in graph.start_waves() {
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

        let spec = SpawnSpec {
            name,
            svc: &svc,
            port,
            color_idx,
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

    pub async fn stop_all(&self) {
        let graph = match DependencyGraph::from_config(&self.cfg()) {
            Ok(g) => g,
            Err(_) => return,
        };
        let names: Vec<String> = graph.stop_order().map(|s| s.to_string()).collect();
        for name in &names {
            self.stop_service(name).await;
        }
    }

    pub async fn stop_service(&self, name: &str) {
        let stop_timeout = self
            .cfg()
            .service
            .get(name)
            .map(|s| s.stop_timeout)
            .unwrap_or(std::time::Duration::from_secs(5));

        let mut map = self.handles.write().await;
        if let Some(h) = map.get_mut(name) {
            // Cancel file watcher first
            if let Some(ref stop_tx) = h.watcher_stop {
                let _ = stop_tx.send(());
            }
            #[cfg(unix)]
            {
                use nix::sys::signal::{kill, Signal};
                use nix::unistd::Pid;
                if let Some(pid) = h.state.pid() {
                    // Negative PID = kill the entire process group (pgid = pid since we
                    // spawned with process_group(0)). This ensures child processes of wrappers
                    // like `npm run dev` are also terminated, not just the wrapper itself.
                    let pgid = Pid::from_raw(-(pid as i32));
                    let _ = kill(pgid, Signal::SIGTERM);
                    let _ = tokio::time::timeout(stop_timeout, h.child.wait()).await;
                    // SIGKILL the group if still alive after grace period
                    let _ = kill(pgid, Signal::SIGKILL);
                }
            }
            let _ = h.child.kill().await;
            h.state = ServiceState::Stopped;
            self.emit(SupervisorEvent::StateChanged {
                service: name.to_string(),
                state: "stopped".into(),
            });
        }
    }

    /// Stop only the named services and any services that transitively depend on them,
    /// in safe order (dependents first, then targets).
    pub async fn stop_named(&self, names: &[String]) {
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
    }

    /// Reload A3sfile.hcl from disk and apply changes without restarting unchanged services.
    pub async fn reload_from_disk(&self) -> Result<()> {
        let new_cfg = DevConfig::from_file(&self.config_path)?;
        self.reload(Arc::new(new_cfg)).await
    }

    pub async fn restart_service(&self, name: &str) -> Result<()> {
        let color_idx = self
            .handles
            .read()
            .await
            .get(name)
            .map(|h| h.color_idx)
            .unwrap_or(0);
        self.stop_service(name).await;
        self.start_service(name, color_idx).await
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
                        if !matches!(h.state, ServiceState::Running { .. } | ServiceState::Unhealthy { .. }) {
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
                    let exp = base.saturating_pow(restart_count).min(
                        restart_policy.max_backoff.as_secs(),
                    );
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

                let svc_def = match config_cell.read().unwrap().service.get(&svc_name) {
                    Some(s) => s.clone(),
                    None => break,
                };
                // Use originally assigned port — avoids re-assigning a new port for port=0 services
                let port = if assigned_port > 0 {
                    assigned_port
                } else {
                    svc_def.port
                };

                let spec = SpawnSpec {
                    name: &svc_name,
                    svc: &svc_def,
                    port,
                    color_idx,
                };
                match spawn_process(&spec, &log).await {
                    Ok(result) => {
                        if let Some(sub) = &svc_def.subdomain {
                            proxy.update(sub.clone(), port).await;
                        }
                        handles.write().await.insert(
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

                let svc_def = match config_cell.read().unwrap().service.get(&changed_svc) {
                    Some(s) => s.clone(),
                    None => continue,
                };

                let spec = SpawnSpec {
                    name: &changed_svc,
                    svc: &svc_def,
                    port,
                    color_idx,
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
    pub async fn reload(&self, new_config: Arc<DevConfig>) -> Result<()> {
        let old_config = self.cfg();

        // 1. Stop removed / newly-disabled services.
        for name in old_config.service.keys() {
            let gone = !new_config.service.contains_key(name);
            let disabled = new_config.service.get(name).is_some_and(|s| s.disabled);
            if gone || disabled {
                tracing::info!("[{name}] stopping — removed or disabled in reloaded config");
                self.stop_service(name).await;
            }
        }

        // 2. Swap in the new config so start_service / restart_service see it.
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
                    self.restart_service(name).await?;
                }
                None => {
                    tracing::info!("[{name}] new service — starting");
                    self.start_service(name, idx).await?;
                }
            }
        }

        tracing::info!("config reloaded ({} services)", new_config.service.len());
        Ok(())
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
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            watch: None,
            health: None,
            restart: Default::default(),
            stop_timeout: std::time::Duration::from_secs(1),
            disabled: false,
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
        })
    }

    fn make_supervisor(cfg: Arc<DevConfig>) -> Arc<Supervisor> {
        let proxy = Arc::new(crate::proxy::ProxyRouter::new(0));
        let (sup, _) = Supervisor::new(cfg, proxy, std::path::PathBuf::from(""));
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
