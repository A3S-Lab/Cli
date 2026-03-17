dev {
  proxy_port = 7080
  log_level  = "info"
}

# ── store: in-memory key-value HTTP service ────────────────────────────────
service "store" {
  cmd    = "python3 store.py"
  port   = 6380
  labels = ["infra"]

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
    INTERVAL  = "3"
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

# ── web: HTML frontend, depends on api ────────────────────────────────────
service "web" {
  cmd        = "python3 web.py"
  port       = 3000
  depends_on = ["api"]
  labels     = ["frontend"]

  env = {
    API_URL = "http://localhost:${api.port}"
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

# ── env_override example: switch to alternate ports for CI ────────────────
env_override "ci" {
  service "store"  { env = { PORT = "16380" } }
  service "api"    { env = { PORT = "18001" } }
  service "worker" { env = { PORT = "18002", INTERVAL = "1" } }
  service "web"    { env = { PORT = "13000" } }
}
