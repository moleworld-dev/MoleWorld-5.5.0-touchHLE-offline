#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
摩尔庄园 touchHLE WASM 无头运行 harness(wasm-poc 分支专属)。

做三件事:
  1. 起一个线程化 HTTP 服务,服务 debug 产物目录(touchHLE.js/.wasm/.data + index.html + MoleWorld.ipa),
     并接收 index.html POST 过来的 /log(写 /tmp/mole-wasm-log.txt)、/log/reset(清空)。
  2. 用 playwright 无头 Chromium(SwiftShader 软件 WebGL2)加载 index.html 跑游戏 boot。
  3. 等到日志出现终止标志(panic/ABORT)或超时,截图 canvas + 打印日志尾。

用法:
  python3 run-headless.py [--dir <debug目录>] [--timeout 40] [--headed]
依赖:playwright(Python,已装 chromium-1148)。
"""
import argparse, http.server, os, socketserver, sys, threading, time, urllib.parse

LOG_PATH = "/tmp/mole-wasm-log.txt"

DEFAULT_DIR = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..", "..", "fresh-port", "20-touchHLE-src", "touchHLE",
    "target", "wasm32-unknown-emscripten", "debug",
)

CTYPES = {
    ".js": "text/javascript", ".wasm": "application/wasm",
    ".data": "application/octet-stream", ".ipa": "application/octet-stream",
    ".html": "text/html; charset=utf-8", ".mem": "application/octet-stream",
}


def make_handler(root):
    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a):  # 静音
            pass

        def _send(self, code, body=b"", ctype="text/plain"):
            self.send_response(code)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            # emscripten 默认不需要 SAB(无 pthread),但加上无害且利于将来
            self.send_header("Cross-Origin-Opener-Policy", "same-origin")
            self.send_header("Cross-Origin-Embedder-Policy", "require-corp")
            self.end_headers()
            if body:
                self.wfile.write(body)

        def do_POST(self):
            path = urllib.parse.urlparse(self.path).path
            n = int(self.headers.get("Content-Length", 0))
            data = self.rfile.read(n) if n else b""
            if path == "/log/reset":
                open(LOG_PATH, "w").close()
                self._send(200, b"ok")
            elif path == "/log":
                with open(LOG_PATH, "ab") as f:
                    f.write(data + b"\n")
                self._send(200, b"ok")
            else:
                self._send(404)

        def do_GET(self):
            path = urllib.parse.urlparse(self.path).path
            if path == "/" or path == "":
                path = "/index.html"
            fp = os.path.normpath(os.path.join(root, path.lstrip("/")))
            if not fp.startswith(os.path.abspath(root)) or not os.path.isfile(fp):
                self._send(404, b"not found")
                return
            ext = os.path.splitext(fp)[1]
            ctype = CTYPES.get(ext, "application/octet-stream")
            try:
                with open(fp, "rb") as f:
                    body = f.read()
            except OSError:
                self._send(500)
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
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", default=DEFAULT_DIR)
    ap.add_argument("--timeout", type=float, default=45)
    ap.add_argument("--port", type=int, default=8791)
    ap.add_argument("--headed", action="store_true")
    ap.add_argument("--shot", default="/tmp/mole-wasm-shot.png")
    args = ap.parse_args()

    root = os.path.abspath(args.dir)
    if not os.path.isfile(os.path.join(root, "touchHLE.js")):
        print(f"!! 找不到 touchHLE.js 于 {root}", file=sys.stderr)
        sys.exit(2)

    open(LOG_PATH, "w").close()
    httpd = ThreadingHTTP(("127.0.0.1", args.port), make_handler(root))
    t = threading.Thread(target=httpd.serve_forever, daemon=True)
    t.start()
    url = f"http://127.0.0.1:{args.port}/index.html"
    print(f"[harness] 服务 {root}\n[harness] {url}  超时 {args.timeout}s  headed={args.headed}")

    from playwright.sync_api import sync_playwright

    gl_args = [
        "--enable-unsafe-swiftshader",
        "--use-gl=angle",
        "--use-angle=swiftshader",
        "--ignore-gpu-blocklist",
        "--enable-webgl",
        "--disable-gpu-sandbox",
    ]
    console_lines = []
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=not args.headed, args=gl_args)
        page = browser.new_page(viewport={"width": 1100, "height": 820})
        page.on("console", lambda m: console_lines.append(f"[console.{m.type}] {m.text}"))
        page.on("pageerror", lambda e: console_lines.append(f"[pageerror] {e}"))
        # 捕获未处理的 JS 错误/拒绝(含 emscripten abort 抛出的 RuntimeError 的栈)。
        page.add_init_script(
            "window.__jserr=[];"
            "addEventListener('error',e=>{window.__jserr.push('ERROR: '+((e.error&&e.error.stack)||e.message||String(e)))});"
            "addEventListener('unhandledrejection',e=>{window.__jserr.push('REJECT: '+((e.reason&&e.reason.stack)||String(e.reason)))});"
        )
        page.goto(url, wait_until="commit")

        deadline = time.time() + args.timeout
        terminal = ("Rust panic", "ABORT:", "abort(", "RuntimeError", "thread 'main' panicked")
        last_size = -1
        while time.time() < deadline:
            time.sleep(1.0)
            try:
                with open(LOG_PATH) as f:
                    txt = f.read()
            except OSError:
                txt = ""
            if len(txt) != last_size:
                last_size = len(txt)
            if any(k in txt for k in terminal):
                print("[harness] 检测到终止标志,提前结束")
                time.sleep(2.0)
                break
        try:
            jserr = page.evaluate("window.__jserr || []")
            if jserr:
                print("\n===== 未处理 JS 错误(含 abort 栈)=====")
                for e in jserr[:6]:
                    print(e)
                print("=====================================\n")
        except Exception as e:
            print(f"[harness] 取 JS 错误失败: {e}")
        try:
            page.screenshot(path=args.shot)
            print(f"[harness] 截图 -> {args.shot}")
        except Exception as e:
            print(f"[harness] 截图失败: {e}")
        browser.close()
    httpd.shutdown()

    print("\n===== console (浏览器 JS 侧) 末尾 =====")
    for l in console_lines[-25:]:
        print(l)
    print("\n===== /tmp/mole-wasm-log.txt 行数 =====")
    try:
        with open(LOG_PATH) as f:
            n = sum(1 for _ in f)
        print(f"{n} 行(完整内容用 Read 看 {LOG_PATH})")
    except OSError:
        print("无日志")


if __name__ == "__main__":
    main()
