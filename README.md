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
| `a3s status` | Show service status table |
| `a3s logs [--service name]` | Tail logs (all or one service) |
| `a3s logs --grep <keyword>` | Filter log output by keyword |
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

### Planned 📋

- [ ] SIGHUP config reload — apply changes to `A3sfile.hcl` without a full restart
- [ ] Per-service restart policy — configurable `max_restarts`, `backoff`, and `on_failure` behavior in `A3sfile.hcl`
- [ ] Graceful shutdown timeout — configurable per-service grace period before SIGKILL
- [ ] Test coverage — `supervisor`, `proxy`, `watcher`, `box_mgr` modules have no unit tests

## License

MIT — see [LICENSE](LICENSE).
