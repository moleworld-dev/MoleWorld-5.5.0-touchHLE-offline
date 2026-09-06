#!/bin/bash
# make-testflight.sh — 组装 + 发布签名 + (可选)上传一个 App Store / TestFlight 用的 .ipa。
#
# 与 make-ios-ipa.sh(AltStore ad-hoc 路线)、mw-ios-run.sh(开发证书真机侧载)互不干扰:
# 这个脚本专门产出【Apple Distribution 发布签名】的 .ipa,带资产目录(Assets.car)和
# App Store Connect 上传校验所需的全部 Info.plist 键。
#
# 前置(用户在 Apple 门户做完 A+B+E 后提供,见 dev-scripts 里的 TestFlight 计划):
#   DIST_IDENTITY  发布签名身份,形如 "Apple Distribution: Name (TEAMID)"
#                  (用 `security find-identity -v -p codesigning` 查)
#   PROFILE        App Store 类型的 .mobileprovision 路径
#   TEAM_ID        10 位 Team ID(默认从 DIST_IDENTITY 括号里抽)
#   BUNDLE_ID      默认 org.touchhle.moleworldhd(若门户注册了别的,在这里覆盖)
#   BUILD          CFBundleVersion(每次上传必须唯一且递增;默认时间戳分钟数)
# 上传(可选,加 --upload;需 ASC API key):
#   ASC_KEY_ID, ASC_ISSUER_ID  App Store Connect API Team key 的 Key ID / Issuer ID
#                              (.p8 放 ~/.appstoreconnect/private_keys/AuthKey_<KEYID>.p8)
#
# 用法(发布签名打包):
#   先构建【干净 release,不带 interp_hb】:
#     SB=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin
#     ... RUSTC=$SB/rustc $SB/cargo build --release --target aarch64-apple-ios \
#         --no-default-features --features static,cpu_interpreter --bin touchHLE
#   再:
#     DIST_IDENTITY="Apple Distribution: Name (TEAMID)" PROFILE=~/Downloads/x.mobileprovision \
#       dev-scripts/make-testflight.sh
#   打包 + 上传:
#     DIST_IDENTITY=... PROFILE=... ASC_KEY_ID=... ASC_ISSUER_ID=... \
#       dev-scripts/make-testflight.sh --upload
set -euo pipefail

TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$TOUCHHLE_DIR"

# ---- 配置 ----
EXE="target/aarch64-apple-ios/release/touchHLE"
GAME_APP="../../01-cracked/Payload/MoleWorld.app"
ICON_SRC="res/icon.png"
APPNAME="MoleWorldHD"
BUNDLE_ID="${BUNDLE_ID:-org.touchhle.moleworldhd}"
VERSION="5.5.0"
# CFBundleVersion 必须唯一且单调递增:用 当前 epoch 分钟数(脚本不能用 date?可以,这是 host shell)
BUILD="${BUILD:-$(date +%Y%m%d%H%M)}"
STAGE="_tf_stage"
APP="$STAGE/Payload/$APPNAME.app"
DO_UPLOAD=0
[ "${1:-}" = "--upload" ] && DO_UPLOAD=1

# ---- 前置检查 ----
[ -f "$EXE" ] || { echo "✗ 缺 device 可执行 $EXE,先构建(干净 release,不带 interp_hb)"; exit 1; }
[ -d "$GAME_APP" ] || { echo "✗ 缺游戏 $GAME_APP"; exit 1; }
: "${DIST_IDENTITY:?✗ 需设 DIST_IDENTITY(Apple Distribution 签名身份)}"
: "${PROFILE:?✗ 需设 PROFILE(App Store .mobileprovision 路径)}"
[ -f "$PROFILE" ] || { echo "✗ profile 文件不存在:$PROFILE"; exit 1; }
TEAM_ID="${TEAM_ID:-$(echo "$DIST_IDENTITY" | sed -nE 's/.*\(([A-Z0-9]{10})\).*/\1/p')}"
[ -n "$TEAM_ID" ] || { echo "✗ 无法从 DIST_IDENTITY 抽出 Team ID,请显式设 TEAM_ID"; exit 1; }
echo "▶ Bundle=$BUNDLE_ID  Team=$TEAM_ID  Build=$BUILD  Identity=$DIST_IDENTITY"

