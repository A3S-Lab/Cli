use std::sync::Arc;

use tokio::process::{Child, Command};

use crate::config::ServiceDef;
use crate::error::{DevError, Result};
use crate::log::LogAggregator;

/// Everything needed to spawn a service process.
pub struct SpawnSpec<'a> {
    pub name: &'a str,
    pub svc: &'a ServiceDef,
    pub port: u16,
    pub color_idx: usize,
    /// Directory containing A3sfile.hcl — used to resolve relative `log_file` paths.
    pub config_dir: &'a std::path::Path,
    /// Runtime mode: "local" or "box".
    pub runtime: &'a str,
}

pub struct SpawnResult {
    pub child: Child,
    pub pid: u32,
}

/// Run a hook command (in the service working directory).
/// Returns an error if the hook exits non-zero.
pub(super) async fn run_hook(hook: &str, svc: &ServiceDef, label: &str) -> Result<()> {
    let parts = split_cmd(hook);
    let program = parts.first().map(|s| s.as_str()).unwrap_or("sh");
    let mut cmd = Command::new(program);
    cmd.args(&parts[1..]).envs(&svc.env);
    if let Some(dir) = &svc.dir {
        cmd.current_dir(dir);
    }
    let status = cmd.status().await.map_err(|e| DevError::Process {
        service: label.to_string(),
        msg: format!("{label} hook: {e}"),
    })?;
    if !status.success() {
        return Err(DevError::Process {
            service: label.to_string(),
            msg: format!("{label} hook exited with {status}"),
        });
    }
    Ok(())
}

/// Auto-detect a container image from the service command.
/// Returns a best-guess base image. Services with an explicit `box.image` override this.
fn box_image(svc: &ServiceDef) -> &'static str {
    if let Some(ref b) = svc.r#box {
        if b.image.is_some() {
            return ""; // caller handles the Some case
        }
    }
    let cmd = svc.cmd.trim();
    if cmd.starts_with("python3") || cmd.starts_with("python") {
        "python:3.12-slim"
    } else if cmd.starts_with("node") || cmd.starts_with("npx") {
        "node:20-alpine"
    } else if cmd.starts_with("bun") || cmd.starts_with("bunx") {
        "oven/bun:latest"
    } else if cmd.starts_with("deno") {
        "denoland/deno:latest"
    } else if cmd.starts_with("ruby") {
        "ruby:3.3-slim"
    } else if cmd.starts_with("php") {
        "php:8.3-cli-alpine"
    } else if cmd.starts_with("go ") || cmd.starts_with("go\t") {
        "golang:1.22-alpine"
    } else {
        "ubuntu:24.04"
    }
}

/// Build the `a3s-box run` argument list that wraps the service command.
fn build_box_args(spec: &SpawnSpec<'_>, workdir: &str) -> Vec<String> {
    let image = if let Some(ref b) = spec.svc.r#box {
        if let Some(ref img) = b.image {
            img.as_str().to_owned()
        } else {
            box_image(spec.svc).to_owned()
        }
    } else {
        box_image(spec.svc).to_owned()
    };

    let mut args: Vec<String> = vec![
        "run".into(),
        "--rm".into(),
        "--name".into(),
        format!("a3s-{}", spec.name),
        "-p".into(),
        format!("{}:{}", spec.port, spec.port),
        "-e".into(),
        format!("PORT={}", spec.port),
        "-e".into(),
        "HOST=0.0.0.0".into(),
        "-v".into(),
        format!("{workdir}:/workspace"),
        "-w".into(),
        "/workspace".into(),
    ];
    for (k, v) in &spec.svc.env {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push(image);
    // append the original command tokens
    args.extend(split_cmd(&spec.svc.cmd));
    args
}

