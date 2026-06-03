#!/bin/bash
# ============================================================
#  摩尔庄园 5.5.0  ·  touchHLE 启动器(iPhone 版 · 480×320 · 锁定 3:2)
#  双击本文件即可启动 iPhone 版(游戏会加载整套 iPhone 横屏美术)。
#  注意:iPhone 美术是 480×320,比 iPad(1024×768)更小更糊;
#        retina(-hd 高清)美术因模拟器 UIScreen.scale 写死 1.0 暂不会加载。
#  想要最清晰 → 用 iPad 版(「启动摩尔庄园.command」/「…-锁定比例.command」)。
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

echo "正在启动 摩尔庄园 5.5.0(iPhone 版 · 480×320 · 锁定 3:2)..."
echo "  游戏自动加载 iPhone 横屏美术;画面等比、四周黑边。"
echo ""

APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
cd "$TOUCHHLE_DIR" || exit 1
exec ./target/release/touchHLE "$APP_ABS" --landscape-right --device-family=iphone --lock-aspect
