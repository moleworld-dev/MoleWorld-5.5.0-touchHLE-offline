#!/bin/bash
# ============================================================================
#  启动 摩尔庄园 5.5.0(touchHLE)并连接在线私服 —— 方便交互调试
#  双击本文件运行。游戏窗口会打开;终端实时打印日志;Ctrl-C 结束。
#  日志同时存到 /tmp/mole_online_debug.log 与 touchHLE 目录的 touchHLE_log.txt。
# ============================================================================

# touchHLE 需要 CWD 下有 touchHLE_dylibs/ 与 touchHLE_fonts/,所以先进它的目录。
TOUCHHLE_DIR="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/20-touchHLE-src/touchHLE"
cd "$TOUCHHLE_DIR" || { echo "❌ 进不去 touchHLE 目录:$TOUCHHLE_DIR"; read -r; exit 1; }

# ===================== 可改配置(也可在外部 export 覆盖)=====================
export MOLE_MIMI="${MOLE_MIMI:-88888888}"                  # 登录米米号
export MOLE_SERVER="${MOLE_SERVER:-159.54.175.68:7821}"     # 私服 IP:端口(或 login.moleworld.net:7821)
export MOLE_PASSWORD="${MOLE_PASSWORD:-}"                   # 密码(空=服务器宽松接受)
export MOLE_HUD="${MOLE_HUD:-0}"                            # 调试悬浮窗:默认关,1=开
# ★[2026-09-24] MOLE_FIX_MAPEXTEND(与 iOS 分支统一):只在村庄可视区/可行走区/出生区三个查表点,且扩地位不是合法组合时
#   临时补成最小合法组合来消除拖图闪屏;不白送扩地、不写存档。在线模式私服是权威,只做原版的已完工桥补位,不做收回。设 0 关。
export MOLE_FIX_MAPEXTEND="${MOLE_FIX_MAPEXTEND:-1}"
# ===========================================================================

APP="/Users/xiaochoumao/Documents/github repo/摩尔庄园 5.5.0/fresh-port/01-cracked/Payload/MoleWorld.app"
BIN="./target/release/touchHLE"
LOG="/tmp/mole_online_debug.log"

if [ ! -x "$BIN" ]; then
  echo "❌ 找不到 touchHLE 二进制:$TOUCHHLE_DIR/$BIN"
  echo "   先在该目录跑:cargo build --release"
  read -r; exit 2
fi

echo "============================================================"
echo "🎮  摩尔庄园 5.5.0 · 在线模式"
echo "    米米号 : $MOLE_MIMI"
echo "    私服   : $MOLE_SERVER"
echo "    悬浮窗 : $MOLE_HUD (MOLE_HUD=1 可开)"
echo "    时区   : MOLE_TZ=${MOLE_TZ:-未设置(默认北京时间 Asia/Shanghai，MOLE_TZ=host 跟随本机)}"
echo "    日志   : $LOG  (+ touchHLE_log.txt)"
echo "    结束   : 在此窗口按 Ctrl-C"
echo "============================================================"

# --allow-network-access 打开联网;不带它即纯离线单机。
"$BIN" "$APP" --landscape-right --device-family=ipad --allow-network-access 2>&1 | tee "$LOG"

echo "============================================================"
echo "游戏已退出。日志在 $LOG。按回车关闭本窗口。"
read -r
