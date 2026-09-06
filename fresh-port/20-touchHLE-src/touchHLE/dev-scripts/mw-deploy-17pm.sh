#!/bin/bash
# 一键:编 iOS 二进制 → 现签 profile(如缺) → 重建 staging → 用开发证书签 → 装 17PM → 启动。
# 这是本项目【唯一允许】的调试链路(dev 签名 + 真机;禁 PlayCover / 桌面 dynarmic / 魔改)。
# 用法: dev-scripts/mw-deploy-17pm.sh [--no-build]
set -uo pipefail
TH="$(cd "$(dirname "$0")/.." && pwd)"
DEV_CD=5502DB69-355D-5576-8BAB-631A91D70122        # iPhone 17 Pro Max (CoreDevice)
BID=org.touchhle.moleworldhd
# ★证书会随 Xcode 轮换:自动取钥匙串里当前有私钥的 Apple Development 证书(可用 MW_CERT_SHA1 覆盖)
CERT="${MW_CERT_SHA1:-$(security find-identity -v -p codesigning 2>/dev/null | awk '/Apple Development/{print $2; exit}')}"
[ -n "$CERT" ] || { echo "✗ 钥匙串里没有 Apple Development 证书"; exit 1; }
STAGE=/tmp/mw_realapp
PROF=/tmp/moleworldhd_dev_fresh.mobileprovision
ENT=/tmp/mw_dev_ent.plist
EXE="$TH/target/aarch64-apple-ios/release/touchHLE"
APP="$STAGE/Payload/MoleWorldHD.app"
MINT="${MW_MINT_SCRIPT:-$TH/dev-scripts/mw-asc-mint.py}"   # 现签脚本已固化进仓库

if [ "${1:-}" != "--no-build" ]; then
  echo "[1/5] 编译 iOS 二进制(cpu_interpreter,无 JIT)"
  SB="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
  # ★坑:Xcode 换过路径时 build.rs 会缓存已失效的 libclang_rt.ios.a 路径 → touch 强制重跑
  [ -n "${MW_TOUCH_BUILDRS:-}" ] && touch "$TH/build.rs"
  ( cd "$TH" && CMAKE_POLICY_VERSION_MINIMUM=3.5 IPHONEOS_DEPLOYMENT_TARGET=13.0 RUSTC="$SB/rustc" \
      "$SB/cargo" build --release --target aarch64-apple-ios \
      --no-default-features --features static,cpu_interpreter --bin touchHLE ) || { echo "✗ 编译失败"; exit 1; }
fi
[ -f "$EXE" ] || { echo "✗ 缺二进制 $EXE"; exit 1; }

if [ ! -f "$PROF" ]; then
  echo "[2/5] 缺 provisioning profile,现签"
  MW_CERT_SHA1="$CERT" python3 "$MINT" || { echo "✗ 现签 profile 失败"; exit 1; }
fi

echo "[3/5] 重建 staging + entitlements"
rm -rf "$STAGE"; mkdir -p "$STAGE"
( cd "$STAGE" && unzip -q "$TH/摩尔庄园HD.ipa" ) || { echo "✗ 解 IPA 失败"; exit 1; }
security cms -D -i "$PROF" > /tmp/prof.plist 2>/dev/null
/usr/libexec/PlistBuddy -x -c "Print :Entitlements" /tmp/prof.plist > "$ENT" || { echo "✗ 抽 entitlements 失败"; exit 1; }

