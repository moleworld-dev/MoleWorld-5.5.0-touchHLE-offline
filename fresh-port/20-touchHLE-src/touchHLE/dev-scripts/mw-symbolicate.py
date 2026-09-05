#!/usr/bin/env python3
"""把 touchHLE 日志里的 [HOSTSTALL]/[PROF] 宿主回溯符号化,并对 [PROF] 采样做热点聚合。

用法: mw-symbolicate.py <log.txt> [--bin <与设备上一致的 touchHLE 二进制>] [--top 25]
需要日志里有 "dyld slide=0x..."(mole_watchdog 启动时打印)。
"""
import argparse, collections, re, subprocess, sys

ap = argparse.ArgumentParser()
ap.add_argument("log")
ap.add_argument("--bin", default="target/aarch64-apple-ios/release/touchHLE")
ap.add_argument("--top", type=int, default=25)
a = ap.parse_args()
txt = open(a.log, encoding="utf-8", errors="ignore").read()
m = re.search(r"dyld slide=(0x[0-9a-f]+)", txt)
if not m:
    sys.exit("日志里没有 dyld slide(不是带 mole_watchdog 的构建?)")
slide = m.group(1)
# backtrace_symbols_fd 行: "N   MoleWorldHD   0x0000000104xxxxxx sym + off" / "N   libsystem_kernel.dylib 0x..."
frame_re = re.compile(r"^\s*(\d+)\s+(\S+)\s+(0x[0-9a-f]+)")
blocks = []  # list of (kind, [(img, addr)])
cur = None
for ln in txt.splitlines():
    if ln.startswith("[HOSTSTALL] ---- main-thread") or ln.startswith("[PROF]"):
        cur = ("stall" if ln.startswith("[HOSTSTALL]") else "prof", [])
        blocks.append(cur)
        continue
    if cur is None:
        continue
    fm = frame_re.match(ln)
    if fm:
        cur[1].append((fm.group(2), fm.group(3)))
    elif ln.startswith("[") and not ln.startswith("[HOSTSTALL] ---- end"):
        cur = None
own = {addr for _, fr in blocks for img, addr in fr if "MoleWorldHD" in img or "touchHLE" in img}
sym = {}
if own:
    addrs = sorted(own)
    for i in range(0, len(addrs), 400):
        chunk = addrs[i:i + 400]
        out = subprocess.run(["atos", "-o", a.bin, "-arch", "arm64", "-s", slide] + chunk,
                             capture_output=True, text=True).stdout.splitlines()
        for ad, s in zip(chunk, out):
            s = re.sub(r"::h[0-9a-f]{16}", "", s)  # 去掉 rust hash 后缀
            sym[ad] = s.split(" (in ")[0]
def name(img, addr):
    return sym.get(addr) or f"{img}+{addr}"
stalls = [b for b in blocks if b[0] == "stall"]
profs = [b for b in blocks if b[0] == "prof"]
for k, (_, fr) in enumerate(stalls):
    print(f"=== [HOSTSTALL] 回溯 #{k+1} ===")
    for i, (img, ad) in enumerate(fr[:40]):
        print(f"  {i:2d} {name(img, ad)}")
if profs:
    print(f"\n=== [PROF] 采样 {len(profs)} 个 ===")
    # 跳过信号处理/backtrace 自身的前几帧:找到第一帧不含 sigtramp/backtrace/on_sigusr1 的
    skip = re.compile(r"sigtramp|backtrace|on_sigusr1|_sigtramp")
    self_c, incl_c = collections.Counter(), collections.Counter()
    for _, fr in profs:
        names = [name(i, ad) for i, ad in fr]
        while names and skip.search(names[0]):
            names.pop(0)
        if not names:
            continue
        self_c[names[0]] += 1
        for n in set(names):
            incl_c[n] += 1
    n = len(profs)
    print(f"\n--- 自身热点(栈顶)top {a.top} ---")
    for fn, c in self_c.most_common(a.top):
        print(f"  {c*100/n:5.1f}%  {fn}")
    print(f"\n--- 包含热点(在栈上出现)top {a.top} ---")
    for fn, c in incl_c.most_common(a.top):
        print(f"  {c*100/n:5.1f}%  {fn}")
