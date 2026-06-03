#!/bin/bash
# ============================================================
#  摩尔庄园 5.5.0  ·  touchHLE 启动器(iPad · 锁定比例 4:3)
#  双击本文件即可启动。画面保持 4:3 原始比例、四周黑边、不变形。
#  (想自由拉伸铺满 → 用「启动摩尔庄园.command」;想 iPhone 版 → 用「…-iPhone版.command」)
# ============================================================
cd "$(dirname "$0")"

TOUCHHLE_DIR="fresh-port/20-touchHLE-src/touchHLE"
BIN="$TOUCHHLE_DIR/target/release/touchHLE"
APP="fresh-port/01-cracked/Payload/MoleWorld.app"

if [ ! -x "$BIN" ]; then
  echo "找不到 touchHLE 可执行文件: $BIN"
  echo "请先编译: cd '$TOUCHHLE_DIR' && cargo build --release"
  read -n1 -s -r -p "按任意键退出..."
  exit 1
fi

echo "正在启动 摩尔庄园 5.5.0(iPad · 锁定比例 4:3)..."
echo "  画面等比、四周黑边、不变形;拖动窗口大小时保持 4:3。"
echo ""

APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
cd "$TOUCHHLE_DIR" || exit 1
exec ./target/release/touchHLE "$APP_ABS" --landscape-right --device-family=ipad --lock-aspect
