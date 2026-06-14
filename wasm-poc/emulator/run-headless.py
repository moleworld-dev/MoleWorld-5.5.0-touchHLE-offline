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
    ap.add_argument("--periodic-shots", action="store_true")
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

    if args.headed:
        # 有头 + 真 GPU(Mac 的 Metal/ANGLE):WebGL canvas 能被正确合成/截图。
        gl_args = ["--ignore-gpu-blocklist", "--enable-webgl"]
    else:
        # 无头:用 SwiftShader 软 WebGL2(注意:其 framebuffer 内容截图常抓不到,
        # 用 present.rs 的 glReadPixels 日志确认渲染)。
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
        # 强制 preserveDrawingBuffer=true,这样 playwright 截图能抓到 WebGL canvas 内容
        # (默认 false 时截图永远是黑的,即便游戏其实在正常渲染)。仅调试用,不改游戏代码。
        page.add_init_script(
            "(function(){const o=HTMLCanvasElement.prototype.getContext;"
            "HTMLCanvasElement.prototype.getContext=function(t,a){"
            "if(t==='webgl2'||t==='webgl'||t==='experimental-webgl'){a=Object.assign({},a,{preserveDrawingBuffer:true,alpha:false});}"
            "return o.call(this,t,a);};})();"
        )
        page.goto(url, wait_until="commit")

        deadline = time.time() + args.timeout
        terminal = ("Rust panic", "ABORT:", "abort(", "RuntimeError", "thread 'main' panicked")
        last_size = -1
        shot_n = 0
        next_shot = time.time() + 3
        while time.time() < deadline:
            time.sleep(1.0)
            # 周期截图,观察渲染随时间的变化(定位黑屏发生点)。
            if args.periodic_shots and time.time() >= next_shot:
                try:
                    p = f"/tmp/mole-wasm-shot-{shot_n}.png"
                    page.screenshot(path=p)
                    print(f"[harness] 周期截图 -> {p}")
                    shot_n += 1
                except Exception:
                    pass
                next_shot = time.time() + 3
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
            info = page.evaluate(
                "(()=>{const c=document.getElementById('canvas');"
                "let attrs=null;try{const g=c.getContext('webgl2')||c.getContext('webgl');"
                "attrs=g&&g.getContextAttributes();}catch(e){}"
                "return {w:c&&c.width,h:c&&c.height,cssw:c&&c.clientWidth,cssh:c&&c.clientHeight,"
                "preserve:attrs&&attrs.preserveDrawingBuffer};})()"
            )
            print(f"[harness] canvas backing={info.get('w')}x{info.get('h')} "
                  f"css={info.get('cssw')}x{info.get('cssh')} preserveDrawingBuffer={info.get('preserve')}")
        except Exception as e:
            print(f"[harness] canvas info 失败: {e}")
        # 解释器吞吐(MIPS):play.html 把每秒读数收进 window.__mips。打印全序列 + 峰值/中位。
        try:
            mips = page.evaluate("window.__mips || []") or []
            if mips:
                srt = sorted(mips)
                peak = srt[-1]
                med = srt[len(srt) // 2]
                print(f"[harness] MIPS 读数({len(mips)} 个/秒): " +
                      " ".join(f"{m:.1f}" for m in mips))
                print(f"[harness] MIPS 峰值={peak:.1f}  中位={med:.1f}")
            else:
                print("[harness] 无 MIPS 读数(游戏未进入 CPU 稳态?)")
        except Exception as e:
            print(f"[harness] 取 MIPS 失败: {e}")
        # 从 emscripten MEMFS 读 /frame.ppm(present.rs 写的真实渲染帧),存成 PNG。
        try:
            b64 = page.evaluate(
                "(()=>{try{const d=Module.FS.readFile('/frame.ppm');let s='';"
                "const C=0x8000;for(let i=0;i<d.length;i+=C){"
                "s+=String.fromCharCode.apply(null,d.subarray(i,i+C));}return btoa(s);}"
                "catch(e){return 'ERR:'+e;}})()"
            )
            if b64 and not b64.startswith("ERR:"):
                import base64
                raw = base64.b64decode(b64)
                with open("/tmp/mole-frame.ppm", "wb") as f:
                    f.write(raw)
                print(f"[harness] 真实渲染帧 -> /tmp/mole-frame.ppm ({len(raw)} bytes)")
            else:
                print(f"[harness] 读 /frame.ppm: {b64}")
        except Exception as e:
            print(f"[harness] 读 frame.ppm 失败: {e}")
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

    try:
        with open("/tmp/mole-console-all.txt", "w") as f:
            f.write("\n".join(console_lines))
        print(f"[harness] 全部 console({len(console_lines)} 行)-> /tmp/mole-console-all.txt")
    except OSError:
        pass
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
