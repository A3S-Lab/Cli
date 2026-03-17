#!/usr/bin/env python3
"""
api — REST API backed by the store service.

Environment:
  PORT      bind port (default 8001)
  STORE_URL base URL of the store service (default http://localhost:6380)

Endpoints:
  GET  /health           → {"ok": true, "store": <store-health>}
  GET  /items            → {"items": [{"id": ..., "name": ..., "value": ...}]}
  POST /items            → body: {"name": n, "value": v} → {"id": ..., "name": n, "value": v}
  GET  /items/<id>       → {"id": ..., "name": ..., "value": ...}
  DELETE /items/<id>     → {"ok": true}
"""
import http.server, json, os, urllib.parse, urllib.request, uuid

STORE_URL = os.environ.get("STORE_URL", "http://localhost:6380")

def store_get(key: str):
    try:
        with urllib.request.urlopen(f"{STORE_URL}/get?key={urllib.parse.quote(key)}", timeout=2) as r:
            return json.loads(r.read()).get("value")
    except Exception:
        return None

def store_set(key: str, value: str):
    body = json.dumps({"key": key, "value": value}).encode()
    req  = urllib.request.Request(f"{STORE_URL}/set", data=body,
                                   headers={"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=2):
        pass

def store_del(key: str):
    req = urllib.request.Request(
        f"{STORE_URL}/del?key={urllib.parse.quote(key)}", method="DELETE")
    with urllib.request.urlopen(req, timeout=2):
        pass

def store_keys():
    try:
        with urllib.request.urlopen(f"{STORE_URL}/keys", timeout=2) as r:
            return json.loads(r.read()).get("keys", [])
    except Exception:
        return []

def store_health() -> bool:
    try:
        with urllib.request.urlopen(f"{STORE_URL}/health", timeout=2) as r:
            return json.loads(r.read()).get("ok", False)
    except Exception:
        return False

class Handler(http.server.BaseHTTPRequestHandler):
    def _json(self, code: int, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _read_body(self):
        length = int(self.headers.get("Content-Length", 0))
        return json.loads(self.rfile.read(length) or b"{}")

    def do_GET(self):
        path = urllib.parse.urlparse(self.path).path.rstrip("/")
        if path == "/health":
            self._json(200, {"ok": True, "store": store_health()})
        elif path == "/items":
            keys  = [k for k in store_keys() if k.startswith("item:")]
            items = []
            for k in keys:
                raw = store_get(k)
                if raw:
                    try:
                        items.append(json.loads(raw))
                    except Exception:
                        pass
            self._json(200, {"items": items})
        elif path.startswith("/items/"):
            item_id = path[len("/items/"):]
            raw = store_get(f"item:{item_id}")
            if raw:
                self._json(200, json.loads(raw))
            else:
                self._json(404, {"error": "item not found"})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        path = urllib.parse.urlparse(self.path).path.rstrip("/")
        if path == "/items":
            body  = self._read_body()
            name  = body.get("name", "")
            value = body.get("value", "")
            if not name:
                self._json(400, {"error": "name required"})
                return
            item_id = str(uuid.uuid4())[:8]
            item    = {"id": item_id, "name": name, "value": value}
            store_set(f"item:{item_id}", json.dumps(item))
            self._json(201, item)
        else:
            self._json(404, {"error": "not found"})

    def do_DELETE(self):
        path = urllib.parse.urlparse(self.path).path.rstrip("/")
        if path.startswith("/items/"):
            item_id = path[len("/items/"):]
            raw = store_get(f"item:{item_id}")
            if raw:
                store_del(f"item:{item_id}")
                self._json(200, {"ok": True})
            else:
                self._json(404, {"error": "item not found"})
        else:
            self._json(404, {"error": "not found"})

    def log_message(self, fmt, *args):
        print(f"[api] {self.address_string()} {fmt % args}", flush=True)

port = int(os.environ.get("PORT", 8001))
print(f"[api] listening on :{port}  store={STORE_URL}", flush=True)
http.server.HTTPServer(("", port), Handler).serve_forever()
