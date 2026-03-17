# gateway.hcl — demo reverse proxy for the a3s demo stack
#
# Single entrypoint on :8080 routes to all four services:
#
#   /          → web  (frontend, HTML)
#   /api/…     → api  (REST CRUD — strip /api prefix, rate-limited)
#   /worker/…  → worker (background status — strip /worker prefix)
#   /store/…   → store  (KV service — strip /store prefix, api-key required)
#
# All services sit on their fixed local ports (6380 / 8001 / 8002 / 3000).
# The gateway exposes a unified entry on :8080 so callers never need to know
# individual service ports.

entrypoints "web" {
  address = "0.0.0.0:8080"
}

# ── Routers (higher priority wins when multiple rules match) ──────────────

# /api/* → api service  (priority 20 — more specific than catch-all)
routers "api" {
  rule        = "PathPrefix(`/api`)"
  service     = "api-backend"
  middlewares = ["strip-api", "rate-limit", "cors"]
  priority    = 20
}

# /worker/* → worker service
routers "worker" {
  rule        = "PathPrefix(`/worker`)"
  service     = "worker-backend"
  middlewares = ["strip-worker", "cors"]
  priority    = 20
}

# /store/* → store service  (internal — require API key)
routers "store" {
  rule        = "PathPrefix(`/store`)"
  service     = "store-backend"
  middlewares = ["strip-store", "store-api-key"]
  priority    = 20
}

# / → web frontend  (lowest priority catch-all)
routers "web" {
  rule     = "PathPrefix(`/`)"
  service  = "web-backend"
  priority = 1
}

# ── Services (backend pools with active health checks) ───────────────────

services "web-backend" {
  load_balancer {
    strategy = "round-robin"
    servers  = [{ url = "http://127.0.0.1:3000" }]
    health_check {
      path     = "/health"
      interval = "5s"
    }
  }
}

services "api-backend" {
  load_balancer {
    strategy = "round-robin"
    servers  = [{ url = "http://127.0.0.1:8001" }]
    health_check {
      path     = "/health"
      interval = "5s"
    }
  }
}

services "worker-backend" {
  load_balancer {
    strategy = "round-robin"
    servers  = [{ url = "http://127.0.0.1:8002" }]
    health_check {
      path     = "/health"
      interval = "5s"
    }
  }
}

services "store-backend" {
  load_balancer {
    strategy = "round-robin"
    servers  = [{ url = "http://127.0.0.1:6380" }]
    health_check {
      path     = "/health"
      interval = "5s"
    }
  }
}

# ── Middlewares ───────────────────────────────────────────────────────────

# Strip route prefixes before forwarding to backends
middlewares "strip-api"    { type = "strip-prefix"; prefixes = ["/api"] }
middlewares "strip-worker" { type = "strip-prefix"; prefixes = ["/worker"] }
middlewares "strip-store"  { type = "strip-prefix"; prefixes = ["/store"] }

# Rate limiting for API routes (100 req/s, burst 20)
middlewares "rate-limit" {
  type  = "rate-limit"
  rate  = 100
  burst = 20
}

# CORS for API and worker routes
middlewares "cors" {
  type            = "cors"
  allowed_origins = ["*"]
  allowed_methods = ["GET", "POST", "DELETE", "OPTIONS"]
}

# API-key protection for the store route (internal use only).
# The key value comes from the STORE_API_KEY OS env var injected by
# A3sfile.hcl: env("STORE_API_KEY", "demo-store-secret")
middlewares "store-api-key" {
  type   = "api-key"
  header = "X-Store-Key"
  keys   = ["${STORE_API_KEY}"]
}
