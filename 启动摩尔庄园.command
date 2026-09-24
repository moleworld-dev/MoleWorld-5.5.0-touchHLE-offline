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
echo "  时区:   MOLE_TZ=${MOLE_TZ:-未设置(默认北京时间 Asia/Shanghai，export MOLE_TZ=host 可跟随本机时区)}"
echo ""
echo "操作提示:"
echo "  · 鼠标左键 = 触摸(点击/拖动)"
echo "  · 进入游戏后, 点击对话框可推进剧情"
echo "  · 关闭游戏: 直接关掉游戏窗口, 或在此终端按 Ctrl+C"
echo ""

# 进入 touchHLE 目录运行(用绝对路径指向 .app)
APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
cd "$TOUCHHLE_DIR" || exit 1
# ★[2026-09-24] MOLE_FIX_MAPEXTEND(与 iOS 分支统一):只在村庄可视区/可行走区/出生区三个查表点,且存档里的扩地位
#   不是合法组合(例如坏档 mapExtend=6)时,临时补成包含它的最小合法组合来消除拖图闪屏;合法存档上等于不存在。
#   存档、扩地摆放、成就和任务一律按真实 mapExtend,不白送任何扩地。另外每次进自家村会按存档证据对账:
#   2026-06-11 以后被旧版「恒返回 0x1F」写坏的存档,会一次性稳妥收回白送的扩地位,并补回被抹掉的下扩。设 0 关。
export MOLE_FIX_MAPEXTEND="${MOLE_FIX_MAPEXTEND:-1}"
exec ./target/release/touchHLE "$APP_ABS" --landscape-right --device-family=ipad
