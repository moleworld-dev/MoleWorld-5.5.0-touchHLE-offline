#!/bin/bash
# 拉 17PM 上 touchHLE 的运行日志(先杀冻结进程,否则容器拷贝 socket 会断)。
# 用法: dev-scripts/mw-pull-log.sh [输出路径]
set -uo pipefail
DEV_CD=5502DB69-355D-5576-8BAB-631A91D70122
BID=org.touchhle.moleworldhd
OUT="${1:-/tmp/th_log.txt}"
for _ in 1 2 3; do xcrun devicectl device process signal --device "$DEV_CD" --signal SIGKILL "$BID" 2>/dev/null >/dev/null; sleep 1; done
sleep 2
rm -f "$OUT"
for i in 1 2 3 4 5 6; do
  xcrun devicectl device copy from --device "$DEV_CD" --domain-type appDataContainer \
    --domain-identifier "$BID" --source 'Documents/touchHLE_log.txt' --destination "$OUT" 2>/dev/null >/dev/null
  [ -s "$OUT" ] && break
  sleep 3
done
[ -s "$OUT" ] && echo "✓ $OUT ($(wc -l < "$OUT") 行)" || { echo "✗ 拉取失败(设备锁屏/掉线?)"; exit 1; }
