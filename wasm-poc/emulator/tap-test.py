#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""点击注入测试:进村后在 canvas 上点几个目标(顶栏 UI 按钮 / 建筑 / 底部按钮),逐次截图,
看点击是否有响应(弹菜单/换界面=canvas 大变)。验证 wasm 触摸→游戏交互链路是否真能用。
用法:python3.11 tap-test.py --dir <release目录> [--wait 36] [--port 8834]
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

def shot(page, path):
    page.screenshot(path=path)
    return np.asarray(Image.open(path).convert("RGB"))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True); ap.add_argument("--wait", type=float, default=36)
    ap.add_argument("--port", type=int, default=8834)
    args = ap.parse_args()
    root = os.path.abspath(args.dir)
    httpd = TServer(("127.0.0.1", args.port), make_handler(root))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{args.port}/index.html"
    from playwright.sync_api import sync_playwright
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False, args=["--ignore-gpu-blocklist", "--enable-webgl"])
        page = browser.new_page(viewport={"width": 1100, "height": 820})
        page.add_init_script("(function(){const o=HTMLCanvasElement.prototype.getContext;"
            "HTMLCanvasElement.prototype.getContext=function(t,a){if(t==='webgl2'||t==='webgl'){"
            "a=Object.assign({},a,{preserveDrawingBuffer:true,alpha:false});}return o.call(this,t,a);};})();")
        page.goto(url, wait_until="commit")
        page.wait_for_timeout(int(args.wait*1000))
        b = page.eval_on_selector("#canvas", "c=>{const r=c.getBoundingClientRect();return {x:r.x,y:r.y,w:r.width,h:r.height};}")
        print(f"[tap] canvas box={b}")
        base = shot(page, "/tmp/tap-0base.png")
        # 目标(canvas 内相对比例);游戏区在 canvas 里上下信箱黑边居中,纵向取偏中上
        targets = {
            "ui_topleft": (0.10, 0.30),   # 顶栏 UI 按钮区(等级/金币旁的功能按钮)
            "building_mid": (0.38, 0.55), # 中部一栋建筑
            "btn_bottomright": (0.62, 0.78),  # 右下"免费/建造"按钮区
            "ui_msgboard": (0.20, 0.33),  # 顶栏更右的按钮(留言板/任务)
        }
        results = []
        for name, (fx, fy) in targets.items():
            cx, cy = b["x"]+b["w"]*fx, b["y"]+b["h"]*fy
            page.mouse.click(cx, cy)
            page.wait_for_timeout(1200)
            a = shot(page, f"/tmp/tap-{name}.png")
            diff = float(np.abs(a.astype(int) - base.astype(int)).mean())
            results.append((name, diff))
            # 回到基线:再点空白处/Esc 尝试关闭(不强求)
            page.keyboard.press("Escape"); page.wait_for_timeout(600)
            base = shot(page, "/tmp/tap-0base.png")
        browser.close()
    httpd.shutdown()
    print("\n=== 各点击与点击前的画面差异(diff 大=有响应,弹了菜单/换了界面)===")
    for name, d in results:
        print(f"  {name:16} diff={d:6.1f}  {'← 有响应' if d > 8 else '(无明显变化)'}")

if __name__ == "__main__":
    main()