# [游戏资源同步] .app 内嵌的 MoleWorld.ipa 是游戏本体(store zip,Payload/MoleWorld.app 结构,ios_entry 就地解),
# 以前只换 touchHLE 二进制、从不重打它 → 仓库里新增/重打包的资源(宽屏底图 X_wide.png、重打包图集)上不了
# 设备。这里若仓库 payload 里有比内嵌 IPA 更新的文件,就按 make-ios-ipa.sh 同样配方重打(剔除
# .decoded.plist / .DS_Store / IDA 的 .i64)。
GAME_APP="$TH/../../01-cracked/Payload/MoleWorld.app"
INNER="$APP/MoleWorld.ipa"
# ★重打结果必须缓存在 staging 之外:staging 每次都 rm -rf 重解,拿 $INNER 的时间戳当基准的话,
#   基准永远是母 IPA 里的旧时间 → 每次部署都要 cp -R 整个 payload 再 zip 一遍(几百 MB、几十秒),
#   而重打好的东西下一次又被删掉。缓存放 target/(已 gitignore),只有仓库 payload 真的更新才重打。
CACHE="$TH/target/mw-inner-game.ipa"
if [ -d "$GAME_APP" ] && { [ ! -f "$CACHE" ] || [ -n "$(find "$GAME_APP" -newer "$CACHE" -type f ! -name '*.i64' ! -name '.DS_Store' ! -name '*.decoded.plist' | head -1)" ]; }; then
  echo "[3.5/5] 仓库 payload 比缓存新 → 重打游戏本体(store zip,配方同 make-ios-ipa.sh)"
  GS=$(mktemp -d); mkdir -p "$GS/Payload"
  cp -R "$GAME_APP" "$GS/Payload/MoleWorld.app"
  find "$GS/Payload/MoleWorld.app" \( -name "*.decoded.plist" -o -name ".DS_Store" -o -name "*.i64" \) -delete || true
  rm -f "$CACHE"
  ( cd "$GS" && zip -r -X -0 -q "$CACHE" Payload )
  rm -rf "$GS"
  echo "    内嵌 IPA: $(du -h "$CACHE" | cut -f1),含 _wide.png $(unzip -l "$CACHE" | grep -c '_wide.png') 张"
fi
[ -f "$CACHE" ] && cp -f "$CACHE" "$INNER"

echo "[4/5] 换二进制 + 嵌 profile + 签名"
cp "$EXE" "$APP/MoleWorldHD.thin"
lipo -create "$APP/MoleWorldHD.thin" -output "$APP/MoleWorldHD" && rm -f "$APP/MoleWorldHD.thin"
# ★★ iOS 27 强制 UIScene 生命周期:凡"链接的 SDK >= 26"且未采用 UIScene 的 app,一启动就被
# __UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption trap 掉(EXC_BREAKPOINT 秒退,
# app 自己的日志根本来不及写,真凭据在 systemCrashLogs 的 .ips 里)。SDL2 的 UIKit 后端没适配 UIScene,
# 所以用 Xcode(iOS27 SDK)编出来的二进制必崩。修法:把 LC_BUILD_VERSION 的 sdk 声明降到 18.0(<26)
# 即可让系统按旧 SDK app 放行,无需改 SDL2。必须在 codesign【之前】做(vtool 会使签名失效)。
vtool -set-build-version 2 13.0 18.0 -replace -output "$APP/MoleWorldHD.patched" "$APP/MoleWorldHD" >/dev/null 2>&1 \
  && mv "$APP/MoleWorldHD.patched" "$APP/MoleWorldHD" \
  || echo "(警告: vtool 降 SDK 失败,iOS27 上可能秒退)"
chmod +x "$APP/MoleWorldHD"
cp -f "$PROF" "$APP/embedded.mobileprovision"
codesign --force --sign "$CERT" --entitlements "$ENT" "$APP" || { echo "✗ 签名失败(证书是否轮换?)"; exit 1; }
codesign --verify "$APP" || echo "(verify 警告)"

echo "[5/5] 打包 + 装机 + 启动"
rm -f /tmp/mw_realapp.ipa
( cd "$STAGE" && zip -r -X -q /tmp/mw_realapp.ipa Payload )
xcrun devicectl device process signal --device "$DEV_CD" --signal SIGKILL "$BID" 2>/dev/null >/dev/null
xcrun devicectl device install app --device "$DEV_CD" /tmp/mw_realapp.ipa 2>&1 | grep -iE "App installed|installationURL|error|Locked" | tail -1
xcrun devicectl device process launch --device "$DEV_CD" --terminate-existing "$BID" 2>&1 | grep -iE "Launched|error|Locked" | tail -1
