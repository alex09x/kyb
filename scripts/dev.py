#!/usr/bin/env python3
"""
KYB Local Development Server (0-second iteration)
Serves src/web/index.html directly from disk with live-reload,
while proxying all API requests to the upstream KYB backend.
"""

import os
import sys
import time
import json
import socket
import argparse
from http.server import HTTPServer, BaseHTTPRequestHandler
import urllib.request
import urllib.error

REPO_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HTML_PATH = os.path.join(REPO_DIR, "src", "web", "index.html")

def get_default_backend():
    # 1. ~/.config/kyb/host
    try:
        with open(os.path.expanduser("~/.config/kyb/host")) as f:
            h = f.read().strip()
            if h:
                return f"http://{h}:9310"
    except Exception:
        pass
    # 2. scripts/fleet.local.sh
    try:
        with open(os.path.join(REPO_DIR, "scripts", "fleet.local.sh")) as f:
            for line in f:
                if line.startswith("SERVER="):
                    val = line.split("=", 1)[1].strip().strip('"').strip("'")
                    host = val.split("@")[-1]
                    return f"http://{host}:9310"
    except Exception:
        pass
    return "http://127.0.0.1:9310"

LIVE_RELOAD_SCRIPT = b"""
<script>
// Live reload for KYB local dev server
(function() {
  let lastMtime = 0;
  setInterval(async () => {
    try {
      const res = await fetch('/__dev_mtime');
      const data = await res.json();
      if (lastMtime && data.mtime > lastMtime) {
        console.log('[KYB Dev] src/web/index.html changed, reloading page...');
        window.location.reload();
      }
      lastMtime = data.mtime;
    } catch (_) {}
  }, 400);
})();
</script>
"""

class DevProxyHandler(BaseHTTPRequestHandler):
    backend_url = "http://127.0.0.1:9310"
    html_path = HTML_PATH

    def do_GET(self):
        path = self.path.split("?")[0]
        if path in ("/", "/index.html"):
            self.serve_html()
        elif path == "/__dev_mtime":
            self.serve_mtime()
        else:
            self.proxy_request("GET")

    def do_POST(self):
        self.proxy_request("POST")

    def do_PUT(self):
        self.proxy_request("PUT")

    def do_DELETE(self):
        self.proxy_request("DELETE")

    def do_PATCH(self):
        self.proxy_request("PATCH")

    def serve_html(self):
        try:
            with open(self.html_path, "rb") as f:
                content = f.read()
            if b"</body>" in content:
                content = content.replace(b"</body>", LIVE_RELOAD_SCRIPT + b"</body>")
            else:
                content = content + LIVE_RELOAD_SCRIPT

            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Cache-Control", "no-cache, no-store, must-revalidate")
            self.send_header("Content-Length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)
        except Exception as e:
            self.send_error(500, f"Error reading {self.html_path}: {e}")

    def serve_mtime(self):
        try:
            mtime = os.path.getmtime(self.html_path)
            data = json.dumps({"mtime": mtime}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
        except Exception as e:
            self.send_error(500, str(e))

    def proxy_request(self, method):
        target_url = f"{self.backend_url}{self.path}"
        content_length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(content_length) if content_length > 0 else None

        headers = {}
        for k, v in self.headers.items():
            if k.lower() not in ("host", "content-length"):
                headers[k] = v

        req = urllib.request.Request(target_url, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(req, timeout=15) as resp:
                resp_body = resp.read()
                self.send_response(resp.status)
                for hk, hv in resp.getheaders():
                    if hk.lower() not in ("transfer-encoding", "content-encoding", "content-length"):
                        self.send_header(hk, hv)
                self.send_header("Content-Length", str(len(resp_body)))
                self.end_headers()
                self.wfile.write(resp_body)
        except urllib.error.HTTPError as e:
            resp_body = e.read()
            self.send_response(e.code)
            for hk, hv in e.headers.items():
                if hk.lower() not in ("transfer-encoding", "content-encoding", "content-length"):
                    self.send_header(hk, hv)
            self.send_header("Content-Length", str(len(resp_body)))
            self.end_headers()
            self.wfile.write(resp_body)
        except Exception as e:
            err_msg = json.dumps({"error": f"Proxy error connecting to {self.backend_url}: {e}"}).encode("utf-8")
            self.send_response(502)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(err_msg)))
            self.end_headers()
            self.wfile.write(err_msg)

    def log_message(self, format, *args):
        # Calm console logging: only log mutations and errors, silence live-reload polls
        msg = format % args
        if "/__dev_mtime" in msg:
            return
        sys.stderr.write(f"[dev] {msg}\n")

def main():
    parser = argparse.ArgumentParser(description="KYB Local Dev Server")
    parser.add_argument("--port", "-p", type=int, default=9311, help="Local port (default: 9311)")
    parser.add_argument("--backend", "-b", type=str, default=None, help="Backend KYB URL")
    parser.add_argument("--file", "-f", type=str, default=HTML_PATH, help="Path to index.html")
    args = parser.parse_args()

    backend = args.backend or get_default_backend()
    DevProxyHandler.backend_url = backend.rstrip("/")
    DevProxyHandler.html_path = args.file

    server_address = ("127.0.0.1", args.port)
    try:
        httpd = HTTPServer(server_address, DevProxyHandler)
    except OSError as e:
        print(f"Error binding to 127.0.0.1:{args.port}: {e}")
        print("Try another port with --port <PORT>")
        sys.exit(1)

    print("=" * 60)
    print("  🚀 KYB Local Development Server Running")
    print("=" * 60)
    print(f"  • Local UI:      http://localhost:{args.port}/")
    print(f"  • Source file:   {args.file}")
    print(f"  • API Backend:   {DevProxyHandler.backend_url}")
    print(f"  • Live Reload:   Enabled (edits to index.html auto-reload)")
    print("=" * 60)
    print("Press Ctrl+C to stop.\n")

    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\nDev server stopped.")

if __name__ == "__main__":
    main()
