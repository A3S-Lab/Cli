#!/usr/bin/env python3
"""
web — minimal HTML frontend that proxies /api/* to the api service.

Environment:
  PORT     bind port (default 3000)
  API_URL  base URL of the api service (default http://localhost:8001)
"""
import http.server, json, os, urllib.parse, urllib.request

API_URL = os.environ.get("API_URL", "http://localhost:8001")

HTML = """\
<!doctype html><html><head><meta charset="utf-8">
<title>a3s demo</title>
<style>body{font-family:sans-serif;max-width:640px;margin:2rem auto}
table{border-collapse:collapse;width:100%}td,th{border:1px solid #ccc;padding:6px 10px}
button{cursor:pointer}</style></head><body>
<h2>a3s demo — items</h2>
<p><button onclick="load()">Refresh</button></p>
<table><thead><tr><th>id</th><th>name</th><th>value</th><th></th></tr></thead>
<tbody id="tb"></tbody></table>
<h3>Add item</h3>
<input id="n" placeholder="name"> <input id="v" placeholder="value">
<button onclick="add()">Add</button>
<script>
async function load(){
  const r=await fetch('/api/items');const d=await r.json();
  document.getElementById('tb').innerHTML=
    (d.items||[]).map(i=>`<tr><td>${i.id}</td><td>${i.name}</td><td>${i.value}</td>
    <td><button onclick="del('${i.id}')">delete</button></td></tr>`).join('');
}
async function add(){
  const n=document.getElementById('n').value,v=document.getElementById('v').value;
  await fetch('/api/items',{method:'POST',headers:{'Content-Type':'application/json'},
    body:JSON.stringify({name:n,value:v})});
  document.getElementById('n').value='';document.getElementById('v').value='';load();
}
async function del(id){await fetch('/api/items/'+id,{method:'DELETE'});load();}
load();
</script></body></html>
"""

def _proxy(url: str, method="GET", body=None, headers=None):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=3) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()

class Handler(http.server.BaseHTTPRequestHandler):
    def _send(self, code, ctype, body: bytes):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        n = int(self.headers.get("Content-Length", 0))
        return self.rfile.read(n) if n else b""

    def do_GET(self):
        p = urllib.parse.urlparse(self.path).path
        if p == "/health":
            self._send(200, "application/json", b'{"ok":true}')
        elif p.startswith("/api/"):
            code, data = _proxy(API_URL + p[4:])
            self._send(code, "application/json", data)
        else:
            self._send(200, "text/html; charset=utf-8", HTML.encode())

    def do_POST(self):
        p = urllib.parse.urlparse(self.path).path
        if p.startswith("/api/"):
            body = self._read_body()
            code, data = _proxy(API_URL + p[4:], method="POST", body=body,
                                 headers={"Content-Type": "application/json"})
            self._send(code, "application/json", data)

    def do_DELETE(self):
        p = urllib.parse.urlparse(self.path).path
        if p.startswith("/api/"):
            code, data = _proxy(API_URL + p[4:], method="DELETE")
            self._send(code, "application/json", data)

    def log_message(self, fmt, *args):
        print(f"[web] {self.address_string()} {fmt % args}", flush=True)

port = int(os.environ.get("PORT", 3000))
print(f"[web] listening on :{port}  api={API_URL}", flush=True)
http.server.HTTPServer(("", port), Handler).serve_forever()
