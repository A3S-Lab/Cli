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
# Create an A3sfile.hcl in the current directory, then start all services
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
  labels     = ["backend", "critical"]

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
  cmd    = "postgres -D /usr/local/var/postgresql@16"
  port   = 5432
  labels = ["backend", "critical"]

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
| `a3s up --label <label>` | Start services with specific label (can be repeated) |
| `a3s up --env <name>` | Apply a named `env_override` block (e.g., `--env staging`) |
| `a3s up --detach` | Start as background daemon |
| `a3s up --detach --wait` | Start daemon, block until all services healthy |
| `a3s down [services]` | Stop all (or named) services |
| `a3s down --label <label>` | Stop services with specific label (can be repeated) |
| `a3s restart <service>` | Restart a service |
| `a3s reload` | Reload A3sfile.hcl without restarting unchanged services |
| `a3s status` / `a3s ps` | Show service status table |
| `a3s status --json` | Machine-readable JSON status |
| `a3s logs [--service name]` | Tail logs (all or one service, repeatable) |
| `a3s logs --grep <keyword>` | Filter log output by keyword |
| `a3s logs --last N` | Show last N lines of history (default: 200) |
| `a3s run <cmd>` | Run a one-off command with env merged from all services |
| `a3s run --service <name> <cmd>` | Run with env from a specific service |
| `a3s exec <service> -- <cmd>` | Run a command in a service's working directory and env |
| `a3s validate` | Validate A3sfile.hcl without starting anything |
| `a3s validate --strict` | Also check binaries exist on PATH and ports are free |
| `a3s top [--interval N]` | Live CPU% and memory view per service (default: 2s refresh); in k8s mode shows Pod CPU/memory via `kubectl top` |
| `a3s port-forward <service> <local>:<remote>` | Forward local port to service in k8s cluster (k8s mode only, e.g., `a3s port-forward api 8080:3000`) |

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

A `.env` file in the same directory as `A3sfile.hcl` is automatically loaded and applied as the
lowest-priority env source for every service. Variables in `env` and `env_file` take precedence.

`${VAR}` placeholders in `cmd`, `env` values, and hook commands are expanded from OS environment
variables at startup. Unknown variables are left as `${VAR}`.

```hcl
dev {
  proxy_port     = 7080      # Local reverse proxy port (default: 7080)
  log_level      = "info"    # Log level: trace, debug, info, warn, error
  runtime        = "local"   # Runtime mode: "local" (default) or "k8s"
  k8s_context    = "orbstack" # kubectl context (k8s mode only, optional)
  k8s_namespace  = "dev"     # Kubernetes namespace (k8s mode only, default: "default")
  registry       = "localhost:5000" # Container registry for k8s mode (optional, e.g., "localhost:5000")
  https          = true      # Enable HTTPS for reverse proxy (generates self-signed cert in .a3s/)
}

service "<name>" {
  cmd        = "..."     # Shell command to run (required)
  dir        = "."       # Working directory (default: A3sfile.hcl directory)
  port       = 3000      # Port the service listens on (0 = auto-assign)
  subdomain  = "api"     # Proxy subdomain: http://<subdomain>.localhost (optional)
  depends_on = ["db"]    # Services to start before this one (optional)
  disabled   = false     # Skip this service entirely (optional)
  labels     = ["backend", "critical"]  # Labels for grouping and filtering (optional)

  env = {                # Environment variables (optional)
    KEY = "value"
  }

  env_file = ".env"      # Load variables from a .env file (optional)
                         # Variables in `env` take precedence over env_file
  log_file = "logs/api.log"  # Append stdout/stderr to this file (optional)
                             # Relative to A3sfile.hcl directory

  pre_start = "migrate db"   # Shell command to run before starting (optional)
                             # Non-zero exit aborts startup
  post_stop = "cleanup.sh"   # Shell command to run after stopping (optional)

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

  # Kubernetes-specific config (only used when runtime = "k8s")
  k8s {
    image      = "node:20-alpine"  # Container image (required in k8s mode)
    dockerfile = "./Dockerfile"    # Path to Dockerfile for building (optional)
    replicas   = 1                 # Number of replicas (default: 1)

    resources {                    # Resource requests/limits (optional)
      cpu_request    = "100m"
      cpu_limit      = "500m"
      memory_request = "128Mi"
      memory_limit   = "512Mi"
    }

    # Helm chart support (optional) — uses `helm template` instead of generating manifests
    helm_chart  = "./charts/myapp"  # Path to Helm chart directory
    helm_values = "./values.yaml"   # Path to Helm values file (optional)

    # Kustomize support (optional) — uses `kubectl kustomize` instead of generating manifests
    kustomize_dir = "./k8s/overlays/dev"  # Path to Kustomize directory

    # Secret support (optional) — stored as Kubernetes Secret, injected as env vars
    secret_file = ".env.secret"    # Path to .env-format file with sensitive values
    secrets = {                    # Or inline key-value pairs (secret_file takes precedence)
      API_KEY     = "my-secret-key"
      DB_PASSWORD = "hunter2"
    }

    # Volume mounts (optional) — mount local directories, emptyDir, configMap, or secret
    volumes = [
      {
        name       = "code"
        type       = "hostPath"      # hostPath, emptyDir, configMap, secret
        host_path  = "./src"         # Required for hostPath (relative to A3sfile.hcl)
        mount_path = "/app/src"      # Mount path in container (required)
        read_only  = false           # Optional, default: false
      },
      {
        name       = "data"
        type       = "emptyDir"
        mount_path = "/data"
      }
    ]
  }
}

# Named environment overrides — apply with `a3s up --env <name>`
# Merges env variables on top of the base service env (override wins).
env_override "staging" {
  service "api" {
    env = {
      DATABASE_URL = "postgres://staging-db:5432/app"
    }
  }
}
```