rm -rf "$STAGE" && mkdir -p "$APP"

# ---- 1) 主二进制(lipo 成单 arch fat,清旧签名)----
cp "$EXE" "$APP/$APPNAME.thin"
lipo -create "$APP/$APPNAME.thin" -output "$APP/$APPNAME"
rm -f "$APP/$APPNAME.thin"
chmod +x "$APP/$APPNAME"

# ---- 2) touchHLE 运行时资源 ----
cp -R touchHLE_fonts "$APP/"
cp touchHLE_default_options.txt "$APP/"
# [MoleWorld iOS] App Store 不允许 bundle 里有松散 .dylib(guest 库触发 ITMS-90171「不允许
# 独立库」/ 90209「段对齐」)。把 touchHLE_dylibs 拷进去,但其中的 *.dylib 打进
# touchHLE_dylibs.zip 后删掉松散的——上传校验不扫 zip 内部,运行时 ResourceFile 从该 zip
# 按 basename 提取(见 paths.rs)。README/COPYING 等文本保持松散(licenses.rs 要读,非 Mach-O
# 不会被拦)。
cp -R touchHLE_dylibs "$APP/"
( cd "$APP/touchHLE_dylibs" && zip -X -q "$TOUCHHLE_DIR/$APP/touchHLE_dylibs.zip" *.dylib && rm -f *.dylib )

# ---- 3) 游戏打包进 MoleWorld.ipa(.app 根)----
rm -rf _tf_game && mkdir -p _tf_game/Payload
cp -R "$GAME_APP" "_tf_game/Payload/MoleWorld.app"
find "_tf_game/Payload/MoleWorld.app" \( -name "*.decoded.plist" -o -name ".DS_Store" \) -delete || true
( cd _tf_game && zip -r -X -0 -q "$TOUCHHLE_DIR/$APP/MoleWorld.ipa" Payload )
rm -rf _tf_game

# ---- 4) 资产目录(actool → Assets.car)+ CFBundleIconName ----
# App Store 上传强制要图标在资产目录里。用【显式逐尺寸】格式(不依赖 actool 的运行时派生,
# 更稳),并【依次尝试 默认 / Xcode-beta / 显式稳定版 的 actool】:Xcode 自动升级常导致某个
# Xcode 的 actool 缺匹配的模拟器 runtime 而失败,自动挑第一个能产出 Assets.car 的。
ICONSET="$STAGE/Assets.xcassets/AppIcon.appiconset"
mkdir -p "$ICONSET"
for s in 20 29 40 58 60 76 80 87 120 152 167 180 1024; do
  sips -z $s $s "$ICON_SRC" --out "$ICONSET/icon-$s.png" >/dev/null 2>&1
