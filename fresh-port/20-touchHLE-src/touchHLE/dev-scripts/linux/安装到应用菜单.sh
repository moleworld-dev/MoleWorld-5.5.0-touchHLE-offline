#!/bin/sh
# ============================================================
#  在「应用程序菜单」和桌面生成「摩尔庄园HD」启动图标。
#  这是 GNOME 上最可靠的「一键启动」方式(双击 .sh 在 GNOME 默认不会运行)。
#  运行一次即可,之后在程序列表里搜「摩尔」就能像普通软件一样点击启动。
# ============================================================
set -u

DIR="$(cd "$(dirname "$0")" && pwd)"
BIN="$DIR/touchHLE"
APP="$DIR/MoleWorld.app"
ICON="$DIR/icon.png"

if [ ! -e "$BIN" ]; then
  echo "找不到 touchHLE(请在解压后的游戏文件夹里运行本脚本)。"
  if [ -t 0 ]; then printf "按回车键关闭..."; read -r _ || true; fi
  exit 1
fi

APPS="$HOME/.local/share/applications"
mkdir -p "$APPS"
DESKTOP="$APPS/moleworldhd.desktop"

cat > "$DESKTOP" <<EOF
[Desktop Entry]
Type=Application
Version=1.0
Name=摩尔庄园HD
Name[en]=MoleWorld HD
Comment=摩尔庄园移动版 5.5.0 离线版(touchHLE)
Comment[en]=MoleWorld HD 5.5.0 offline (touchHLE)
Exec="$BIN" "$APP" --landscape-right --device-family=ipad
Path=$DIR
Icon=$ICON
Terminal=false
Categories=Game;
StartupNotify=true
EOF
chmod +x "$DESKTOP" 2>/dev/null || true

# 桌面也放一份(若有桌面目录),并尽力标记为「可信」让 GNOME 允许双击
for D in "$HOME/Desktop" "$HOME/桌面"; do
  if [ -d "$D" ]; then
    cp -f "$DESKTOP" "$D/摩尔庄园HD.desktop" 2>/dev/null || true
    chmod +x "$D/摩尔庄园HD.desktop" 2>/dev/null || true
    gio set "$D/摩尔庄园HD.desktop" metadata::trusted true 2>/dev/null || true
  fi
done

update-desktop-database "$APPS" 2>/dev/null || true

echo "✓ 已把『摩尔庄园HD』安装到应用程序菜单(在程序列表搜「摩尔」即可启动),并尝试放到桌面。"
echo "  若桌面图标显示为「不受信任」,右键它选「允许启动 / Allow Launching」。"
if [ -t 0 ]; then printf "按回车键关闭..."; read -r _ || true; fi
