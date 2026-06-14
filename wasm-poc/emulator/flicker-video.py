#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""淘米启动页闪烁 —— 录像差分(faithful)。
element.screenshot 会强制 paint 总能读到 buffer 内容,掩盖真实的「实时合成器」闪烁。
改用 Playwright 录像(捕获真实合成输出,逐帧=用户实际所见),再 ffmpeg 抽帧测亮度,
对比 Run A(无 preserveDrawingBuffer)vs Run B(强制 preserveDrawingBuffer:true)。
若 A 出现亮度大幅来回跳变(闪)、B 平稳 → 实锤 preserveDrawingBuffer 是根因+修复。
用法:python3.11 flicker-video.py --dir <release目录> [--secs 16] [--port 8812]
"""
import argparse, http.server, os, socketserver, subprocess, sys, threading, urllib.parse, glob
import numpy as np
from PIL import Image

CTYPES = {".js": "text/javascript", ".wasm": "application/wasm", ".data": "application/octet-stream",
          ".ipa": "application/octet-stream", ".html": "text/html; charset=utf-8"}
PRESERVE_JS = ("(function(){const o=HTMLCanvasElement.prototype.getContext;"
    "HTMLCanvasElement.prototype.getContext=function(t,a){"
    "if(t==='webgl2'||t==='webgl'||t==='experimental-webgl'){"
    "a=Object.assign({},a,{preserveDrawingBuffer:true,alpha:false});}"
    "return o.call(this,t,a);};})();")

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

def record(url, preserve, secs, viddir):
    from playwright.sync_api import sync_playwright
    os.makedirs(viddir, exist_ok=True)
    with sync_playwright() as p:
        browser = p.chromium.launch(headless=False, args=["--ignore-gpu-blocklist", "--enable-webgl"])
        ctx = browser.new_context(viewport={"width": 1100, "height": 840},
                                  record_video_dir=viddir, record_video_size={"width": 1100, "height": 840})
        page = ctx.new_page()
        if preserve: page.add_init_script(PRESERVE_JS)
        page.goto(url, wait_until="commit")
        page.wait_for_timeout(int(secs * 1000))
        ctx.close(); browser.close()
    vids = sorted(glob.glob(os.path.join(viddir, "*.webm")), key=os.path.getmtime)
    return vids[-1] if vids else None

def frames_brightness(video, fps=12):
    fdir = video + "_frames"
    os.makedirs(fdir, exist_ok=True)
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", video,
                    "-vf", f"fps={fps},crop=in_w*0.62:in_h:0:0", os.path.join(fdir, "f%04d.png")], check=True)
    out = []
    for fp in sorted(glob.glob(os.path.join(fdir, "f*.png"))):
        arr = np.asarray(Image.open(fp).convert("L"), dtype=np.float32)
        out.append((fp, float(arr.mean())))
    return out

def analyze(name, fb):
    if not fb: return "(无帧)", 0, []
    vals = [b for _, b in fb]
    # 闪烁=相邻帧亮度大幅跳变。统计 |Δ|>20 的次数
    jumps = sum(1 for k in range(1, len(vals)) if abs(vals[k]-vals[k-1]) > 20)
    s = "".join("#" if v >= (max(vals)+min(vals))/2 else "." for v in vals)
    return s, jumps, vals

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dir", required=True); ap.add_argument("--secs", type=float, default=16)
    ap.add_argument("--port", type=int, default=8812)
    args = ap.parse_args()
    root = os.path.abspath(args.dir)
    httpd = TServer(("127.0.0.1", args.port), make_handler(root))
    threading.Thread(target=httpd.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{args.port}/index.html"
    print(f"[vid] {url}")
    for label, preserve, vdir in (("A 无preserve", False, "/tmp/vid-noprev"), ("B 强制preserve", True, "/tmp/vid-prev")):
        print(f"\n=== Run {label} 录像中({args.secs}s)===")
        v = record(url, preserve, args.secs, vdir)
        if not v: print("  录像失败"); continue
        fb = frames_brightness(v)
        s, jumps, vals = analyze(label, fb)
        print(f"亮度时间线({len(fb)}帧@12fps):\n{s}")
        print(f"大跳变(|Δ|>20)次数={jumps}  范围[{min(vals):.0f},{max(vals):.0f}]")
        # 存几张代表帧供查看
        if fb:
            for tag, fp in (("first", fb[0][0]), ("mid", fb[len(fb)//2][0]), ("last", fb[-1][0])):
                try: Image.open(fp).save(f"/tmp/vidframe_{label.split()[0]}_{tag}.png")
                except Exception: pass
    httpd.shutdown()
    print("\n判读:Run A 大跳变远多于 Run B → preserveDrawingBuffer 实锤为根因+修复。")

if __name__ == "__main__":
    main()
