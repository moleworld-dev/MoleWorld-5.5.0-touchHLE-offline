#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""拖动注入测试:进村后在 canvas 上注入一段拖动平移,逐步截图,确认:
①拖动期间游戏不崩、不变黑/不卡启动图(渲染正常)②村庄随拖动平移(场景在动)。
实时「一闪一闪」截图抓不到(element.screenshot 强制 paint),此脚本只验「不崩+在动+收尾正常」。
用法:python3.11 drag-test.py --dir <release目录> [--wait 35] [--port 8819]
"""
import argparse, http.server, os, socketserver, threading, urllib.parse
import numpy as np
from PIL import Image

CTYPES = {".js": "text/javascript", ".wasm": "application/wasm", ".data": "application/octet-stream",
          ".ipa": "application/octet-stream", ".html": "text/html; charset=utf-8"}

def make_handler(root):
    class H(http.server.BaseHTTPRequestHandler):
        def log_message(self, *a): pass
        def do_GET(self):
            path = urllib.parse.urlparse(self.path).path
            if path in ("/", ""): path = "/index.html"
            fp = os.path.normpath(os.path.join(root, path.lstrip("/")))
            if not fp.startswith(os.path.abspath(root)) or not os.path.isfile(fp):
                self.send_response(404); self.end_headers(); self.wfile.write(b"nf"); return
            with open(fp, "rb") as f: body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", CTYPES.get(os.path.splitext(fp)[1], "application/octet-stream"))
            self.send_header("Content-Length", str(len(body)))
            for h in (("Cache-Control","no-store"),("Cross-Origin-Opener-Policy","same-origin"),
                      ("Cross-Origin-Embedder-Policy","require-corp"),("Cross-Origin-Resource-Policy","cross-origin")):
                self.send_header(*h)
            self.end_headers(); self.wfile.write(body)
    return H

class TServer(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True; allow_reuse_address = True

def canvas_mean(page, path):
    page.screenshot(path=path)
    a = np.asarray(Image.open(path).convert("RGB"))
    g = a[:, :int(a.shape[1]*0.60)]
    return g.mean(axis=(0,1))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True); ap.add_argument("--wait", type=float, default=35)
    ap.add_argument("--port", type=int, default=8819)
    args = ap.parse_args()
    root = os.path.abspath(args.dir)
    httpd = TServer(("127.0.0.1", args.port), make_handler(root))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{args.port}/index.html"
    print(f"[drag] {url}  等村庄 {args.wait}s")
    from playwright.sync_api import sync_playwright
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False, args=["--ignore-gpu-blocklist", "--enable-webgl"])
        page = browser.new_page(viewport={"width": 1100, "height": 820})
        page.add_init_script("(function(){const o=HTMLCanvasElement.prototype.getContext;"
            "HTMLCanvasElement.prototype.getContext=function(t,a){if(t==='webgl2'||t==='webgl'){"
            "a=Object.assign({},a,{preserveDrawingBuffer:true,alpha:false});}return o.call(this,t,a);};})();")
        page.goto(url, wait_until="commit")
        page.wait_for_timeout(int(args.wait*1000))
        box = page.eval_on_selector("#canvas", "c=>{const r=c.getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height};}")
        cx, cy = box["x"]+box["w"]*0.35, box["y"]+box["h"]*0.5
        print(f"[drag] canvas box={box}")
        means = []
        means.append(("pre", canvas_mean(page, "/tmp/drag-0pre.png")))
        # 注入一段向左的拖动(平移地图),分步移动并逐步截图
        page.mouse.move(cx, cy); page.mouse.down()
        for i in range(1, 7):
            page.mouse.move(cx - i*40, cy - i*10, steps=3)
            page.wait_for_timeout(160)
            means.append((f"drag{i}", canvas_mean(page, f"/tmp/drag-{i}.png")))
        page.mouse.up()
        page.wait_for_timeout(400)
        means.append(("post", canvas_mean(page, "/tmp/drag-7post.png")))
        browser.close()
    httpd.shutdown()
    print("\n=== 拖动各步 canvas 均值(村庄≈暖色;若变[~0]=崩黑,若~187=回启动图)===")
    for tag, m in means:
        print(f"  {tag:6} {m.round(1)}")
    vals = np.array([m for _, m in means])
    moved = np.abs(np.diff(vals.mean(axis=1))).max()
    print(f"相邻步均值最大变化={moved:.1f}(>3=场景确实随拖动在动)")

if __name__ == "__main__":
    main()