/// Spawn a service process, attach stdout to the log aggregator, and return the child.
/// Stderr is forwarded to the log aggregator as well.
pub async fn spawn_process(spec: &SpawnSpec<'_>, log: &Arc<LogAggregator>) -> Result<SpawnResult> {
    // Run pre_start hook before launching the service process.
    if let Some(ref hook) = spec.svc.pre_start {
        tracing::info!("[{}] running pre_start: {hook}", spec.name);
        run_hook(hook, spec.svc, spec.name).await?;
    }

    // Build program + args depending on runtime.
    let (program, args, extra_args) = if spec.runtime == "box" {
        // Remove any stale container with the same name before starting.
        let container_name = format!("a3s-{}", spec.name);
        tokio::process::Command::new("a3s-box")
            .args(["rm", "-f", &container_name])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .ok();
        let workdir = spec
            .svc
            .dir
            .as_ref()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|| spec.config_dir.to_string_lossy().into_owned());
        let box_args = build_box_args(spec, &workdir);
        let prog = "a3s-box".to_owned();
        (prog, box_args, vec![])
    } else {
        let parts = split_cmd(&spec.svc.cmd);
        let prog = parts.first().map(|s| s.as_str()).unwrap_or("sh").to_owned();
        let extra = framework_port_args(&parts, spec.port);
        (prog, parts[1..].to_vec(), extra)
    };

    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .args(&extra_args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // For local mode, inject envs and PORT/HOST directly; box mode injects them via CLI flags.
    if spec.runtime != "box" {
        cmd.envs(&spec.svc.env)
            .env("PORT", spec.port.to_string())
            .env("HOST", "127.0.0.1");
    }

    if let Some(dir) = &spec.svc.dir {
        cmd.current_dir(dir);
    }

    // Put the child in its own process group so SIGTERM/-SIGKILL reaches all
    // descendant processes (e.g. `npm run dev` spawning node).
    #[cfg(unix)]
    cmd.process_group(0);

    let mut child = cmd.spawn().map_err(|e| DevError::Process {
        service: spec.name.to_string(),
        msg: e.to_string(),
    })?;

    let pid = child.id().unwrap_or(0);

    if let Some(stdout) = child.stdout.take() {
        log.attach(spec.name.to_string(), spec.color_idx, stdout);
    }

    if let Some(stderr) = child.stderr.take() {
        log.attach_stderr(spec.name.to_string(), spec.color_idx, stderr);
    }

    // Tee logs to file if configured
    if let Some(log_path) = &spec.svc.log_file {
        let resolved = if log_path.is_absolute() {
            log_path.clone()
        } else {
            spec.config_dir.join(log_path)
        };
        log.register_log_file(
            spec.name.to_string(),
            resolved,
            spec.svc.log_rotate_mb * 1024 * 1024,
        );
    }

    Ok(SpawnResult { child, pid })
}

/// Bind to port 0 and return the OS-assigned free port.
///
/// Note: there is an inherent TOCTOU race between dropping the listener and
/// the service process binding to the returned port. In practice the window is
/// a few microseconds and the OS ephemeral-port pool is not immediately
/// recycled, so collisions are vanishingly rare for a local dev tool.
pub fn free_port() -> Option<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Shell-style command splitting: handles single/double quotes and backslash escapes.
/// e.g. `node server.js --title 'hello world'` → ["node", "server.js", "--title", "hello world"]
pub fn split_cmd(cmd: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = cmd.chars().peekable();
    let mut in_single = false;
    let mut in_double = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Detect framework from the command and inject `--port <port>` if needed.
/// `parts` is the full split command (program + args).
pub fn framework_port_args(parts: &[String], port: u16) -> Vec<String> {
    let p = port.to_string();
    let direct = [
        "vite",
        "next",
        "astro",
        "nuxt",
        "remix",
        "svelte-kit",
        "wrangler",
    ];
    let runners = ["npx", "pnpm", "yarn", "bunx"];

    let program = parts.first().map(|s| s.as_str()).unwrap_or("");
    let second = parts.get(1).map(|s| s.as_str()).unwrap_or("");

    let framework = if direct.contains(&program) {
        program
    } else if runners.contains(&program) {
        if second == "exec" || second == "run" || second == "dlx" {
            parts.get(2).map(|s| s.as_str()).unwrap_or("")
        } else {
            second
        }
    } else {
        ""
    };

    if direct.contains(&framework) {
        vec!["--port".into(), p]
    } else {
        vec![]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_cmd_simple() {
        assert_eq!(split_cmd("node server.js"), vec!["node", "server.js"]);
    }

    #[test]
    fn test_split_cmd_single_quotes() {
        assert_eq!(
            split_cmd("node server.js --title 'hello world'"),
            vec!["node", "server.js", "--title", "hello world"]
        );
    }

    #[test]
    fn test_split_cmd_double_quotes() {
        assert_eq!(
            split_cmd(r#"echo "hello world""#),
            vec!["echo", "hello world"]
        );
    }

    #[test]
    fn test_split_cmd_backslash() {
        assert_eq!(split_cmd(r"echo hello\ world"), vec!["echo", "hello world"]);
    }

    #[test]
    fn test_framework_port_args_direct() {
        let parts = vec!["vite".to_string()];
        assert_eq!(framework_port_args(&parts, 3000), vec!["--port", "3000"]);
    }

    #[test]
    fn test_framework_port_args_npx() {
        let parts = vec!["npx".to_string(), "vite".to_string()];
        assert_eq!(framework_port_args(&parts, 3000), vec!["--port", "3000"]);
    }

    #[test]
    fn test_framework_port_args_pnpm_exec() {
        let parts = vec!["pnpm".to_string(), "exec".to_string(), "next".to_string()];
        assert_eq!(framework_port_args(&parts, 3000), vec!["--port", "3000"]);
    }

    #[test]
    fn test_framework_port_args_unknown() {
        let parts = vec!["node".to_string(), "server.js".to_string()];
        assert_eq!(framework_port_args(&parts, 3000), Vec::<String>::new());
    }
}
