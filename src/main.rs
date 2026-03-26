use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use colored::Colorize;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

mod box_mgr;
mod cert;
mod config;
mod error;
mod graph;
mod health;
mod ipc;
mod k8s;
mod log;
mod proxy;
mod state;
mod supervisor;
mod ui;
mod watcher;

use config::DevConfig;
use error::{DevError, Result};
use ipc::{IpcRequest, IpcResponse};
use supervisor::Supervisor;

#[derive(Parser)]
#[command(
    name = "a3s",
    version,
    about = "a3s — local development orchestration for the A3S monorepo",
    allow_external_subcommands = true
)]
struct Cli {
    /// Path to A3sfile.hcl
    #[arg(short, long, default_value = "A3sfile.hcl")]
    file: PathBuf,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start all (or named) services in dependency order
    Up {
        /// Start only these services
        services: Vec<String>,
        /// Filter services by label (can be repeated, e.g., --label backend --label critical)
        #[arg(short, long)]
        label: Vec<String>,
        /// Apply a named env_override block from A3sfile.hcl (e.g., --env staging)
        #[arg(short, long)]
        env: Option<String>,
        /// Run as background daemon (detach from terminal)
        #[arg(short = 'd', long)]
        detach: bool,
        /// Disable the web UI (default: enabled on port 10350)
        #[arg(long)]
        no_ui: bool,
        /// Web UI port
        #[arg(long, default_value_t = ui::DEFAULT_UI_PORT)]
        ui_port: u16,
        /// Wait for all services to become healthy before returning (requires --detach)
        #[arg(long)]
        wait: bool,
        /// Timeout in seconds for --wait (default: 60)
        #[arg(long, default_value_t = 60)]
        wait_timeout: u64,
    },
    /// Stop all (or named) services
    Down {
        /// Stop only these services
        services: Vec<String>,
        /// Filter services by label (can be repeated)
        #[arg(short, long)]
        label: Vec<String>,
    },
    /// Restart a service
    Restart { service: String },
    /// Show service status (alias: ps)
    #[command(alias = "ps")]
    Status {
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Continuously refresh (like top)
        #[arg(short, long)]
        watch: bool,
        /// Refresh interval in seconds (only with --watch)
        #[arg(short, long, default_value_t = 2)]
        interval: u64,
    },
    /// Tail logs (all services or one)
    Logs {
        /// Filter to specific services (can be repeated)
        #[arg(short, long)]
        service: Vec<String>,
        /// Keep streaming
        #[arg(short, long, default_value_t = true)]
        follow: bool,
        /// Filter log lines by keyword (case-insensitive)
        #[arg(short, long)]
        grep: Option<String>,
        /// Number of historical lines to show (default: 200)
        #[arg(short = 'n', long, default_value_t = 200)]
        last: usize,
        /// Prefix each line with a timestamp
        #[arg(short = 't', long)]
        timestamps: bool,
    },
    /// Validate A3sfile.hcl without starting anything
    Validate {
        /// Also check that service binaries exist on PATH and ports are not already in use
        #[arg(long)]
        strict: bool,
    },
    /// Show live CPU and memory usage per service (requires a running daemon)
    Top {
        /// Refresh interval in seconds
        #[arg(short, long, default_value_t = 2)]
        interval: u64,
    },
    /// Forward local port to a service in k8s cluster (k8s mode only)
    PortForward {
        /// Service name
        service: String,
        /// Port mapping: <local-port>:<remote-port>
        ports: String,
    },
    /// Upgrade a3s to the latest version
    Upgrade,
    /// List all installed a3s ecosystem tools
    List,
    /// Update installed a3s ecosystem tools (all if no names given)
    Update {
        /// Tool name(s) to update: box, gateway, power (default: all)
        tools: Vec<String>,
    },
    /// Run a command in a service's environment and directory
    Exec {
        /// Service name to take env and dir from
        service: String,
        /// Command and arguments
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Run a one-off command with the environment from A3sfile.hcl
    Run {
        /// Load env from a specific service (default: merge all services)
        #[arg(short, long)]
        service: Option<String>,
        /// Command and arguments to run
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Reload A3sfile.hcl without restarting unchanged services
    Reload,
    /// Proxy to an a3s ecosystem tool (e.g. `a3s box`, `a3s gateway`)
    #[command(external_subcommand)]
    Tool(Vec<String>),
}

#[tokio::main]
async fn main() {
    // Parse CLI first so we can read log_level from A3sfile.hcl for `up`
    let cli = Cli::parse();

    let log_level = if matches!(cli.command, Commands::Up { .. }) {
        std::fs::read_to_string(&cli.file)
            .ok()
            .and_then(|s| hcl::from_str::<config::DevConfig>(&config::expand_env_func(&s)).ok())
            .map(|c| c.dev.log_level)
            .unwrap_or_else(|| "info".into())
    } else {
        "warn".into()
    };

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(log_level)),
        )
        .without_time()
        .init();

