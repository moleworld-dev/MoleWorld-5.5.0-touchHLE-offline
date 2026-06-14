#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""摩尔庄园 wasm 启动用的极简静态服务器。
用法:python3 serve.py <port> <目录>
- 给 .wasm 正确的 application/wasm,关缓存(改完重 build 刷新即生效)。
- 带 COOP/COEP(与无头 harness 一致,稳妥;无线程其实也能不带)。
"""
import http.server, os, socketserver, sys, urllib.parse

CTYPES = {
    ".js": "text/javascript", ".wasm": "application/wasm",
    ".data": "application/octet-stream", ".ipa": "application/octet-stream",
    ".html": "text/html; charset=utf-8", ".mem": "application/octet-stream",
}


def make_handler(root):
    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, fmt, *a):
            sys.stderr.write("  " + (fmt % a) + "\n")

        def do_GET(self):
            path = urllib.parse.urlparse(self.path).path
            if path in ("/", ""):
                path = "/index.html"
            fp = os.path.normpath(os.path.join(root, path.lstrip("/")))
            if not fp.startswith(os.path.abspath(root)) or not os.path.isfile(fp):
                self.send_response(404)
                self.end_headers()
                self.wfile.write(b"not found")
                return
            ctype = CTYPES.get(os.path.splitext(fp)[1], "application/octet-stream")
            try:
                with open(fp, "rb") as f:
                    body = f.read()
            except OSError:
                self.send_response(500)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
            self.send_header("Cross-Origin-Resource-Policy", "cross-origin")
            self.end_headers()
            self.wfile.write(body)
    return H


class ThreadingHTTP(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8800
    root = os.path.abspath(sys.argv[2]) if len(sys.argv) > 2 else os.getcwd()
    httpd = ThreadingHTTP(("127.0.0.1", port), make_handler(root))
    print(f"[serve] {root}\n[serve] http://127.0.0.1:{port}/  (Ctrl-C 停止)", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        print("\n[serve] 停止")


if __name__ == "__main__":
    main()