done
cat > "$ICONSET/Contents.json" <<'EOF'
{
  "images" : [
    {"size":"20x20","idiom":"iphone","filename":"icon-40.png","scale":"2x"},
    {"size":"20x20","idiom":"iphone","filename":"icon-60.png","scale":"3x"},
    {"size":"29x29","idiom":"iphone","filename":"icon-58.png","scale":"2x"},
    {"size":"29x29","idiom":"iphone","filename":"icon-87.png","scale":"3x"},
    {"size":"40x40","idiom":"iphone","filename":"icon-80.png","scale":"2x"},
    {"size":"40x40","idiom":"iphone","filename":"icon-120.png","scale":"3x"},
    {"size":"60x60","idiom":"iphone","filename":"icon-120.png","scale":"2x"},
    {"size":"60x60","idiom":"iphone","filename":"icon-180.png","scale":"3x"},
    {"size":"20x20","idiom":"ipad","filename":"icon-20.png","scale":"1x"},
    {"size":"20x20","idiom":"ipad","filename":"icon-40.png","scale":"2x"},
    {"size":"29x29","idiom":"ipad","filename":"icon-29.png","scale":"1x"},
    {"size":"29x29","idiom":"ipad","filename":"icon-58.png","scale":"2x"},
    {"size":"40x40","idiom":"ipad","filename":"icon-40.png","scale":"1x"},
    {"size":"40x40","idiom":"ipad","filename":"icon-80.png","scale":"2x"},
    {"size":"76x76","idiom":"ipad","filename":"icon-76.png","scale":"1x"},
    {"size":"76x76","idiom":"ipad","filename":"icon-152.png","scale":"2x"},
    {"size":"83.5x83.5","idiom":"ipad","filename":"icon-167.png","scale":"2x"},
    {"size":"1024x1024","idiom":"ios-marketing","filename":"icon-1024.png","scale":"1x"}
  ],
  "info" : { "author" : "xcode", "version" : 1 }
}
EOF
ACTOOL_OK=0
for DD in "" "/Applications/Xcode-beta.app/Contents/Developer" "/Applications/Xcode.app/Contents/Developer"; do
  { [ -n "$DD" ] && [ ! -d "$DD" ]; } && continue
  rm -f "$APP/Assets.car"
  env ${DD:+DEVELOPER_DIR="$DD"} xcrun actool "$STAGE/Assets.xcassets" --compile "$APP" --app-icon AppIcon \
    --output-partial-info-plist "$STAGE/icon-partial.plist" \
    --platform iphoneos --minimum-deployment-target 13.0 \
    --target-device iphone --target-device ipad > "$STAGE/actool.log" 2>&1 || true
  if [ -f "$APP/Assets.car" ]; then ACTOOL_OK=1; echo "✓ actool 成功(DEVELOPER_DIR=${DD:-默认 Xcode})"; break; fi
done
[ "$ACTOOL_OK" = 1 ] || { echo "✗ actool 所有 Xcode 都没产出 Assets.car:"; tail -6 "$STAGE/actool.log"; exit 1; }

# ---- 5) Info.plist(含 App Store 上传校验所需的全部键)----
# DT* 构建标记:transporter 校验"是否用公开 SDK 构建",从当前 Xcode/SDK 取真实值。
SDK_VER="$(xcrun --sdk iphoneos --show-sdk-version)"
SDK_BUILD="$(xcrun --sdk iphoneos --show-sdk-build-version 2>/dev/null || echo "")"
XCODE_VER="$(xcodebuild -version 2>/dev/null | sed -nE 's/Xcode ([0-9.]+)/\1/p' | head -1)"
XCODE_BUILD="$(xcodebuild -version 2>/dev/null | sed -nE 's/Build version (.*)/\1/p' | head -1)"
# DTXcode 格式如 1610 表示 16.1.0
DTXCODE="$(printf '%04d' "$(echo "$XCODE_VER" | awk -F. '{printf "%d%d%d",$1,$2,($3==""?0:$3)}')" 2>/dev/null || echo "1600")"
OS_BUILD="$(sw_vers -buildVersion)"

