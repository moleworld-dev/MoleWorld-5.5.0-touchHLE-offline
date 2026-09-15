#!/bin/bash
# mw-ios-sim.sh — 把 touchHLE iOS-simulator 构建装进已启动的模拟器、启动、命令行截屏。
# 全程命令行(simctl),不需要 GUI 控制、不需要真机解锁、不需要开发者签名。
#   组装 .app(首次)→ 换二进制 → ad-hoc 签名 → simctl install → launch → 截屏 → Read。
set -uo pipefail
BID=org.touchhle.moleworldhd
WAIT="${1:-20}"
SHOT="${2:-/tmp/mw_sim_shot.png}"
S(){ python3 -c "import time,sys;time.sleep(float(sys.argv[1]))" "$1"; }

TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"; cd "$TOUCHHLE_DIR"
EXE="target/aarch64-apple-ios-sim/release/touchHLE"
GAME_IPA="$HOME/Library/Containers/io.playcover.PlayCover/Applications/org.touchhle.moleworldhd.app/MoleWorld.ipa"
STAGE=/tmp/mw_sim
APP="$STAGE/MoleWorldHD.app"

[ -f "$EXE" ] || { echo "✗ 缺 $EXE,先构建模拟器版"; exit 1; }

# 首次组装(后续只换二进制)
if [ ! -d "$APP" ]; then
  echo "[stage] 首次组装模拟器 .app"
  rm -rf "$STAGE"; mkdir -p "$APP"
  cp -R touchHLE_fonts "$APP/"; cp -R touchHLE_dylibs "$APP/"; cp touchHLE_default_options.txt "$APP/"
  [ -f "$GAME_IPA" ] && cp "$GAME_IPA" "$APP/MoleWorld.ipa" || { echo "✗ 缺游戏 ipa"; exit 1; }
  sips -z 120 120 res/icon.png --out "$APP/AppIcon60x60@2x.png" >/dev/null 2>&1
  cat > "$APP/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
	<key>CFBundleExecutable</key><string>MoleWorldHD</string>
	<key>CFBundleIdentifier</key><string>org.touchhle.moleworldhd</string>
	<key>CFBundleName</key><string>MoleWorldHD</string>
	<key>CFBundleDisplayName</key><string>摩尔庄园HD</string>
	<key>CFBundleVersion</key><string>5.5.0</string>
	<key>CFBundleShortVersionString</key><string>5.5.0</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>LSRequiresIPhoneOS</key><true/>
	<key>MinimumOSVersion</key><string>13.0</string>
	<key>UIRequiresFullScreen</key><true/>
	<key>CFBundleSupportedPlatforms</key><array><string>iPhoneSimulator</string></array>
	<key>UIDeviceFamily</key><array><integer>1</integer><integer>2</integer></array>
	<key>UILaunchScreen</key><dict/>
	<key>UISupportedInterfaceOrientations</key>
	<array><string>UIInterfaceOrientationLandscapeRight</string><string>UIInterfaceOrientationLandscapeLeft</string></array>
	<key>UISupportedInterfaceOrientations~ipad</key>
	<array><string>UIInterfaceOrientationLandscapeRight</string><string>UIInterfaceOrientationLandscapeLeft</string></array>
</dict></plist>
PLIST
fi

echo "[1] 换二进制(模拟器版,arm64-sim)"
cp "$EXE" "$APP/MoleWorldHD"
chmod +x "$APP/MoleWorldHD"

echo "[2] ad-hoc 签名"
codesign --force --sign - "$APP" 2>&1 | tail -1 || true

echo "[3] 终止旧实例 + 卸载"
xcrun simctl terminate booted "$BID" 2>/dev/null || true

echo "[4] 装进已启动模拟器"
xcrun simctl install booted "$APP" 2>&1 | tail -2

echo "[5] 启动(--console 不挂,后台拿日志)"
xcrun simctl launch booted "$BID" 2>&1 | tail -1

echo "[6] 等 ${WAIT}s 渲染"; S "$WAIT"

echo "[7] 截屏 → $SHOT"
xcrun simctl io booted screenshot "$SHOT" 2>&1 | tail -1
ls -la "$SHOT" 2>/dev/null

echo "[8] app 日志位置(模拟器沙盒)"
DATA=$(xcrun simctl get_app_container booted "$BID" data 2>/dev/null)
LOG="$DATA/Library/Application Support/touchhle.org/touchHLE/touchHLE_log.txt"
echo "$LOG"
[ -f "$LOG" ] && cp "$LOG" /tmp/mw_sim_log.txt && echo "日志已拷到 /tmp/mw_sim_log.txt ($(wc -l < /tmp/mw_sim_log.txt) 行)" || echo "(暂无日志)"
