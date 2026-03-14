# a3s

Local development orchestration tool for the A3S monorepo — and a unified CLI for the entire A3S ecosystem.

## What it does

`a3s` is a single binary that replaces the need to juggle multiple terminals and process managers when working on A3S projects. It:

- Starts and supervises multiple services defined in `A3sfile.hcl`
- Restarts crashed services automatically with exponential backoff
- Watches source files and hot-restarts services on change
- Routes services to subdomains via a local reverse proxy
- Proxies to A3S ecosystem tools (`a3s box`, `a3s gateway`, etc.), auto-installing them if missing
- Provides a web UI for real-time service and container monitoring

## Install

```bash
brew install a3s-lab/tap/a3s
```

Or build from source (requires the UI to be built first):

```bash
cd src/ui && npm install && npm run build
cargo build --release
```

Or via `just`:

```bash
just ui-install
just build-ui
just build
```

## Quick start

```bash
# Create an A3sfile.hcl in the current directory
a3s init

# Edit A3sfile.hcl, then start all services
a3s up

# Start in background
a3s up --detach

# Check status
a3s status

# Tail logs
a3s logs
a3s logs --service api

# Stop everything
a3s down
```

## A3sfile.hcl

```hcl
dev {
  proxy_port = 7080
  log_level  = "info"
}

service "api" {
  cmd        = "cargo run -p my-api"
  dir        = "./services/api"
  port       = 3000
  subdomain  = "api"
  depends_on = ["db"]

  env = {
    DATABASE_URL = "postgres://localhost:5432/dev"
  }

  watch {
    paths   = ["./services/api/src"]
    ignore  = ["target"]
    restart = true
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }
}

service "db" {
  cmd  = "postgres -D /usr/local/var/postgresql@16"
  port = 5432

  health {
    type    = "tcp"
    timeout = "1s"
    retries = 10
  }
}
```

## Commands

### Service orchestration

| Command | Description |
|---------|-------------|
| `a3s up [services]` | Start all (or named) services in dependency order |
| `a3s up --detach` | Start as background daemon |
| `a3s down [services]` | Stop all (or named) services |
| `a3s restart <service>` | Restart a service |
| `a3s reload` | Reload A3sfile.hcl without restarting unchanged services |
| `a3s status` | Show service status table |
| `a3s logs [--service name]` | Tail logs (all or one service) |
| `a3s logs --grep <keyword>` | Filter log output by keyword |
| `a3s logs --last N` | Show last N lines of history (default: 200) |
| `a3s run <cmd>` | Run a one-off command with env merged from all services |
| `a3s run --service <name> <cmd>` | Run with env from a specific service |
| `a3s exec <service> -- <cmd>` | Run a command in a service's working directory and env |
| `a3s validate` | Validate A3sfile.hcl without starting anything |
| `a3s init` | Generate a new A3sfile.hcl |

### A3S ecosystem tools

`a3s` acts as a unified entry point for all A3S tools. If a tool is not installed, it is downloaded automatically from GitHub Releases.

```bash
a3s box run ubuntu:24.04 -- bash
a3s box ps
a3s gateway --help
```

| Command | Description |
|---------|-------------|
| `a3s list` | List installed A3S ecosystem tools |
| `a3s update [tools]` | Update ecosystem tools (all if no names given) |
| `a3s upgrade` | Upgrade the `a3s` binary itself |

## Web UI

When running `a3s up`, a web UI is available at `http://localhost:10350` by default.

- **Services tab** — real-time status, log stream, per-service restart/stop buttons, resizable sidebar
- **Box tab** — container, image, network, and volume management for `a3s-box`

Disable the UI with `--no-ui`. Change the port with `--ui-port <port>`.

## Proxy routing

Services with a `subdomain` field are reachable at `http://<subdomain>.localhost:<proxy_port>`.
The proxy runs on port `7080` by default and is configured in the `dev {}` block.

## Configuration reference

```hcl
dev {
  proxy_port = 7080      # Local reverse proxy port (default: 7080)
  log_level  = "info"    # Log level: trace, debug, info, warn, error
}

service "<name>" {
  cmd        = "..."     # Shell command to run (required)
  dir        = "."       # Working directory (default: A3sfile.hcl directory)
  port       = 3000      # Port the service listens on (0 = auto-assign)
  subdomain  = "api"     # Proxy subdomain: http://<subdomain>.localhost (optional)
  depends_on = ["db"]    # Services to start before this one (optional)
  disabled   = false     # Skip this service entirely (optional)

  env = {                # Environment variables (optional)
    KEY = "value"
  }

  env_file = ".env"      # Load variables from a .env file (optional)
                         # Variables in `env` take precedence over env_file
  log_file = "logs/api.log"  # Append stdout/stderr to this file (optional)
                             # Relative to A3sfile.hcl directory

  watch {                # Restart on file change (optional)
    paths   = ["./src"]
    ignore  = ["target", "node_modules"]
    restart = true
  }

  health {               # Health check before unblocking dependents (optional)
    type     = "http"    # http or tcp
    path     = "/health" # HTTP path (http only)
    interval = "2s"      # Check interval (default: 2s)
    timeout  = "1s"      # Per-check timeout (default: 1s)
    retries  = 5         # Retries before giving up (default: 3)
  }

  stop_timeout = "10s"   # Grace period before SIGKILL (default: 5s)

  restart {              # Crash-recovery policy (optional)
    max_restarts = 10    # Max restarts before giving up (default: 10)
    backoff      = "1s"  # Initial backoff delay (default: 1s, exponential)
    max_backoff  = "30s" # Maximum backoff delay (default: 30s)
    on_failure   = "restart"  # "restart" (default) or "stop"
  }
}
```