cat > "$APP/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key>          <string>$APPNAME</string>
	<key>CFBundleIdentifier</key>          <string>$BUNDLE_ID</string>
	<key>CFBundleName</key>                <string>$APPNAME</string>
	<key>CFBundleDisplayName</key>         <string>摩尔庄园HD</string>
	<key>CFBundleVersion</key>             <string>$BUILD</string>
	<key>CFBundleShortVersionString</key>  <string>$VERSION</string>
	<key>CFBundlePackageType</key>         <string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>CFBundleDevelopmentRegion</key>   <string>zh_CN</string>
	<key>LSRequiresIPhoneOS</key>          <true/>
	<key>MinimumOSVersion</key>            <string>13.0</string>
	<key>UIRequiresFullScreen</key>        <true/>
	<key>UIFileSharingEnabled</key>        <true/>
	<key>LSSupportsOpeningDocumentsInPlace</key> <true/>
	<key>LSApplicationCategoryType</key>   <string>public.app-category.games</string>
	<key>ITSAppUsesNonExemptEncryption</key> <false/>
	<key>UIRequiredDeviceCapabilities</key> <array><string>arm64</string></array>
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
	<key>DTPlatformName</key>              <string>iphoneos</string>
	<key>DTPlatformVersion</key>           <string>$SDK_VER</string>
	<key>DTSDKName</key>                   <string>iphoneos$SDK_VER</string>
	<key>DTSDKBuild</key>                  <string>$SDK_BUILD</string>
	<key>DTXcode</key>                     <string>$DTXCODE</string>
	<key>DTXcodeBuild</key>                <string>$XCODE_BUILD</string>
	<key>BuildMachineOSBuild</key>         <string>$OS_BUILD</string>
</dict>
</plist>
PLIST

# 合并 actool 给出的图标键(CFBundleIconName + CFBundleIcons[~ipad])
/usr/libexec/PlistBuddy -c "Merge $STAGE/icon-partial.plist" "$APP/Info.plist"
# 顶层 CFBundleIconName(部分校验直接读顶层)
ICONNAME="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIcons:CFBundlePrimaryIcon:CFBundleIconName' "$APP/Info.plist" 2>/dev/null || echo AppIcon)"
/usr/libexec/PlistBuddy -c "Add :CFBundleIconName string $ICONNAME" "$APP/Info.plist" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Set :CFBundleIconName $ICONNAME" "$APP/Info.plist"
plutil -lint "$APP/Info.plist" >/dev/null && echo "✓ Info.plist 合法"

# ---- 6) 发布 entitlements(从 profile 抽,强制 get-task-allow=false)----
security cms -D -i "$PROFILE" > "$STAGE/profile.plist"
/usr/libexec/PlistBuddy -x -c 'Print :Entitlements' "$STAGE/profile.plist" > "$STAGE/entitlements.plist"
/usr/libexec/PlistBuddy -c "Set :get-task-allow false" "$STAGE/entitlements.plist" 2>/dev/null || \
  /usr/libexec/PlistBuddy -c "Add :get-task-allow bool false" "$STAGE/entitlements.plist"
echo "  entitlements: application-identifier=$(/usr/libexec/PlistBuddy -c 'Print :application-identifier' "$STAGE/entitlements.plist" 2>/dev/null), get-task-allow=$(/usr/libexec/PlistBuddy -c 'Print :get-task-allow' "$STAGE/entitlements.plist")"

# ---- 6.5) 校准 LC_BUILD_VERSION 的 sdk 声明(★必须在 codesign 之前:vtool 会让签名失效)----
# 这个值被【两个方向】同时夹住,只有 [26.0, 27.0) 这个窗口能同时满足:
#   · 下界:App Store Connect 拒收 sdk < 26 的包(altool 报 90725「必须用 iOS 26 或更新的 SDK 构建」)。
#     所以开发侧载用的 18.0(mw-deploy-17pm.sh)在这里【传不上去】。
#   · 上界:iOS 27 强制 UIScene——凡 sdk >= 27 且未适配 scene 生命周期的 app 一启动就被
#     __UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption trap 掉(EXC_BREAKPOINT 秒退),
#     而 SDL2(本仓 2.26.4)完全没有 scene 支持。
#     ★真机实测(17PM / iOS 27.0):sdk=26.5 存活并正常渲染,sdk=27.0 秒退 ⇒ 门槛在 27,不在 26。
# 正确做法是【用正式版 Xcode 编译】,链接器自然打上 26.x;xcode-select 指向 Xcode-beta 时会打上 27.0,
# 此时下面兜底降到 26.5。顺带一提:DTXcode 等元数据也必须来自正式版 Xcode,否则外部测试提交会被
# 「此构建版本使用的是 Beta 版 Xcode」挡下(见脚本头部用法里的 DEVELOPER_DIR)。
SDK_DECL="$(vtool -show-build-version "$APP/$APPNAME" 2>/dev/null | sed -nE 's/^ *sdk ([0-9.]+).*/\1/p' | head -1)"
SDK_MAJOR="${SDK_DECL%%.*}"
if [ -z "$SDK_DECL" ]; then
	echo "✗ 读不出 LC_BUILD_VERSION,拒绝出包"; exit 1
