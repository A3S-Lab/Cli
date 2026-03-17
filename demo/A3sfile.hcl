dev {
  proxy_port = 7080
  # env("LOG_LEVEL", "info") — override with LOG_LEVEL=debug a3s up
  log_level  = env("LOG_LEVEL", "info")
}

# ── store: in-memory key-value HTTP service ────────────────────────────────
service "store" {
  cmd    = "python3 store.py"
  port   = 6380
  labels = ["infra"]

  env = {
    APP_ENV = env("APP_ENV", "development")
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }

  restart {
    max_restarts = 5
    backoff      = "1s"
    max_backoff  = "10s"
  }

  log_file = "logs/store.log"
}

# ── api: REST backend, depends on store ────────────────────────────────────
service "api" {
  cmd        = "python3 api.py"
  port       = 8001
  depends_on = ["store"]
  labels     = ["backend"]

  env = {
    STORE_URL = "http://localhost:${store.port}"
    APP_ENV   = env("APP_ENV", "development")
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }

  restart {
    max_restarts = 5
    backoff      = "1s"
    max_backoff  = "10s"
  }

  log_file = "logs/api.log"
}

# ── worker: background heartbeat writer, depends on store ──────────────────
service "worker" {
  cmd        = "python3 worker.py"
  port       = 8002
  depends_on = ["store"]
  labels     = ["backend"]

  env = {
    STORE_URL = "http://localhost:${store.port}"
    # env("WORKER_INTERVAL", "3") — override with WORKER_INTERVAL=<n> a3s up
    INTERVAL  = env("WORKER_INTERVAL", "3")
    APP_ENV   = env("APP_ENV", "development")
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }

  restart {
    max_restarts = 5
    backoff      = "1s"
    max_backoff  = "10s"
  }

  log_file = "logs/worker.log"
}

# ── web: HTML frontend, depends on api ─────────────────────────────────────
service "web" {
  cmd        = "python3 web.py"
  port       = 3000
  depends_on = ["api"]
  labels     = ["frontend"]

  env = {
    API_URL = "http://localhost:${api.port}"
    APP_ENV = env("APP_ENV", "development")
  }

  health {
    type     = "http"
    path     = "/health"
    interval = "2s"
    timeout  = "1s"
    retries  = 5
  }

  log_file = "logs/web.log"
}

# ── gateway: unified reverse proxy, depends on all backends ────────────────
#
# Exposes a single entrypoint on :8080:
#   /          → web  (HTML frontend)
#   /api/…     → api  (REST, strip prefix, rate-limited)
#   /worker/…  → worker (status, strip prefix)
#   /store/…   → store  (KV, strip prefix, api-key required)
#
# Routes are defined in gateway.hcl alongside this file.
service "gateway" {
  cmd        = "a3s-gateway --config gateway.hcl"
  port       = 8080
  depends_on = ["store", "api", "worker", "web"]
  labels     = ["infra"]

  env = {
    # Passed to a3s-gateway as an OS env var so gateway.hcl can reference
    # it via ${STORE_API_KEY}.  Override with STORE_API_KEY=<secret> a3s up
    STORE_API_KEY = env("STORE_API_KEY", "demo-store-secret")
    APP_ENV       = env("APP_ENV", "development")
  }

  health {
    type     = "http"
    path     = "/api/gateway/health"
    interval = "3s"
    timeout  = "2s"
    retries  = 5
  }

  restart {
    max_restarts = 3
    backoff      = "2s"
    max_backoff  = "15s"
  }

  log_file = "logs/gateway.log"
}

# ── env_override: switch to alternate ports for CI ────────────────────────
env_override "ci" {
  service "store"  { env = { PORT = "16380" } }
  service "api"    { env = { PORT = "18001" } }
  service "worker" { env = { PORT = "18002", INTERVAL = "1" } }
  service "web"    { env = { PORT = "13000" } }
}
