#!/bin/bash
# ============================================================
#  摩尔庄园 5.5.0  ·  touchHLE 启动器【宽屏适配·全屏】
#  --fullscreen + --fill-screen = 按你显示器比例铺满整个屏幕:
#    无黑边、不拉伸、不变形、UI 不错位(全屏无 resize/无 set_size,
#    避开 macOS 缩放窗口的 framebuffer 怪癖)。这是桌面最完美的适配方式。
#  世界场景(村庄/黄金岛)显示更多海洋;标题/剧情/小游戏用补好的宽图。
#  ★想窗口模式: 去掉下面的 --fullscreen(窗口可拖拽缩放、填满无黑边,
#    但拖成很不同的比例会轻微拉伸;别指望它像全屏那样完美)。
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

echo "正在启动 摩尔庄园 5.5.0【宽屏适配·全屏】..."
echo "  · --fullscreen --fill-screen 按屏比铺满整屏,无黑边不拉伸不错位"
echo "  · 想窗口模式: 去掉本文件最后一行的 --fullscreen"
echo "  · 退出全屏: 关掉窗口 或 Cmd+Q / 在此终端 Ctrl+C"
echo "  · 超宽屏(32:9)想要更满: 追加 --max-aspect=3.5"
echo "  · 时区: MOLE_TZ=${MOLE_TZ:-未设置(默认北京时间 Asia/Shanghai，export MOLE_TZ=host 可跟随本机时区)}"
echo ""

APP_ABS="$(cd "$(dirname "$APP")" && pwd)/$(basename "$APP")"
cd "$TOUCHHLE_DIR" || exit 1
# ★[2026-09-24] MOLE_FIX_MAPEXTEND(与 iOS 分支统一):只在村庄可视区/可行走区/出生区三个查表点,且存档里的扩地位
#   不是合法组合(例如坏档 mapExtend=6)时,临时补成包含它的最小合法组合来消除拖图闪屏;合法存档上等于不存在。
#   存档、扩地摆放、成就和任务一律按真实 mapExtend,不白送任何扩地。另外每次进自家村会按存档证据对账:
#   2026-06-11 以后被旧版「恒返回 0x1F」写坏的存档,会一次性稳妥收回白送的扩地位,并补回被抹掉的下扩。设 0 关。
export MOLE_FIX_MAPEXTEND="${MOLE_FIX_MAPEXTEND:-1}"
# ★UI 4:3 虚拟化:商店/结算/选关/弹窗等复杂布局 UI 按原设计 1024 布局并居中,世界场景仍 Hor+(见 mole_cheats.rs UI43)
export MOLE_UI43="${MOLE_UI43:-1}"
exec ./target/release/touchHLE "$APP_ABS" --landscape-right --device-family=ipad --fill-screen --fullscreen