    if let Err(e) = run(cli).await {
        eprintln!("{} {e}", "[a3s]".red().bold());
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    // Project-specific socket path — computed once and used by all IPC client commands.
    let sock = ipc::socket_path(&cli.file);

    match &cli.command {
        Commands::Up {
            services,
            label,
            env,
            detach,
            no_ui,
            ui_port,
            wait,
            wait_timeout,
        } => {
            if *detach {
                // Re-launch self as background daemon, dropping --detach flag
                let exe = std::env::current_exe()
                    .map_err(|e| DevError::Config(format!("cannot find self: {e}")))?;
                let mut args: Vec<String> =
                    vec!["--file".into(), cli.file.display().to_string(), "up".into()];
                if *no_ui {
                    args.push("--no-ui".into());
                }
                if *ui_port != ui::DEFAULT_UI_PORT {
                    args.push("--ui-port".into());
                    args.push(ui_port.to_string());
                }
                if let Some(e) = env {
                    args.push("--env".into());
                    args.push(e.clone());
                }
                for lbl in label {
                    args.push("--label".into());
                    args.push(lbl.clone());
                }
                args.extend(services.iter().cloned());

                std::process::Command::new(&exe)
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .map_err(|e| DevError::Config(format!("failed to daemonize: {e}")))?;

                println!("{} a3s daemon started in background", "✓".green());
                if *wait {
                    println!("{} waiting for services to become healthy...", "→".cyan());
                    wait_for_healthy(&sock, *wait_timeout).await?;
                    println!("{} all services healthy", "✓".green());
                } else {
                    println!("  run {} to check status", "a3s status".cyan());
                    println!("  run {} to stop", "a3s down".cyan());
                }
                return Ok(());
            }

            let cfg = Arc::new(DevConfig::from_file_with_env(&cli.file, env.as_deref())?);

            // Check runtime mode
            if cfg.dev.runtime == "k8s" {
                // Kubernetes mode
                println!("{} runtime: kubernetes", "→".cyan());

                // Check if kubectl is available
                if !k8s::K8sClient::check_available().await? {
                    return Err(DevError::Config(
                        "kubectl not found. Please install kubectl to use k8s runtime mode.".into(),
                    ));
                }

                let k8s_client =
                    k8s::K8sClient::new(cfg.dev.k8s_context.clone(), cfg.dev.k8s_namespace.clone());
                let (log, log_rx) = crate::log::LogAggregator::new();
                let log = std::sync::Arc::new(log);
                tokio::spawn(crate::log::LogAggregator::print_loop(log_rx));
                crate::log::LogAggregator::spawn_history_recorder(log.clone());
                let k8s_runtime = k8s::K8sRuntime::new(k8s_client, log, cfg.dev.registry.clone());

                println!("{} namespace: {}", "→".cyan(), cfg.dev.k8s_namespace);
                if let Some(ref ctx) = cfg.dev.k8s_context {
                    println!("{} context: {}", "→".cyan(), ctx);
                }

                let config_dir = cli
                    .file
                    .parent()
                    .unwrap_or(std::path::Path::new("."))
                    .to_path_buf();

                // Deploy services to k8s
                return run_k8s_mode(
                    cfg,
                    k8s_runtime,
                    config_dir,
                    services.clone(),
                    label.clone(),
                )
                .await;
            }

            // Local / box process mode (default)
            let runtime_label = if cfg.dev.runtime == "box" { "box" } else { "local" };
            println!("{} runtime: {runtime_label}", "→".cyan());

            // Start proxy
            let proxy = if cfg.dev.https {
                let config_dir = cli.file.parent().unwrap_or(std::path::Path::new("."));
                let (cert, key) = cert::get_or_create_cert(config_dir).await?;
                Arc::new(
                    proxy::ProxyRouter::new(cfg.dev.proxy_port)
                        .with_https(cert, key)
                        .map_err(|e| DevError::Config(format!("failed to setup HTTPS: {}", e)))?,
                )
            } else {
                Arc::new(proxy::ProxyRouter::new(cfg.dev.proxy_port))
            };
            let proxy_port = cfg.dev.proxy_port;
            let protocol = if cfg.dev.https { "https" } else { "http" };
            let proxy_run = proxy.clone();
            tokio::spawn(async move { proxy_run.run().await });
            println!(
                "{} proxy  {}://*.localhost:{}",
                "→".cyan(),
                protocol,
                proxy_port
            );

            let (sup, _) = Supervisor::new(cfg.clone(), proxy, cli.file.clone(), env.clone());
            let sup: Arc<Supervisor> = Arc::new(sup);

            tokio::spawn(supervisor::ipc::serve(sup.clone()));

            // Start web UI
            if !no_ui {
                let ui_port = *ui_port;
                let sup_ui = sup.clone();
                tokio::spawn(async move { ui::serve(sup_ui, ui_port).await });
                println!("{} ui     http://localhost:{}", "→".cyan(), ui_port);
                // Open browser after a short delay
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    let _ = std::process::Command::new("open")
                        .arg(format!("http://localhost:{ui_port}"))
                        .spawn();
                });
            }

            // Determine which services to start based on explicit names and/or labels
            let mut target_services = services.clone();
            if !label.is_empty() {
                let labeled = filter_by_labels(&cfg, label);
                if !labeled.is_empty() {
                    if !label.is_empty() && labeled.is_empty() {
                        return Err(DevError::Config(format!(
                            "no services found with labels: {}",
                            label.join(", ")
                        )));
                    }
                    target_services.extend(labeled);
                }
            }

            if target_services.is_empty() {
                sup.clone().start_all().await?;
            } else {
                sup.clone().start_named(&target_services).await?;
            }

            // Wait for Ctrl+C, SIGTERM (shutdown) or SIGHUP (config reload).
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm = signal(SignalKind::terminate())
                    .unwrap_or_else(|_| signal(SignalKind::terminate()).unwrap());
                let mut sighup = signal(SignalKind::hangup())
                    .unwrap_or_else(|_| signal(SignalKind::hangup()).unwrap());
                loop {
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => break,
                        _ = sigterm.recv() => break,
                        _ = sighup.recv() => {
                            tracing::info!("SIGHUP received — reloading config");
                            if let Err(e) = sup.reload_from_disk().await {
                                tracing::error!("config reload failed: {e}");
                            }
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            tokio::signal::ctrl_c().await.ok();

            println!("\n{} shutting down...", "→".yellow());
            sup.clone().stop_all().await;
            let _ = std::fs::remove_file(&sock);
        }

        Commands::Validate { strict } => {
            let cfg = Arc::new(DevConfig::from_file(&cli.file)?);
            println!(
                "{} A3sfile.hcl is valid ({} services)",
                "✓".green(),
                cfg.service.len(),
            );
            for (name, svc) in &cfg.service {
                let deps = if svc.depends_on.is_empty() {
                    String::new()
                } else {
                    format!(" → depends on: {}", svc.depends_on.join(", "))
                };
                let sub = svc
                    .subdomain
                    .as_deref()
                    .map(|s| format!(" (http://{s}.localhost)"))
                    .unwrap_or_default();
                let port_str = if svc.port == 0 {
                    "auto".to_string()
                } else {
                    svc.port.to_string()
                };
                println!("  {} :{}{}{}", name.cyan(), port_str, sub, deps.dimmed());
            }
            graph::DependencyGraph::from_config(&cfg)?;
            println!("{} dependency graph OK", "✓".green());

            if *strict {
                println!("\n{} strict checks:", "→".cyan());
                let mut all_ok = true;

                // k8s mode: additional checks
                if cfg.dev.runtime == "k8s" {
                    // Check kubectl available
                    if k8s::K8sClient::check_available().await? {
                        println!("  {} kubectl found", "✓".green());
                    } else {
                        println!("  {} kubectl not found on PATH", "✗".red());
                        all_ok = false;
                    }

                    // Check every non-disabled service has k8s.image
                    for (name, svc) in &cfg.service {
                        if svc.disabled {
                            continue;
                        }
                        match &svc.k8s {
                            Some(k) => {
                                println!("  {} [{name}] image: {}", "✓".green(), k.image);
                                // Check dockerfile exists if specified
                                if let Some(ref df) = k.dockerfile {
                                    let path = if df.is_absolute() {
                                        df.clone()
                                    } else {
                                        cli.file
                                            .parent()
                                            .unwrap_or(std::path::Path::new("."))
                                            .join(df)
                                    };
                                    if path.exists() {
                                        println!(
                                            "  {} [{name}] dockerfile found: {}",
                                            "✓".green(),
                                            path.display()
                                        );
                                    } else {
                                        println!(
                                            "  {} [{name}] dockerfile not found: {}",
                                            "✗".red(),
                                            path.display()
                                        );
                                        all_ok = false;
                                    }
                                }
                                // Check Helm chart exists if specified
                                if let Some(ref chart) = k.helm_chart {
                                    let path = if chart.is_absolute() {
                                        chart.clone()
                                    } else {
                                        cli.file
                                            .parent()
                                            .unwrap_or(std::path::Path::new("."))
                                            .join(chart)
                                    };
                                    if path.exists() && path.join("Chart.yaml").exists() {
                                        println!(
                                            "  {} [{name}] Helm chart found: {}",
                                            "✓".green(),
                                            path.display()
                                        );
                                    } else {
                                        println!("  {} [{name}] Helm chart not found or missing Chart.yaml: {}", "✗".red(), path.display());
                                        all_ok = false;
                                    }
                                    if k8s::K8sClient::check_helm_available().await.is_ok() {
                                        println!("  {} helm found", "✓".green());
                                    } else {
                                        println!("  {} helm not found on PATH (required for Helm charts)", "✗".red());
                                        all_ok = false;
                                    }
                                }
                                // Check Kustomize directory exists if specified
                                if let Some(ref kdir) = k.kustomize_dir {
                                    let path = if kdir.is_absolute() {
                                        kdir.clone()
                                    } else {
                                        cli.file
                                            .parent()
                                            .unwrap_or(std::path::Path::new("."))
                                            .join(kdir)
                                    };
                                    if path.exists() && path.join("kustomization.yaml").exists() {
                                        println!(
                                            "  {} [{name}] Kustomize dir found: {}",
                                            "✓".green(),
                                            path.display()
                                        );
                                    } else {
                                        println!("  {} [{name}] Kustomize dir not found or missing kustomization.yaml: {}", "✗".red(), path.display());
                                        all_ok = false;
                                    }
                                }
                                // Check Helm values file exists if specified
                                if let Some(ref values) = k.helm_values {
                                    let path = if values.is_absolute() {
                                        values.clone()
                                    } else {
                                        cli.file
                                            .parent()
                                            .unwrap_or(std::path::Path::new("."))
                                            .join(values)
                                    };
                                    if path.exists() {
                                        println!(
                                            "  {} [{name}] Helm values file found: {}",
                                            "✓".green(),
                                            path.display()
                                        );
                                    } else {
                                        println!(
                                            "  {} [{name}] Helm values file not found: {}",
                                            "✗".red(),
                                            path.display()
                                        );
                                        all_ok = false;
                                    }
                                }
                            }
                            None => {
                                println!(
                                    "  {} [{name}] missing k8s {{ image = \"...\" }} block",
                                    "✗".red()
                                );
                                all_ok = false;
                            }
                        }
                    }

                    // Check cluster is reachable
                    let client = k8s::K8sClient::new(
                        cfg.dev.k8s_context.clone(),
                        cfg.dev.k8s_namespace.clone(),
                    );
                    let mut cmd = tokio::process::Command::new("kubectl");
                    cmd.arg("cluster-info");
                    if let Some(ref ctx) = cfg.dev.k8s_context {
                        cmd.arg("--context").arg(ctx);
                    }
                    cmd.stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                    let _ = client; // suppress unused warning
                    match cmd.status().await {
                        Ok(s) if s.success() => println!("  {} cluster reachable", "✓".green()),
                        _ => {
                            println!(
                                "  {} cluster not reachable (context: {})",
                                "✗".red(),
                                cfg.dev.k8s_context.as_deref().unwrap_or("default")
                            );
                            all_ok = false;
                        }
                    }
                } else {
                    for (name, svc) in &cfg.service {
                        if svc.disabled {
                            continue;
                        }
                        // Check that the binary in `cmd` exists on PATH.
                        let binary = svc.cmd.split_whitespace().next().unwrap_or("");
                        if which_binary(binary) {
                            println!("  {} [{name}] binary '{binary}' found", "✓".green());
                        } else {
                            println!(
                                "  {} [{name}] binary '{}' not found on PATH",
                                "✗".red(),
                                binary
                            );
                            all_ok = false;
                        }
                        // Check that a fixed port is not already bound.
                        if svc.port != 0 {
                            if port_available(svc.port) {
                                println!(
                                    "  {} [{name}] port {} is available",
                                    "✓".green(),
                                    svc.port
                                );
                            } else {
                                println!(
                                    "  {} [{name}] port {} is already in use",
                                    "✗".red(),
                                    svc.port
                                );
                                all_ok = false;
                            }
                        }
                    }
                }

                if all_ok {
                    println!("{} all strict checks passed", "✓".green());
                } else {
                    return Err(DevError::Config("strict validation failed".into()));
                }
            }
        }

        Commands::Top { interval } => {
            // k8s mode: show pod resource usage via kubectl top
            if let Ok(cfg) = DevConfig::from_file(&cli.file) {
                if cfg.dev.runtime == "k8s" {
                    return k8s_top(&cfg, *interval).await;
                }
            }

            // pid -> (total_cpu_ticks, sample_time) — used for delta CPU% on Linux.
            // On macOS we fall back to ps lifetime average.
            let mut prev_ticks: std::collections::HashMap<u32, (u64, std::time::Instant)> =
                std::collections::HashMap::new();

            loop {
                let resp = ipc_send(IpcRequest::Status, &sock).await;
                match resp {
                    Ok(IpcResponse::Status { rows }) => {
                        // Clear screen and move cursor to top-left.
                        print!("\x1b[2J\x1b[H");
                        println!(
                            "{:<16} {:<12} {:<8} {:<10} {}",
                            "SERVICE".bold(),
                            "STATE".bold(),
                            "PID".bold(),
                            "CPU%".bold(),
                            "MEM".bold(),
                        );
                        println!("{}", "─".repeat(56).dimmed());
                        for row in &rows {
                            let state_colored = match row.state.as_str() {
                                "running" => row.state.green().to_string(),
                                "starting" | "restarting" => row.state.yellow().to_string(),
                                "unhealthy" | "failed" => row.state.red().to_string(),
                                _ => row.state.dimmed().to_string(),
                            };
                            let (cpu_str, mem_str) = row
                                .pid
                                .and_then(|pid| query_process_stats_delta(pid, &mut prev_ticks))
                                .map(|(cpu, mem)| (format!("{cpu:.1}%"), format_bytes(mem)))
                                .unwrap_or_else(|| ("-".into(), "-".into()));
                            println!(
                                "{:<16} {:<20} {:<8} {:<10} {}",
                                row.name,
                                state_colored,
                                row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                                cpu_str,
                                mem_str,
                            );
                        }
                        println!(
                            "\n{} refresh every {}s — Ctrl+C to exit",
                            "·".dimmed(),
                            interval
                        );
                    }
                    Err(e) => {
                        eprintln!("{} {e}", "[a3s]".red().bold());
                        break;
                    }
                    _ => break,
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(*interval)) => {}
                }
            }
        }

        Commands::Status {
            json,
            watch,
            interval,
        } => {
            // k8s mode: show pod status via kubectl
            if let Ok(cfg) = DevConfig::from_file(&cli.file) {
                if cfg.dev.runtime == "k8s" {
                    return k8s_status(&cfg, *json).await;
                }
            }

            if *json && *watch {
                return Err(DevError::Config(
                    "--json and --watch are mutually exclusive".into(),
                ));
            }

            if *watch {
                loop {
                    let resp = ipc_send(IpcRequest::Status, &sock).await;
                    match resp {
                        Ok(IpcResponse::Status { rows }) => {
                            print!("\x1b[2J\x1b[H");
                            println!(
                                "{:<16} {:<12} {:<8} {:<6} {:<8} {:<6} {:<24} {}",
                                "SERVICE".bold(),
                                "STATE".bold(),
                                "PID".bold(),
                                "PORT".bold(),
                                "RESTARTS".bold(),
                                "HEALTH".bold(),
                                "URL".bold(),
                                "UPTIME".bold(),
                            );
                            println!("{}", "─".repeat(86).dimmed());
                            for row in rows {
                                let state_colored = match row.state.as_str() {
                                    "running" => row.state.green().to_string(),
                                    "starting" | "restarting" => row.state.yellow().to_string(),
                                    "unhealthy" | "failed" => row.state.red().to_string(),
                                    _ => row.state.dimmed().to_string(),
                                };
                                let url = row
                                    .subdomain
                                    .map(|s| format!("http://{s}.localhost"))
                                    .unwrap_or_default();
                                let uptime = row
                                    .uptime_secs
                                    .map(format_uptime)
                                    .unwrap_or_else(|| "-".into());
                                let restarts = if row.restart_count == 0 {
                                    "-".dimmed().to_string()
                                } else {
                                    row.restart_count.to_string().yellow().to_string()
                                };
                                let health = match row.healthy {
                                    Some(true) => "✓".green().to_string(),
                                    Some(false) => "✗".red().to_string(),
                                    None => "-".dimmed().to_string(),
                                };
                                println!(
                                    "{:<16} {:<20} {:<8} {:<6} {:<16} {:<14} {:<24} {}",
                                    row.name,
                                    state_colored,
                                    row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                                    if row.port == 0 {
                                        "auto".into()
                                    } else {
                                        row.port.to_string()
                                    },
                                    restarts,
                                    health,
                                    url.dimmed(),
                                    uptime.dimmed(),
                                );
                            }
                            println!(
                                "\n{} refresh every {}s — Ctrl+C to exit",
                                "·".dimmed(),
                                interval
                            );
                        }
                        Err(e) => {
                            eprintln!("{} {e}", "[a3s]".red().bold());
                            break;
                        }
                        _ => break,
                    }
                    tokio::select! {
                        _ = tokio::signal::ctrl_c() => break,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(*interval)) => {}
                    }
                }
                return Ok(());
            }

            let resp = ipc_send(IpcRequest::Status, &sock).await?;
            if let IpcResponse::Status { rows } = resp {
                if *json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&rows)
                            .map_err(|e| DevError::Config(format!("json: {e}")))?
                    );
                } else {
                    println!(
                        "{:<16} {:<12} {:<8} {:<6} {:<8} {:<6} {:<24} {}",
                        "SERVICE".bold(),
                        "STATE".bold(),
                        "PID".bold(),
                        "PORT".bold(),
                        "RESTARTS".bold(),
                        "HEALTH".bold(),
                        "URL".bold(),
                        "UPTIME".bold(),
                    );
                    println!("{}", "─".repeat(86).dimmed());
                    for row in rows {
                        let state_colored = match row.state.as_str() {
                            "running" => row.state.green().to_string(),
                            "starting" | "restarting" => row.state.yellow().to_string(),
                            "unhealthy" | "failed" => row.state.red().to_string(),
                            _ => row.state.dimmed().to_string(),
                        };
                        let url = row
                            .subdomain
                            .map(|s| format!("http://{s}.localhost"))
                            .unwrap_or_default();
                        let uptime = row
                            .uptime_secs
                            .map(format_uptime)
                            .unwrap_or_else(|| "-".into());
                        let restarts = if row.restart_count == 0 {
                            "-".dimmed().to_string()
                        } else {
                            row.restart_count.to_string().yellow().to_string()
                        };
                        let health = match row.healthy {
                            Some(true) => "✓".green().to_string(),
                            Some(false) => "✗".red().to_string(),
                            None => "-".dimmed().to_string(),
                        };
                        println!(
                            "{:<16} {:<20} {:<8} {:<6} {:<16} {:<14} {:<24} {}",
                            row.name,
                            state_colored,
                            row.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
                            if row.port == 0 {
                                "auto".into()
                            } else {
                                row.port.to_string()
                            },
                            restarts,
                            health,
                            url.dimmed(),
                            uptime.dimmed(),
                        );
                    }
                }
            }
        }

        Commands::Down { services, label } => {
            // k8s mode: delete resources directly via kubectl
            if let Ok(cfg) = DevConfig::from_file(&cli.file) {
                if cfg.dev.runtime == "k8s" {
                    return k8s_down(&cfg, services, label).await;
                }
            }

            // Determine which services to stop based on explicit names and/or labels
            let mut target_services = services.clone();
            if !label.is_empty() {
                let cfg = DevConfig::from_file(&cli.file)?;
                let labeled = filter_by_labels(&cfg, label);
                if !labeled.is_empty() {
                    target_services.extend(labeled);
                }
            }

            match ipc_send(
                IpcRequest::Stop {
                    services: target_services,
                },
                &sock,
            )
            .await?
            {
                IpcResponse::Stopped { services: stopped } => {
                    if stopped.is_empty() {
                        println!("{} nothing was running", "·".dimmed());
                    } else {
                        for s in &stopped {
                            println!("{} stopped {}", "✓".green(), s.cyan());
                        }
                    }
                }
                _ => println!("{} stopped", "✓".green()),
            }
        }

        Commands::Restart { service } => {
            // k8s mode: rollout restart
            if let Ok(cfg) = DevConfig::from_file(&cli.file) {
                if cfg.dev.runtime == "k8s" {
                    let client = k8s::K8sClient::new(
                        cfg.dev.k8s_context.clone(),
                        cfg.dev.k8s_namespace.clone(),
                    );
                    client.rollout_restart(service).await?;
                    println!("{} restarted {}", "✓".green(), service.cyan());
                    return Ok(());
                }
            }
            ipc_send(
                IpcRequest::Restart {
                    service: service.clone(),
                },
                &sock,
            )
            .await?;
            println!("{} restarted {}", "✓".green(), service.cyan());
        }

        Commands::Reload => match ipc_send(IpcRequest::Reload, &sock).await? {
            IpcResponse::Reloaded {
                started,
                stopped,
                restarted,
            } => {
                println!("{} config reloaded", "✓".green());
                for s in &stopped {
                    println!("  {} stopped  {}", "–".red(), s.dimmed());
                }
                for s in &restarted {
                    println!("  {} restarted {}", "↺".yellow(), s.cyan());
                }
                for s in &started {
                    println!("  {} started  {}", "+".green(), s.cyan());
                }
                if stopped.is_empty() && restarted.is_empty() && started.is_empty() {
                    println!("  no changes");
                }
            }
            IpcResponse::Error { msg } => {
                return Err(DevError::Config(format!("reload failed: {msg}")));
            }
            _ => {
                println!("{} config reloaded", "✓".green());
            }
        },

        Commands::Logs {
            service,
            follow,
            grep,
            last,
            timestamps,
        } => {
            // k8s mode: stream pod logs via kubectl
            if let Ok(cfg) = DevConfig::from_file(&cli.file) {
                if cfg.dev.runtime == "k8s" {
                    return k8s_logs(&cfg, service, *follow, grep.as_deref(), *last).await;
                }
            }

            let services = if service.is_empty() {
                None
            } else {
                Some(service.clone())
            };
            stream_logs(services, *follow, grep.clone(), *last, *timestamps, &sock).await?;
        }

        Commands::PortForward { service, ports } => {
            // k8s mode only
            let cfg = DevConfig::from_file(&cli.file)?;
            if cfg.dev.runtime != "k8s" {
                return Err(DevError::Config(
                    "port-forward is only available in k8s mode (set runtime = \"k8s\" in A3sfile.hcl)".into()
                ));
            }

            // Parse ports: <local-port>:<remote-port>
            let parts: Vec<&str> = ports.split(':').collect();
            if parts.len() != 2 {
                return Err(DevError::Config(format!(
                    "invalid port mapping '{}', expected format: <local-port>:<remote-port>",
                    ports
                )));
            }

            let local_port: u16 = parts[0]
                .parse()
                .map_err(|_| DevError::Config(format!("invalid local port: {}", parts[0])))?;
            let remote_port: u16 = parts[1]
                .parse()
                .map_err(|_| DevError::Config(format!("invalid remote port: {}", parts[1])))?;

            // Check if service exists
            if !cfg.service.contains_key(service) {
                return Err(DevError::Config(format!(
                    "service '{}' not found in A3sfile.hcl",
                    service
                )));
            }

            println!(
                "{} forwarding localhost:{} -> {}:{}",
                "→".cyan(),
                local_port,
                service,
                remote_port
            );
            println!("{} press Ctrl+C to stop", "·".dimmed());

            // Run kubectl port-forward
            let mut cmd = tokio::process::Command::new("kubectl");
            cmd.arg("port-forward")
                .arg(format!("deployment/{}", service))
                .arg(format!("{}:{}", local_port, remote_port))
                .arg("-n")
                .arg(&cfg.dev.k8s_namespace);

            if let Some(ref ctx) = cfg.dev.k8s_context {
                cmd.arg("--context").arg(ctx);
            }

            let mut child = cmd.spawn().map_err(|e| {
                DevError::Config(format!("failed to start kubectl port-forward: {}", e))
            })?;

            // Wait for Ctrl+C
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    println!("\n{} stopping port-forward...", "→".yellow());
                    let _ = child.kill().await;
                }
                status = child.wait() => {
                    match status {
                        Ok(s) if !s.success() => {
                            return Err(DevError::Config("kubectl port-forward failed".into()));
                        }
                        Err(e) => {
                            return Err(DevError::Config(format!("kubectl port-forward error: {}", e)));
                        }
                        _ => {}
                    }
                }
            }
        }

        Commands::Upgrade => {
            let config = a3s_updater::UpdateConfig {
                binary_name: "a3s",
                crate_name: "a3s",
                current_version: env!("CARGO_PKG_VERSION"),
                github_owner: "A3S-Lab",
                github_repo: "Dev",
            };
            a3s_updater::run_update(&config)
                .await
                .map_err(|e| DevError::Config(e.to_string()))?;
        }

        Commands::List => {
            // a3s ecosystem tools
            let tools = [
                ("box", "a3s-box", "A3S-Lab/Box"),
                ("gateway", "a3s-gateway", "A3S-Lab/Gateway"),
                ("power", "a3s-power", "A3S-Lab/Power"),
            ];

            println!(
                "{:<12} {:<16} {}",
                "TOOL".bold(),
                "BINARY".bold(),
                "STATUS".bold()
            );
            println!("{}", "─".repeat(44).dimmed());
            for (alias, binary, _repo) in &tools {
                let installed = which_binary(binary);
                let status = if installed {
                    "installed".green().to_string()
                } else {
                    "not installed".dimmed().to_string()
                };
                println!("{:<12} {:<16} {}", alias, binary, status);
            }
        }

        Commands::Update { tools: filter } => {
            let all_tools = [
                ("box", "a3s-box", "A3S-Lab", "Box"),
                ("gateway", "a3s-gateway", "A3S-Lab", "Gateway"),
                ("power", "a3s-power", "A3S-Lab", "Power"),
                ("a3s", "a3s", "A3S-Lab", "Dev"),
            ];
            let targets: Vec<_> = if filter.is_empty() {
                all_tools.iter().collect()
            } else {
                all_tools
                    .iter()
                    .filter(|(alias, binary, _, _)| {
                        filter.iter().any(|f| f == alias || f == binary)
                    })
                    .collect()
            };
            if targets.is_empty() {
                return Err(DevError::Config(format!(
                    "unknown tool(s): {} — available: box, gateway, power, a3s",
                    filter.join(", ")
                )));
            }
            for (_, binary, owner, repo) in targets {
                let current = if *binary == "a3s" {
                    env!("CARGO_PKG_VERSION")
                } else {
                    "0.0.0"
                };
                if *binary != "a3s" && !which_binary(binary) {
                    println!(
                        "  {} {} not installed, skipping",
                        "·".dimmed(),
                        binary.dimmed()
                    );
                    continue;
                }
                println!("{} updating {}...", "→".cyan(), binary.cyan());
                let config = a3s_updater::UpdateConfig {
                    binary_name: binary,
                    crate_name: binary,
                    current_version: current,
                    github_owner: owner,
                    github_repo: repo,
                };
                match a3s_updater::run_update(&config).await {
                    Ok(_) => println!("{} {} updated", "✓".green(), binary.cyan()),
                    Err(e) => println!("{} {} — {}", "✗".red(), binary.cyan(), e),
                }
            }
        }

        Commands::Tool(args) => {
            let tool = &args[0];
            let rest = &args[1..];
            proxy_tool(tool, rest).await?;
        }

        Commands::Run { service, cmd } => {
            let cfg = DevConfig::from_file(&cli.file)?;
            let mut env: std::collections::HashMap<String, String> = if let Some(svc_name) = service
            {
                let svc = cfg
                    .service
                    .get(svc_name.as_str())
                    .ok_or_else(|| DevError::Config(format!("unknown service '{svc_name}'")))?;
                svc.env.clone()
            } else {
                // Merge all non-disabled services' env (later services win on conflict)
                cfg.service
                    .values()
                    .filter(|s| !s.disabled)
                    .flat_map(|s| s.env.iter().map(|(k, v)| (k.clone(), v.clone())))
                    .collect()
            };

            // Inject runtime ports from daemon if available (best-effort, silent on failure).
            if let Ok(IpcResponse::Status { rows }) = ipc_send(IpcRequest::Status, &sock).await {
                for row in &rows {
                    if row.port != 0 {
                        let key =
                            format!("PORT_{}", row.name.to_uppercase().replace(['-', '.'], "_"));
                        env.entry(key).or_insert_with(|| row.port.to_string());
                    }
                }
            }

            use std::os::unix::process::CommandExt;
            let err = std::process::Command::new(&cmd[0])
                .args(&cmd[1..])
                .envs(&env)
                .exec();
            return Err(DevError::Process {
                service: cmd[0].clone(),
                msg: err.to_string(),
            });
        }

        Commands::Exec { service, cmd } => {
            let cfg = DevConfig::from_file(&cli.file)?;
            let svc = cfg
                .service
                .get(service.as_str())
                .ok_or_else(|| DevError::Config(format!("unknown service '{service}'")))?;
            let mut env = svc.env.clone();

            // Inject runtime ports from daemon if available (best-effort, silent on failure).
            if let Ok(IpcResponse::Status { rows }) = ipc_send(IpcRequest::Status, &sock).await {
                for row in &rows {
                    if row.port != 0 {
                        let key =
                            format!("PORT_{}", row.name.to_uppercase().replace(['-', '.'], "_"));
                        env.entry(key).or_insert_with(|| row.port.to_string());
                    }
                }
            }

            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(&cmd[0]);
            command.args(&cmd[1..]).envs(&env);
            if let Some(dir) = &svc.dir {
                command.current_dir(dir);
            }
            let err = command.exec();
            return Err(DevError::Process {
                service: cmd[0].clone(),
                msg: err.to_string(),
            });
        }
    }

    Ok(())
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}
fn which_binary(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Filter services by labels. Returns service names that match ANY of the given labels.
/// If `labels` is empty, returns all service names.
fn filter_by_labels(cfg: &DevConfig, labels: &[String]) -> Vec<String> {
    if labels.is_empty() {
        return cfg.service.keys().cloned().collect();
    }
    cfg.service
        .iter()
        .filter(|(_, svc)| labels.iter().any(|label| svc.labels.contains(label)))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Known a3s ecosystem tools: alias -> (binary, github_owner, github_repo)
fn ecosystem_tool(alias: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match alias {
        "box" => Some(("a3s-box", "A3S-Lab", "Box")),
        "gateway" => Some(("a3s-gateway", "A3S-Lab", "Gateway")),
        "power" => Some(("a3s-power", "A3S-Lab", "Power")),
        _ => None,
    }
}

/// Proxy a command to an a3s ecosystem tool, auto-installing if missing.
async fn proxy_tool(alias: &str, args: &[String]) -> Result<()> {
    let (binary, owner, repo) = ecosystem_tool(alias).ok_or_else(|| {
        DevError::Config(format!(
            "unknown tool '{alias}' — run `a3s list` to see available tools"
        ))
    })?;

    if !which_binary(binary) {
        println!(
            "{} {} not found — installing from {}/{}...",
            "→".cyan(),
            binary.cyan(),
            owner,
            repo
        );
        let config = a3s_updater::UpdateConfig {
            binary_name: binary,
            crate_name: binary,
            current_version: "0.0.0", // force install
            github_owner: owner,
            github_repo: repo,
        };
        a3s_updater::run_update(&config)
            .await
            .map_err(|e| DevError::Config(format!("failed to install {binary}: {e}")))?;
    }

    // Replace current process with the tool
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new(binary).args(args).exec();
    Err(DevError::Process {
        service: binary.to_string(),
        msg: err.to_string(),
    })
}

/// Poll the daemon via IPC until all services are healthy or the timeout expires.
/// Used by `a3s up --detach --wait`.
async fn wait_for_healthy(sock: &std::path::Path, timeout_secs: u64) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    // Wait for socket to appear (daemon may still be starting)
    loop {
        if sock.exists() {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(DevError::Config(
                "timeout: daemon did not start in time".into(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    loop {
        if std::time::Instant::now() >= deadline {
            return Err(DevError::Config(
                "timeout: services did not become healthy in time".into(),
            ));
        }
        if let Ok(IpcResponse::Status { rows }) = ipc_send(IpcRequest::Status, sock).await {
            if rows.iter().any(|r| r.state == "failed") {
                return Err(DevError::Config(
                    "one or more services failed to start".into(),
                ));
            }
            let all_settled = rows
                .iter()
                .all(|r| matches!(r.state.as_str(), "running" | "stopped" | "failed"));
            if all_settled {
                return Ok(());
            }
        }
        // socket not ready yet or non-Status response — retry
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn ipc_send(req: IpcRequest, sock: &std::path::Path) -> Result<IpcResponse> {
    let stream = UnixStream::connect(sock)
        .await
        .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;

    let (reader, mut writer) = tokio::io::split(stream);
    let line = serde_json::to_string(&req)
        .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?;
    writer.write_all(format!("{line}\n").as_bytes()).await?;

    let mut lines = BufReader::new(reader).lines();
    let resp_line = lines
        .next_line()
        .await?
        .ok_or_else(|| DevError::Config("daemon closed connection".into()))?;

    serde_json::from_str(&resp_line).map_err(|e| DevError::Config(format!("bad IPC response: {e}")))
}

async fn stream_logs(
    services: Option<Vec<String>>,
    follow: bool,
    grep: Option<String>,
    last: usize,
    timestamps: bool,
    sock: &std::path::Path,
) -> Result<()> {
    let print_line = |svc: &str, color_idx: usize, text: String| {
        if grep
            .as_deref()
            .is_none_or(|g| text.to_lowercase().contains(&g.to_lowercase()))
        {
            let prefix = colorize_prefix(&format!("[{svc}]"), color_idx);
            let body = grep
                .as_deref()
                .map(|g| highlight_grep(&text, g))
                .unwrap_or_else(|| text.clone());
            if timestamps {
                let ts = chrono_now();
                println!("{} {} {}", ts.dimmed(), prefix, body);
            } else {
                println!("{} {}", prefix, body);
            }
        }
    };

    let service_list = services.as_deref().unwrap_or(&[]);

    // First replay history
    {
        let stream = UnixStream::connect(sock)
            .await
            .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;
        let (reader, mut writer) = tokio::io::split(stream);
        let req = IpcRequest::History {
            services: service_list.to_vec(),
            lines: last,
        };
        writer
            .write_all(
                format!(
                    "{}\n",
                    serde_json::to_string(&req)
                        .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?
                )
                .as_bytes(),
            )
            .await?;
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(IpcResponse::LogLine {
                service: svc,
                line: text,
                color_idx,
            }) = serde_json::from_str::<IpcResponse>(&line)
            {
                print_line(&svc, color_idx, text);
            }
        }
    }

    if !follow {
        return Ok(());
    }

    // Then stream live, with Ctrl+C support.
    let stream = UnixStream::connect(sock)
        .await
        .map_err(|_| DevError::Config("no running a3s daemon — run `a3s up` first".into()))?;
    let (reader, mut writer) = tokio::io::split(stream);
    let req = IpcRequest::Logs {
        services: service_list.to_vec(),
        follow: true,
    };
    writer
        .write_all(
            format!(
                "{}\n",
                serde_json::to_string(&req)
                    .map_err(|e| DevError::Config(format!("IPC serialize error: {e}")))?
            )
            .as_bytes(),
        )
        .await?;

    let mut lines = BufReader::new(reader).lines();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            result = lines.next_line() => {
                match result {
                    Ok(Some(line)) => {
                        if let Ok(IpcResponse::LogLine { service: svc, line: text, color_idx }) =
                            serde_json::from_str::<IpcResponse>(&line)
                        {
                            print_line(&svc, color_idx, text);
                        }
                    }
                    _ => break,
                }
            }
        }
    }

    Ok(())
}

/// Format current local time as HH:MM:SS using only std.
fn chrono_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // UTC offset not available in std — display UTC time.
    let s = secs % 86400;
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// Check whether a TCP port is available to bind on localhost.
fn port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

/// Deterministically map a service name to one of the log palette colors.
#[allow(dead_code)]
fn service_color(name: &str) -> usize {
    name.bytes().fold(0usize, |acc, b| {
        acc.wrapping_mul(31).wrapping_add(b as usize)
    })
}

/// Apply a color from the log palette to a string by index.
fn colorize_prefix(s: &str, color_idx: usize) -> String {
    use colored::Colorize;
    const COLORS: usize = 8;
    match color_idx % COLORS {
        0 => s.cyan().to_string(),
        1 => s.green().to_string(),
        2 => s.yellow().to_string(),
        3 => s.magenta().to_string(),
        4 => s.blue().to_string(),
        5 => s.bright_cyan().to_string(),
        6 => s.bright_green().to_string(),
        _ => s.bright_yellow().to_string(),
    }
}

/// Highlight all case-insensitive occurrences of `needle` in `haystack` using bold.
fn highlight_grep(haystack: &str, needle: &str) -> String {
    use colored::Colorize;
    if needle.is_empty() {
        return haystack.to_string();
    }
    let lower = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let mut result = String::with_capacity(haystack.len());
    let mut pos = 0;
    while let Some(idx) = lower[pos..].find(&lower_needle) {
        let abs = pos + idx;
        result.push_str(&haystack[pos..abs]);
        result.push_str(&haystack[abs..abs + needle.len()].bold().to_string());
        pos = abs + needle.len();
    }
    result.push_str(&haystack[pos..]);
    result
}

/// Query CPU% and RSS memory (in bytes) for a process by PID using `ps`.
/// Returns `None` if the process is not found or `ps` is unavailable.
fn query_process_stats(pid: u32) -> Option<(f32, u64)> {
    let output = std::process::Command::new("ps")
        .args(["-o", "%cpu=,rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| !l.trim().is_empty())?;
    let mut parts = line.split_whitespace();
    let cpu: f32 = parts.next()?.parse().ok()?;
    let rss_kb: u64 = parts.next()?.parse().ok()?;
    Some((cpu, rss_kb * 1024))
}

/// Read raw CPU ticks for a process from /proc (Linux only).
/// Returns total utime+stime ticks and the system clock tick rate.
#[cfg(target_os = "linux")]
fn read_proc_cpu_ticks(pid: u32) -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Fields are space-separated; utime is field 14 (0-indexed 13), stime is 15.
    // The comm field (2) may contain spaces inside parens, so find the closing ')' first.
    let after_comm = stat.rfind(')')?;
    let fields: Vec<&str> = stat[after_comm + 2..].split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    // SAFETY: sysconf is a standard POSIX call with no side effects.
    let ticks_per_sec = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    Some((utime + stime, ticks_per_sec))
}

/// Delta-based CPU% and RSS. On Linux uses /proc for accurate per-interval CPU.
/// On other platforms falls back to ps lifetime average.
fn query_process_stats_delta(
    pid: u32,
    prev: &mut std::collections::HashMap<u32, (u64, std::time::Instant)>,
) -> Option<(f32, u64)> {
    #[cfg(target_os = "linux")]
    {
        if let Some((ticks, ticks_per_sec)) = read_proc_cpu_ticks(pid) {
            let now = std::time::Instant::now();
            let cpu = if let Some((prev_ticks, prev_time)) = prev.get(&pid) {
                let delta_ticks = ticks.saturating_sub(*prev_ticks) as f32;
                let delta_secs = now.duration_since(*prev_time).as_secs_f32();
                if delta_secs > 0.0 && ticks_per_sec > 0 {
                    (delta_ticks / ticks_per_sec as f32 / delta_secs) * 100.0
                } else {
                    0.0
                }
            } else {
                0.0 // first sample — show 0 until we have a delta
            };
            prev.insert(pid, (ticks, now));
            // Get RSS from ps since /proc/status parsing is verbose
            let rss_kb = query_process_stats(pid).map(|(_, b)| b).unwrap_or(0);
            return Some((cpu, rss_kb));
        }
    }
    // macOS / fallback
    let _ = prev;
    query_process_stats(pid)
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// k8s down: delete all resources for the given services (or all if empty).
async fn k8s_down(cfg: &DevConfig, services: &[String], label: &[String]) -> Result<()> {
    let client = k8s::K8sClient::new(cfg.dev.k8s_context.clone(), cfg.dev.k8s_namespace.clone());

    let mut targets: Vec<String> = if services.is_empty() && label.is_empty() {
        cfg.service.keys().cloned().collect()
    } else {
        services.to_vec()
    };
    if !label.is_empty() {
        targets.extend(filter_by_labels(cfg, label));
    }
    targets.sort();
    targets.dedup();
    targets.retain(|n| cfg.service.contains_key(n));

    if targets.is_empty() {
        println!("{} nothing to delete", "·".dimmed());
        return Ok(());
    }

    for name in &targets {
        print!("{} deleting {}...", "→".cyan(), name);
        let rt = k8s::K8sRuntime::new(
            client.clone(),
            Arc::new({
                let (log, _) = crate::log::LogAggregator::new();
                log
            }),
            None,
        );
        rt.stop_service(name).await?;
        println!(" {}", "done".green());
    }
    Ok(())
}

/// k8s status: show pod status via kubectl get pods.
async fn k8s_status(cfg: &DevConfig, json: bool) -> Result<()> {
    let mut cmd = std::process::Command::new("kubectl");
    cmd.arg("get")
        .arg("pods")
        .arg("-l")
        .arg("managed-by=a3s")
        .arg("-n")
        .arg(&cfg.dev.k8s_namespace);

    if let Some(ref ctx) = cfg.dev.k8s_context {
        cmd.arg("--context").arg(ctx);
    }

    if json {
        cmd.arg("-o").arg("json");
    } else {
        cmd.arg("-o").arg("wide");
    }

    let status = cmd
        .status()
        .map_err(|e| DevError::Config(format!("kubectl get pods failed: {}", e)))?;

    if !status.success() {
        return Err(DevError::Config("kubectl get pods failed".into()));
    }
    Ok(())
}

/// k8s top: show pod CPU/memory usage via kubectl top pods.
async fn k8s_top(cfg: &DevConfig, interval: u64) -> Result<()> {
    loop {
        let mut cmd = tokio::process::Command::new("kubectl");
        cmd.arg("top")
            .arg("pods")
            .arg("-l")
            .arg("managed-by=a3s")
            .arg("-n")
            .arg(&cfg.dev.k8s_namespace)
            .arg("--no-headers");

        if let Some(ref ctx) = cfg.dev.k8s_context {
            cmd.arg("--context").arg(ctx);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| DevError::Config(format!("kubectl top pods failed: {}", e)))?;

        print!("\x1b[2J\x1b[H");
        println!(
            "{:<32} {:<12} {}",
            "POD".bold(),
            "CPU".bold(),
            "MEMORY".bold(),
        );
        println!("{}", "─".repeat(56).dimmed());

        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    let cpu_colored = if parts[1].ends_with('m') {
                        let millis: u64 = parts[1].trim_end_matches('m').parse().unwrap_or(0);
                        if millis > 500 {
                            parts[1].red().to_string()
                        } else if millis > 200 {
                            parts[1].yellow().to_string()
                        } else {
                            parts[1].green().to_string()
                        }
                    } else {
                        parts[1].to_string()
                    };
                    println!("{:<32} {:<20} {}", parts[0], cpu_colored, parts[2]);
                }
            }
            if stdout.trim().is_empty() {
                println!(
                    "{}",
                    "no pods found (is metrics-server installed?)".dimmed()
                );
            }
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            println!("{} {}", "error:".red(), stderr.trim());
            println!(
                "{}",
                "hint: kubectl top requires metrics-server to be installed".dimmed()
            );
        }

        println!(
            "\n{} refresh every {}s — Ctrl+C to exit",
            "·".dimmed(),
            interval
        );

        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {}
        }
    }
    Ok(())
}

/// k8s logs: stream pod logs via kubectl logs.
async fn k8s_logs(
    cfg: &DevConfig,
    services: &[String],
    follow: bool,
    grep: Option<&str>,
    tail: usize,
) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let targets: Vec<String> = if services.is_empty() {
        cfg.service.keys().cloned().collect()
    } else {
        services.to_vec()
    };

    // Spawn one kubectl logs task per service
    let mut handles = vec![];
    for svc_name in targets {
        let mut cmd = tokio::process::Command::new("kubectl");
        cmd.arg("logs")
            .arg("-l")
            .arg(format!("app={}", svc_name))
            .arg("-n")
            .arg(&cfg.dev.k8s_namespace)
            .arg(format!("--tail={}", tail));

        if let Some(ref ctx) = cfg.dev.k8s_context {
            cmd.arg("--context").arg(ctx);
        }
        if follow {
            cmd.arg("-f");
        }

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let grep = grep.map(|s| s.to_lowercase());
        let handle = tokio::spawn(async move {
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("kubectl logs failed: {}", e);
                    return;
                }
            };
            if let Some(stdout) = child.stdout.take() {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(ref g) = grep {
                        if !line.to_lowercase().contains(g.as_str()) {
                            continue;
                        }
                    }
                    println!("{} {}", format!("[{}]", svc_name).cyan(), line);
                }
            }
        });
        handles.push(handle);
    }

    if follow {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async { for h in handles { let _ = h.await; } } => {}
        }
    } else {
        for h in handles {
            let _ = h.await;
        }
    }

    Ok(())
}

