#!/usr/bin/env python3
"""
worker — background job processor.

Writes a heartbeat to the store every INTERVAL seconds and exposes
its own status over HTTP so the health checker can probe it.

Environment:
  PORT       bind port (default 8002)
  STORE_URL  base URL of the store service (default http://localhost:6380)
  INTERVAL   heartbeat interval in seconds (default 5)

Endpoints:
  GET /health   → {"ok": true}
  GET /status   → {"beats": N, "last_beat": "<iso>", "interval": N}
"""
import datetime, http.server, json, os, threading, time, urllib.request

STORE_URL = os.environ.get("STORE_URL", "http://localhost:6380")
INTERVAL  = int(os.environ.get("INTERVAL", 5))

_state = {"beats": 0, "last_beat": None}
_lock  = threading.Lock()

def _store_set(key: str, value: str):
    body = json.dumps({"key": key, "value": value}).encode()
    req  = urllib.request.Request(f"{STORE_URL}/set", data=body,
                                   headers={"Content-Type": "application/json"}, method="POST")
    urllib.request.urlopen(req, timeout=2)

def _beat_loop():
    while True:
        try:
            now  = datetime.datetime.utcnow().isoformat() + "Z"
            with _lock:
                _state["beats"] += 1
                _state["last_beat"] = now
                count = _state["beats"]
            _store_set("worker:last_beat", now)
            _store_set("worker:beat_count", str(count))
            print(f"[worker] beat #{count} at {now}", flush=True)
        except Exception as e:
            print(f"[worker] beat failed: {e}", flush=True)
        time.sleep(INTERVAL)

class Handler(http.server.BaseHTTPRequestHandler):
    def _json(self, code: int, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if self.path == "/health":
            self._json(200, {"ok": True})
        elif self.path == "/status":
            with _lock:
                self._json(200, {
                    "beats":     _state["beats"],
                    "last_beat": _state["last_beat"],
                    "interval":  INTERVAL,
                    "store_url": STORE_URL,
                })
        else:
            self._json(404, {"error": "not found"})

    def log_message(self, fmt, *args):
        print(f"[worker] {self.address_string()} {fmt % args}", flush=True)

# Start beat loop in background thread
threading.Thread(target=_beat_loop, daemon=True).start()

port = int(os.environ.get("PORT", 8002))
print(f"[worker] listening on :{port}  store={STORE_URL}  interval={INTERVAL}s", flush=True)
http.server.HTTPServer(("", port), Handler).serve_forever()
