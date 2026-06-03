#!/bin/sh
# ============================================================
#  摩尔庄园HD · touchHLE Linux 启动器  (v0.0.4 beta)
#
#  启动方式(任选其一):
#    · 终端:        ./启动游戏.sh
#    · KDE / XFCE:  双击本文件 → 选「运行 / Run」
#    · GNOME:       右键本文件 →「以程序运行 / Run as a Program」
#                   (GNOME 双击 .sh 默认只会用文本编辑器打开,不会运行!)
#    · 想要桌面/菜单图标? 先运行同目录的「安装到应用菜单.sh」
# ============================================================
set -u

# 解析脚本真实所在目录(兼容空格 / 中文 / 软链接 / 被双击时 $0 为相对路径)
SELF="$0"
case "$SELF" in
  /*) : ;;
  *)  SELF="$(pwd)/$SELF" ;;
esac
DIR="$(cd "$(dirname "$SELF")" && pwd)"
cd "$DIR" || { echo "无法进入目录: $DIR"; exit 1; }

BIN="$DIR/touchHLE"
APP="$DIR/MoleWorld.app"

if [ ! -e "$BIN" ]; then
  echo "找不到 touchHLE 可执行文件,压缩包可能没解压完整。"
  if [ -t 0 ]; then printf "按回车键关闭..."; read -r _ || true; fi
  exit 1
fi
chmod +x "$BIN" 2>/dev/null || true

run_game() {
  echo "=================================================="
  echo "  正在启动 摩尔庄园HD ..."
  echo "  · 鼠标左键 = 触摸(点击 / 拖动)"
  echo "  · 游戏内按 T = 召出修改器菜单"
  echo "  · 退出 = 关闭游戏窗口,或在此按 Ctrl+C"
  echo "=================================================="
  echo ""
  "$BIN" "$APP" --landscape-right --device-family=ipad
  st=$?
  if [ "$st" -ne 0 ]; then
    echo ""
    echo "⚠ 游戏异常退出(返回码 $st)。"
    echo "  若需反馈,请把上面的报错信息截图,提交到 GitHub Issues:"
    echo "  https://github.com/Shad0w23333/MoleWorld-5.5.0-touchHLE-offline/issues"
  fi
  if [ -t 0 ]; then printf "按回车键关闭..."; read -r _ || true; fi
  return $st
}

# 被文件管理器双击时通常没有终端(stdout 非 tty)。尝试在终端模拟器里重开一次,
# 让用户能看到运行日志;找不到终端就直接跑(游戏窗口照常出现,只是没有日志)。
if [ -t 1 ] || [ "${MOLE_IN_TERM:-}" = "1" ]; then
  run_game
else
  MOLE_IN_TERM=1
  export MOLE_IN_TERM
  for T in x-terminal-emulator gnome-terminal konsole xfce4-terminal mate-terminal tilix kitty alacritty xterm; do
    if command -v "$T" >/dev/null 2>&1; then
      case "$T" in
        gnome-terminal|tilix) exec "$T" -- "$SELF" ;;
        *)                    exec "$T" -e "$SELF" ;;
      esac
    fi
  done
  # 没找到终端模拟器:直接运行(游戏窗口仍会出现)
  run_game
fi
