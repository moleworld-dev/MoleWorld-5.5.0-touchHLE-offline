#!/bin/bash
# ============================================================
#  摩尔庄园 5.5.0  ·  touchHLE 启动器 (Apple Silicon macOS)
#  双击本文件即可启动游戏。
# ============================================================
cd "$(dirname "$0")"

# touchHLE 必须在它自己的目录下运行(要找 touchHLE_dylibs/ 和 touchHLE_fonts/)
TOUCHHLE_DIR="fresh-port/20-touchHLE-src/touchHLE"
BIN="$TOUCHHLE_DIR/target/release/touchHLE"
APP="fresh-port/01-cracked/Payload/MoleWorld.app"

if [ ! -x "$BIN" ]; then
  echo "找不到 touchHLE 可执行文件: $BIN"
  echo "请先编译: cd '$TOUCHHLE_DIR' && cargo build --release"
  read -n1 -s -r -p "按任意键退出..."
  exit 1
fi

echo "正在启动 摩尔庄园 5.5.0 ..."
echo "  模拟器: touchHLE (arm64)"
echo "  游戏:   MoleWorld.app"
echo ""
echo "操作提示:"
echo "  · 鼠标左键 = 触摸(点击/拖动)"
echo "  · 进入游戏后, 点击对话框可推进剧情"
echo "  · 关闭游戏: 直接关掉游戏窗口, 或在此终端按 Ctrl+C"
echo ""

# 进入 touchHLE 目录运行(用绝对路径指向 .app)
APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
cd "$TOUCHHLE_DIR" || exit 1
exec ./target/release/touchHLE "$APP_ABS" --landscape-right --device-family=ipad