/// Run in Kubernetes mode - deploy services to k8s cluster.
async fn run_k8s_mode(
    cfg: Arc<DevConfig>,
    k8s_runtime: k8s::K8sRuntime,
    config_dir: std::path::PathBuf,
    services: Vec<String>,
    label: Vec<String>,
) -> Result<()> {
    // Determine which services to deploy
    let mut target_services: Vec<String> = if services.is_empty() && label.is_empty() {
        cfg.service.keys().cloned().collect()
    } else {
        services.clone()
    };

    // Add services matching labels
    if !label.is_empty() {
        let labeled = filter_by_labels(&cfg, &label);
        target_services.extend(labeled);
    }

    // Remove duplicates and filter disabled services
    target_services.sort();
    target_services.dedup();
    target_services.retain(|name| cfg.service.get(name).map(|s| !s.disabled).unwrap_or(false));

    if target_services.is_empty() {
        return Err(DevError::Config("no services to deploy".into()));
    }

    // Build dependency graph
    let graph = graph::DependencyGraph::from_config(&cfg)?;

    // Get deployment order
    let target_refs: Vec<&str> = target_services.iter().map(|s| s.as_str()).collect();
    let deploy_order = if target_services.len() == cfg.service.len() {
        graph
            .start_order()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        graph.transitive_start_order(&target_refs)
    };

    println!("{} deploying {} services", "→".cyan(), deploy_order.len());

    // Build port map once so initContainers use the correct port for each dependency.
    let service_ports: std::collections::HashMap<String, u16> = cfg
        .service
        .iter()
        .map(|(name, svc)| (name.clone(), svc.port))
        .collect();

    // Deploy services in dependency order
    for svc_name in &deploy_order {
        let svc = cfg
            .service
            .get(svc_name)
            .ok_or_else(|| DevError::Config(format!("service {} not found", svc_name)))?;

        println!("{} {} deploying...", "→".cyan(), svc_name);
        k8s_runtime
            .start_service(svc_name, svc, &config_dir, &service_ports)
            .await?;
        println!("{} {} running", "✓".green(), svc_name);
    }

    // Deploy ingress if any service has subdomain
    k8s_runtime.deploy_ingress(&cfg.service).await?;

    println!(
        "\n{} all services deployed successfully",
        "✓".green().bold()
    );

    // Start file watchers for services with watch config
    let watched: Vec<String> = deploy_order
        .iter()
        .filter(|name| {
            cfg.service
                .get(*name)
                .and_then(|s| s.watch.as_ref())
                .is_some()
        })
        .cloned()
        .collect();

    if watched.is_empty() {
        println!("\nTo view logs:");
        println!(
            "  kubectl logs -l managed-by=a3s -n {} --tail=100 -f",
            cfg.dev.k8s_namespace
        );
        println!("\nTo check status:");
        println!(
            "  kubectl get pods -l managed-by=a3s -n {}",
            cfg.dev.k8s_namespace
        );
        println!("\nTo delete all:");
        println!(
            "  kubectl delete all -l managed-by=a3s -n {}",
            cfg.dev.k8s_namespace
        );
        return Ok(());
    }

    println!(
        "{} watching {} services for changes (Ctrl+C to stop)",
        "→".cyan(),
        watched.len()
    );

    let (watch_tx, mut watch_rx) = tokio::sync::mpsc::channel::<String>(64);
    let k8s_runtime = Arc::new(k8s_runtime);

    // Spawn a watcher per service
    let mut _watcher_stops = vec![];
    for svc_name in &watched {
        let svc = cfg.service.get(svc_name).unwrap();
        let watch_cfg = svc.watch.as_ref().unwrap();
        let paths: Vec<std::path::PathBuf> = watch_cfg
            .paths
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    config_dir.join(p)
                }
            })
            .collect();
        let stop = watcher::spawn_watcher(
            svc_name.clone(),
            paths,
            watch_cfg.ignore.clone(),
            watch_tx.clone(),
        );
        _watcher_stops.push(stop);
    }

    // Handle file change events
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{} shutting down watchers", "→".cyan());
                break;
            }
            Some(svc_name) = watch_rx.recv() => {
                let svc = match cfg.service.get(&svc_name) {
                    Some(s) => s.clone(),
                    None => continue,
                };
                println!("{} {} changed — rebuilding...", "→".cyan(), svc_name);
                let rt = k8s_runtime.clone();
                let config_dir = config_dir.clone();
                tokio::spawn(async move {
                    if let Err(e) = rt.rebuild_and_restart(&svc_name, &svc, &config_dir).await {
                        tracing::error!("[{}] rebuild failed: {}", svc_name, e);
                    } else {
                        println!("{} {} redeployed", "✓".green(), svc_name);
                    }
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_unavailable_when_bound() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!port_available(port));
        drop(listener);
    }

    #[test]
    fn test_format_bytes_kb() {
        assert_eq!(format_bytes(512 * 1024), "512 KB");
    }

    #[test]
    fn test_format_bytes_mb() {
        assert_eq!(format_bytes(10 * 1024 * 1024), "10.0 MB");
    }

    #[test]
    fn test_service_color_deterministic() {
        assert_eq!(service_color("api"), service_color("api"));
        assert_eq!(service_color("db"), service_color("db"));
    }

    #[test]
    fn test_service_color_different_names() {
        // Different names should (almost certainly) map to different indices.
        assert_ne!(service_color("api"), service_color("db"));
        assert_ne!(service_color("web"), service_color("worker"));
    }

    #[test]
    fn test_highlight_grep_empty_needle() {
        assert_eq!(highlight_grep("hello world", ""), "hello world");
    }

    #[test]
    fn test_highlight_grep_no_match() {
        // No match — returned unchanged (no ANSI codes injected).
        let result = highlight_grep("hello world", "xyz");
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_highlight_grep_case_insensitive() {
        colored::control::set_override(true);
        let result = highlight_grep("Hello World", "hello");
        assert!(result.contains("Hello")); // original casing preserved
        assert!(result.len() > "Hello World".len()); // ANSI codes added
        colored::control::unset_override();
    }

    #[test]
    fn test_highlight_grep_multiple_matches() {
        colored::control::set_override(true);
        let result = highlight_grep("foo bar foo", "foo");
        assert!(result.len() > "foo bar foo".len());
        colored::control::unset_override();
    }

    #[test]
    fn test_chrono_now_format() {
        let ts = chrono_now();
        // Should be HH:MM:SS — exactly 8 chars with two colons.
        assert_eq!(ts.len(), 8);
        assert_eq!(&ts[2..3], ":");
        assert_eq!(&ts[5..6], ":");
        let h: u32 = ts[0..2].parse().unwrap();
        let m: u32 = ts[3..5].parse().unwrap();
        let s: u32 = ts[6..8].parse().unwrap();
        assert!(h < 24);
        assert!(m < 60);
        assert!(s < 60);
    }
}