elif [ "$SDK_MAJOR" -ge 27 ]; then
	echo "  sdk 声明 $SDK_DECL >= 27(iOS 27 会 UIScene trap)→ 降到 26.5"
	vtool -set-build-version 2 13.0 26.5 -replace -output "$APP/$APPNAME.patched" "$APP/$APPNAME" >/dev/null 2>&1 \
	  && mv "$APP/$APPNAME.patched" "$APP/$APPNAME" && chmod +x "$APP/$APPNAME" \
	  || { echo "✗ vtool 降 SDK 失败"; exit 1; }
elif [ "$SDK_MAJOR" -lt 26 ]; then
	echo "✗ sdk 声明 $SDK_DECL < 26,App Store Connect 会以 90725 拒收。"
	echo "  请用正式版 Xcode 26.x 重新编译(DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer)。"; exit 1
fi
echo "✓ LC_BUILD_VERSION:$(vtool -show-build-version "$APP/$APPNAME" 2>/dev/null | sed -nE 's/^ *(minos|sdk) /\1=/p' | tr '\n' ' ')"

# ---- 7) 嵌 profile + 发布签名 ----
cp "$PROFILE" "$APP/embedded.mobileprovision"
# guest dylib 先各自签(Mach-O,bundle 校验要求有签名)
for dylib in "$APP"/touchHLE_dylibs/*.dylib; do
	codesign --force --timestamp --sign "$DIST_IDENTITY" "$dylib" >/dev/null 2>&1 || \
	  codesign --force --sign "$DIST_IDENTITY" "$dylib" 2>/dev/null || echo "  (dylib 签名警告:$dylib)"
done
codesign --force --timestamp --sign "$DIST_IDENTITY" \
  --entitlements "$STAGE/entitlements.plist" --generate-entitlement-der \
  -i "$BUNDLE_ID" "$APP"
echo "--- 签名核对 ---"
codesign -dv --verbose=4 "$APP" 2>&1 | grep -iE "Authority|Identifier|TeamIdentifier|flags" | head -6
codesign --verify --strict --verbose=2 "$APP" 2>&1 | head -3 && echo "✓ 发布签名通过"

# ---- 8) 打包 Payload zip ----
IPA="$TOUCHHLE_DIR/摩尔庄园HD-testflight.ipa"
rm -f "$IPA"
( cd "$STAGE" && zip -r -X -q "$IPA" Payload )
echo "✓ 已生成 $IPA"

# ---- 9) (可选)校验 + 上传 ----
if [ "$DO_UPLOAD" = "1" ]; then
	: "${ASC_KEY_ID:?✗ 上传需 ASC_KEY_ID}"; : "${ASC_ISSUER_ID:?✗ 上传需 ASC_ISSUER_ID}"
	echo "▶ altool 校验中..."
	xcrun altool --validate-app -f "$IPA" -t ios --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID" --output-format xml || { echo "✗ 校验失败,见上"; exit 1; }
	echo "▶ altool 上传中..."
	xcrun altool --upload-app -f "$IPA" -t ios --apiKey "$ASC_KEY_ID" --apiIssuer "$ASC_ISSUER_ID" --output-format xml
	echo "✓ 上传完成。去 App Store Connect → TestFlight 等处理 + 答导出合规 + 指派【内部】测试组。"
fi

rm -rf "$STAGE"
