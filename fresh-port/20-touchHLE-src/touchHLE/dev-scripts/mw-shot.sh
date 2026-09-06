#!/bin/bash
# [MoleWorld iOS 真机取景器] 在容器 Documents 放触发文件 → app 转储接下来 8 帧 → 拉回并转成 PNG。
# 真机不能直接截屏(系统截屏要另装签名 agent),这是唯一的"看见设备画面"的手段。
# 用法: dev-scripts/mw-shot.sh [输出目录] [--relaunch]
#   --relaunch : 先放触发文件再重启 app —— 这样第 0 帧(splash 启动画面)也能拍到。
set -uo pipefail
DEV_CD=5502DB69-355D-5576-8BAB-631A91D70122
BID=org.touchhle.moleworldhd
OUT="${1:-/tmp/mw-shots}"; mkdir -p "$OUT"
RELAUNCH=0; [ "${2:-}" = "--relaunch" ] && RELAUNCH=1
TRIG=$(mktemp); echo now > "$TRIG"
xcrun devicectl device copy to --device "$DEV_CD" --domain-type appDataContainer \
  --domain-identifier "$BID" --source "$TRIG" --destination 'Documents/mole_shot_now' 2>&1 | grep -iE "error|Locked" | head -2
rm -f "$TRIG"
if [ "$RELAUNCH" = 1 ]; then
  xcrun devicectl device process launch --device "$DEV_CD" --terminate-existing "$BID" 2>&1 | grep -iE "Launched|error|Locked" | tail -1
fi
echo "等待取景(8 帧)…"; sleep 12
rm -f "$OUT"/mole_shot_*.ppm "$OUT"/mole_shot_*.png
for i in 0 1 2 3 4 5 6 7; do
  xcrun devicectl device copy from --device "$DEV_CD" --domain-type appDataContainer \
    --domain-identifier "$BID" --source "Documents/mole_shot_0$i.ppm" --destination "$OUT/mole_shot_0$i.ppm" 2>/dev/null >/dev/null
done
n=0
for f in "$OUT"/mole_shot_*.ppm; do
  [ -s "$f" ] || { rm -f "$f"; continue; }
  sips -s format png "$f" --out "${f%.ppm}.png" >/dev/null 2>&1 && n=$((n+1))
done
echo "✓ 取到 $n 张 → $OUT"; ls -la "$OUT"/*.png 2>/dev/null | awk '{print "   ",$NF,$5"B"}'
