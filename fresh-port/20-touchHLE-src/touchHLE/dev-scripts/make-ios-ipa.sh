#!/bin/bash
# Assemble 摩尔庄园HD.ipa (unsigned) for iOS from the device executable + bundled
# game + touchHLE runtime resources. AltStore/SideStore re-signs on install; the
# user enables JIT (StikDebug/JitStreamer) at runtime.
#
# Build the device executable first:
#   SB=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin
#   BOOST_ROOT=/opt/homebrew CMAKE_PREFIX_PATH=/opt/homebrew \
#     CMAKE_POLICY_VERSION_MINIMUM=3.5 IPHONEOS_DEPLOYMENT_TARGET=13.0 \
#     RUSTC=$SB/rustc $SB/cargo build --release --target aarch64-apple-ios --bin touchHLE
# Then run this from the touchHLE dir: dev-scripts/make-ios-ipa.sh
set -euo pipefail

TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$TOUCHHLE_DIR"

EXE="target/aarch64-apple-ios/release/touchHLE"
GAME_APP="../../01-cracked/Payload/MoleWorld.app"
ICON_SRC="res/icon.png"
APPNAME="MoleWorldHD"
STAGE="_ios_stage"
APP="$STAGE/Payload/$APPNAME.app"

[ -f "$EXE" ] || { echo "缺少 device 可执行文件 $EXE,请先构建"; exit 1; }
[ -d "$GAME_APP" ] || { echo "缺少游戏 $GAME_APP"; exit 1; }

rm -rf "$STAGE" && mkdir -p "$APP"

# 1) 主可执行文件
cp "$EXE" "$APP/$APPNAME"
chmod +x "$APP/$APPNAME"

# 2) touchHLE 运行时资源(iOS 上从 .app 根 = SDL base_path 读取)
cp -R touchHLE_fonts "$APP/"
cp -R touchHLE_dylibs "$APP/"
cp touchHLE_default_options.txt "$APP/"

# 3) 把游戏打成 MoleWorld.ipa(store zip,Payload/MoleWorld.app 结构),放 .app 根;
#    ios_entry() 读 <bundle>/MoleWorld.ipa,BundleData 直接就地解 zip。
rm -rf _game_stage && mkdir -p _game_stage/Payload
cp -R "$GAME_APP" "_game_stage/Payload/MoleWorld.app"
find "_game_stage/Payload/MoleWorld.app" \( -name "*.decoded.plist" -o -name ".DS_Store" \) -delete || true
( cd _game_stage && zip -r -X -0 -q "$TOUCHHLE_DIR/$APP/MoleWorld.ipa" Payload )
rm -rf _game_stage

# 4) 图标(从 512x512 干净 PNG 缩放出 iOS 各档)
sips -z 120 120 "$ICON_SRC" --out "$APP/AppIcon60x60@2x.png"        >/dev/null
sips -z 152 152 "$ICON_SRC" --out "$APP/AppIcon76x76@2x~ipad.png"   >/dev/null
sips -z 167 167 "$ICON_SRC" --out "$APP/AppIcon83.5x83.5@2x~ipad.png" >/dev/null
sips -z 1024 1024 "$ICON_SRC" --out "$APP/AppIcon1024.png"          >/dev/null

# 5) Info.plist
cat > "$APP/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>          <string>MoleWorldHD</string>
	<key>CFBundleIdentifier</key>          <string>org.touchhle.moleworldhd</string>
	<key>CFBundleName</key>                <string>MoleWorldHD</string>
	<key>CFBundleDisplayName</key>         <string>摩尔庄园HD</string>
	<key>CFBundleVersion</key>             <string>5.5.0</string>
	<key>CFBundleShortVersionString</key>  <string>5.5.0</string>
	<key>CFBundlePackageType</key>         <string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>LSRequiresIPhoneOS</key>          <true/>
	<key>MinimumOSVersion</key>            <string>13.0</string>
	<key>UIRequiresFullScreen</key>        <true/>
	<key>CFBundleSupportedPlatforms</key>  <array><string>iPhoneOS</string></array>
	<key>UIDeviceFamily</key>              <array><integer>1</integer><integer>2</integer></array>
	<key>UILaunchScreen</key>              <dict/>
	<key>UISupportedInterfaceOrientations</key>
	<array>
		<string>UIInterfaceOrientationLandscapeRight</string>
		<string>UIInterfaceOrientationLandscapeLeft</string>
	</array>
	<key>UISupportedInterfaceOrientations~ipad</key>
	<array>
		<string>UIInterfaceOrientationLandscapeRight</string>
		<string>UIInterfaceOrientationLandscapeLeft</string>
	</array>
	<key>CFBundleIconFiles</key>
	<array>
		<string>AppIcon60x60</string>
		<string>AppIcon76x76</string>
	</array>
	<key>CFBundleIcons</key>
	<dict>
		<key>CFBundlePrimaryIcon</key>
		<dict>
			<key>CFBundleIconFiles</key>
			<array><string>AppIcon60x60</string></array>
		</dict>
	</dict>
	<key>CFBundleIcons~ipad</key>
	<dict>
		<key>CFBundlePrimaryIcon</key>
		<dict>
			<key>CFBundleIconFiles</key>
			<array><string>AppIcon60x60</string><string>AppIcon76x76</string></array>
		</dict>
	</dict>
</dict>
</plist>
PLIST

# 6) 打包成 IPA(Payload/ -> zip)
rm -f "$TOUCHHLE_DIR/摩尔庄园HD.ipa"
( cd "$STAGE" && zip -r -X -q "$TOUCHHLE_DIR/摩尔庄园HD.ipa" Payload )
rm -rf "$STAGE"

echo "✓ 已生成 $TOUCHHLE_DIR/摩尔庄园HD.ipa"
ls -la "$TOUCHHLE_DIR/摩尔庄园HD.ipa"
