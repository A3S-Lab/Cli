#!/usr/bin/env python3
"""
store — in-memory key-value store over HTTP.

Endpoints:
  GET  /health          → {"ok": true}
  GET  /keys            → {"keys": [...]}
  GET  /get?key=<k>     → {"key": k, "value": v}  or 404
  POST /set             → body: {"key": k, "value": v}  → {"ok": true}
  DELETE /del?key=<k>   → {"ok": true}
"""
import http.server, json, os, threading, urllib.parse

_lock  = threading.Lock()
_store: dict[str, str] = {}

class Handler(http.server.BaseHTTPRequestHandler):
    def _json(self, code: int, body: dict):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        parsed = urllib.parse.urlparse(self.path)
        params = dict(urllib.parse.parse_qsl(parsed.query))
        if parsed.path == "/health":
            self._json(200, {"ok": True, "keys": len(_store)})
        elif parsed.path == "/keys":
            with _lock:
                self._json(200, {"keys": list(_store.keys())})
        elif parsed.path == "/get":
            key = params.get("key", "")
            with _lock:
                if key in _store:
                    self._json(200, {"key": key, "value": _store[key]})
                else:
                    self._json(404, {"error": f"key not found: {key}"})
        else:
            self._json(404, {"error": "not found"})

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body   = json.loads(self.rfile.read(length) or b"{}")
        if self.path == "/set":
            key, value = body.get("key", ""), body.get("value", "")
            if not key:
                self._json(400, {"error": "key required"})
                return
            with _lock:
                _store[key] = value
            self._json(200, {"ok": True})
        else:
            self._json(404, {"error": "not found"})

    def do_DELETE(self):
        parsed = urllib.parse.urlparse(self.path)
        params = dict(urllib.parse.parse_qsl(parsed.query))
        if parsed.path == "/del":
            key = params.get("key", "")
            with _lock:
                existed = _store.pop(key, None) is not None
            self._json(200, {"ok": existed})
        else:
            self._json(404, {"error": "not found"})

    def log_message(self, fmt, *args):
        print(f"[store] {self.address_string()} {fmt % args}", flush=True)

port = int(os.environ.get("PORT", 6380))
print(f"[store] listening on :{port}", flush=True)
http.server.HTTPServer(("", port), Handler).serve_forever()