## Development

```bash
# Install UI dependencies
just ui-install

# Build the React UI (required before cargo build)
just build-ui

# Run tests
just test

# Check + lint
just check

# Format
just fmt
```

## Roadmap

### Done ✅

- [x] `A3sfile.hcl` config parsing — `service`, `dev` blocks, `env`/`env_file`, `watch`, `health`, `depends_on`, `disabled`
- [x] Dependency graph — topological start/stop order, cycle detection
- [x] Process supervisor — start, stop, restart with SIGTERM + kill fallback
- [x] Crash recovery — exponential backoff, max 10 restarts, `failed` state
- [x] File watcher — per-service hot restart on source change, ignore patterns, debounce
- [x] Health checks — HTTP and TCP, configurable interval/timeout/retries
- [x] Log aggregator — per-service color output, ring-buffer history (last 200 lines), live broadcast
- [x] Reverse proxy — subdomain routing (`http://<name>.localhost:<port>`)
- [x] IPC daemon — Unix socket, JSON-Lines protocol for `status`/`stop`/`restart`/`logs`/`history`
- [x] Web UI — services view, kube view, box view; SSE log streaming; sidebar resize; restart/stop buttons
- [x] `a3s up --detach` — background daemon mode
- [x] `a3s logs --grep` — keyword filtering for log stream
- [x] `a3s run` / `a3s exec` — one-off commands with service environment
- [x] `a3s validate` — config validation without starting anything
- [x] Ecosystem tool proxy — auto-install `a3s-box`, `a3s-gateway`, `a3s-power` from GitHub Releases
- [x] `a3s upgrade` / `a3s update` — self-update and ecosystem tool updates
- [x] Port `0` — auto-assign a free port at startup; preserved across restarts
- [x] `disabled` services — skipped at start, excluded from dependency validation
- [x] **Ongoing health monitoring** — continuous background health check loop; 3 consecutive failures → `unhealthy` state + SIGTERM + crash-recovery restart; recovers to `running` on success; monitor re-armed after each crash-recovery restart
- [x] **File watcher `watcher_stop` leak fixed** — watcher stop sender is now propagated to restarted service handles; `stop_service()` correctly cancels the OS watcher after file-watcher-triggered restarts

- [x] **SIGHUP config reload** — send `SIGHUP` to the daemon to reload `A3sfile.hcl`; stops removed/disabled services, restarts changed services, starts new services, unchanged services keep running
- [x] **Per-service restart policy** — `restart {}` block with `max_restarts` (default 10), `backoff` (default 1s), `max_backoff` (default 30s), `on_failure = "restart"|"stop"`; exponential backoff; `on_failure = "stop"` leaves service stopped after crash
- [x] **Graceful shutdown timeout** — `stop_timeout` field (default 5s); SIGTERM sent first, SIGKILL after timeout
- [x] **Test coverage** — unit tests added for `config`, `graph`, `proxy`, `watcher` modules (64 tests total)
- [x] **Parallel service startup** — services with no inter-dependencies start concurrently within each dependency wave; serial correctness is preserved (wave N starts only after wave N-1 completes)
- [x] **Selective startup with dep resolution** — `a3s up api` automatically starts `db` (and any other transitive deps) in dependency order before `api`
- [x] **`a3s logs --last N`** — configurable history line count (default 200); e.g. `a3s logs --last 50`
- [x] **Supervisor unit tests** — lifecycle tests for start, stop, restart, start_all, start_named (78 tests total)
- [x] **Process group killing** — services are spawned in their own process group; SIGTERM/-SIGKILL are sent to the entire group so wrapper commands (`npm run dev`, `cargo watch`) kill all child processes, not just the wrapper
- [x] **`a3s reload`** — sends a reload request via IPC; equivalent to `kill -HUP` without needing the daemon PID; stops removed/disabled services, restarts changed, starts new
- [x] **`a3s down <services>` stops dependents first** — `a3s down db` automatically stops `api` (and anything else that depends on db) in safe order before stopping db
- [x] **`log_file` config option** — `log_file = "logs/api.log"` in a service block writes stdout/stderr to disk (append mode, relative to A3sfile.hcl directory); 81 tests total

## License

MIT — see [LICENSE](LICENSE).