### env() function

`env("VAR_NAME")` and `env("VAR_NAME", "default")` can be used anywhere a string value is expected in `A3sfile.hcl`. They are expanded before HCL parsing.

```hcl
service "api" {
  cmd = env("API_CMD", "node server.js")
  env = {
    DATABASE_URL = env("DATABASE_URL", "postgres://localhost:5432/dev")
    SECRET_KEY   = env("SECRET_KEY")
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
- [x] **`log_file` config option** — `log_file = "logs/api.log"` in a service block writes stdout/stderr to disk (append mode, relative to A3sfile.hcl directory)
- [x] **Project isolation** — socket path is derived from a djb2 hash of the canonical project directory; two projects on the same machine get distinct sockets and never interfere
- [x] **Parallel stop** — `stop_service` no longer holds the write lock across the async SIGTERM wait; `stop_all` stops each reverse-dependency wave concurrently (symmetric with parallel start)
- [x] **`a3s up --detach --wait`** — blocks until all services reach `running` state; polls IPC every 500 ms; `--wait-timeout N` (default 60 s); exits non-zero if any service `failed`
- [x] **`a3s ps`** — alias for `a3s status`
- [x] **`a3s status --json`** — machine-readable JSON output for scripts and monitoring; 83 tests total
- [x] **Global `.env` auto-discovery** — a `.env` file in the same directory as `A3sfile.hcl` is automatically loaded as the lowest-priority env source for all services (below per-service `env` and `env_file`)
- [x] **`pre_start` / `post_stop` hooks** — optional shell commands run before a service starts (abort on non-zero exit) and after it stops; run in the service's working directory with its environment
- [x] **Env var interpolation** — `${VAR}` placeholders in `cmd`, `env` values, and hook commands are replaced with OS environment variable values at config load time; unknown variables are preserved as-is; 93 tests total
- [x] **`a3s validate --strict`** — additionally checks that every service's binary exists on `PATH` and that fixed ports are not already bound; exits non-zero if any check fails
- [x] **`a3s top`** — live CPU% and RSS memory view per service, polling the running daemon every 2 seconds (configurable with `--interval`); reads stats via `ps`; in k8s mode shows Pod CPU/memory via `kubectl top pods`; 135 tests total
- [x] **Service labels** — `labels = ["backend", "critical"]` in a service block; `a3s up --label backend` starts only matching services (with deps); `a3s down --label critical` stops matching services; `a3s logs --service` accepts multiple values; 111 tests total
- [x] **`env()` function in A3sfile.hcl** — `env("VAR")` and `env("VAR", "default")` expand OS environment variables directly in HCL source before parsing; works in any string field (`cmd`, `env`, `pre_start`, etc.); 121 tests total
- [x] **`env_override` blocks + `a3s up --env <name>`** — named environment override blocks in `A3sfile.hcl`; `a3s up --env staging` merges the matching block's per-service env on top of the base config; enables dev/staging/prod switching without separate config files
- [x] **`runtime = "k8s"` mode** — set `runtime = "k8s"` in the `dev {}` block to deploy services to a local Kubernetes cluster (OrbStack, Docker Desktop, etc.) instead of running as local processes; generates Deployment, Service, ConfigMap, and Ingress manifests from `A3sfile.hcl`; respects `depends_on` (initContainers), `health` (liveness/readiness probes), `env` (ConfigMap), `subdomain` (Ingress rules), and `k8s {}` block for image, replicas, and resource limits
- [x] **k8s image build** — when `k8s.dockerfile` is set, `a3s up` automatically runs `docker build -t <image> -f <dockerfile>` before deploying; build output is streamed to the log aggregator in real-time
- [x] **k8s file watch → rebuild → rollout restart** — `watch {}` blocks in k8s mode trigger `docker build` + `kubectl rollout restart deployment/<name>` on file changes instead of process restart; debounced, concurrent per-service
- [x] **k8s `a3s down`** — deletes Deployment, Service, and ConfigMap resources for named services (or all if no names given); respects `--label` filtering
- [x] **k8s `a3s status`** — shows pod status via `kubectl get pods -l managed-by=a3s`; supports `--json` for machine-readable output
- [x] **k8s `a3s logs`** — streams pod logs via `kubectl logs -l app=<name>`; supports `--follow`, `--grep`, `--last`, multiple `--service` flags; concurrent multi-service output
- [x] **k8s `a3s restart`** — triggers `kubectl rollout restart deployment/<name>` instead of SIGTERM
- [x] **k8s `a3s validate --strict`** — checks kubectl availability, image/dockerfile configuration, and Dockerfile existence for all services with `k8s {}` blocks
- [x] **k8s local registry push** — set `registry = "localhost:5000"` in the `dev {}` block to automatically tag and push built images to a local registry before deploying; build and push output streamed to logs
- [x] **k8s `a3s top`** — shows Pod CPU and memory usage via `kubectl top pods -l managed-by=a3s`; requires metrics-server to be installed in the cluster; color-coded CPU usage (green < 200m, yellow < 500m, red >= 500m)
- [x] **k8s Helm/Kustomize support** — set `helm_chart` or `kustomize_dir` in the `k8s {}` block to use existing Helm charts or Kustomize overlays instead of generating manifests; `helm template` and `kubectl kustomize` are called automatically; `a3s validate --strict` checks for Chart.yaml/kustomization.yaml existence and helm availability
- [x] **k8s Secret support** — set `secret_file = ".env.secret"` or `secrets = { KEY = "value" }` in the `k8s {}` block to inject sensitive configuration as Kubernetes Secrets (base64-encoded, injected as environment variables via `envFrom.secretRef`); secrets are automatically deployed and deleted with the service
- [x] **k8s Volume mounts** — set `volumes = [{ name, type, mount_path, ... }]` in the `k8s {}` block to mount volumes into containers; supports `hostPath` (local directories for hot-reload), `emptyDir` (temporary storage), `configMap`, and `secret`; hostPath paths are relative to A3sfile.hcl directory and automatically resolved to absolute paths
- [x] **k8s `a3s port-forward`** — forward local port to a service in the k8s cluster via `a3s port-forward <service> <local-port>:<remote-port>`; wraps `kubectl port-forward deployment/<name>`; runs in foreground until Ctrl+C; k8s mode only
- [x] **HTTPS support** — set `https = true` in the `dev {}` block to enable HTTPS for the reverse proxy; automatically generates self-signed certificate (stored in `.a3s/cert.pem` and `.a3s/key.pem`); access services via `https://api.localhost:7080` instead of `http://`; certificate includes `*.localhost` SAN for all subdomains

## License

MIT — see [LICENSE](LICENSE).
