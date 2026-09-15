#!/bin/bash
# mw-ios-run.sh — 一键回路(Path B / 纯 Rust ARMv7 解释器):
#   换二进制 → 重签 → 装机 → 普通启动 → 拉日志 → 报告下一个拦路指令。
#
# 解释器不需要 JIT，不需要 debugserver/CS_DEBUGGED —— 普通 devicectl launch 即可，
# 所以这个脚本全自动、无需 root、无需隧道，反复调用即可推进移植。
#
# 用法：先构建 iOS 二进制(在 touchHLE 目录):
#   SB=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin
#   BOOST_ROOT=/opt/homebrew CMAKE_PREFIX_PATH=/opt/homebrew \
#     CMAKE_POLICY_VERSION_MINIMUM=3.5 IPHONEOS_DEPLOYMENT_TARGET=13.0 \
#     RUSTC=$SB/rustc $SB/cargo build --release --target aarch64-apple-ios \
#       --no-default-features --features static,cpu_interpreter --bin touchHLE
#   然后 dev-scripts/mw-ios-run.sh
set -uo pipefail
DEV_CD=66A49458-15F8-53FA-8B81-D4A0D460382A   # iPhone 16 Pro Max CoreDevice UUID
BID=org.touchhle.moleworldhd
CERT=48455922ECE1FDAE8E6BD93A3B8DDC43D9FE1D81
ENT=/tmp/mw_dev_ent.plist
STAGE=/tmp/mw_realapp
APP="$STAGE/Payload/MoleWorldHD.app"
EXE="target/aarch64-apple-ios/release/touchHLE"
WAIT="${1:-12}"                                # 等几秒让 app 跑(默认 12)
S(){ python3 -c "import time,sys;time.sleep(float(sys.argv[1]))" "$1"; }

TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"; cd "$TOUCHHLE_DIR"
[ -f "$EXE" ] || { echo "✗ 缺 $EXE,先构建"; exit 1; }
[ -d "$APP" ] || { echo "✗ 缺暂存 app $APP"; exit 1; }

echo "[1] 换二进制(lipo 成单 arch fat,清旧签名)"
cp "$EXE" "$APP/MoleWorldHD.thin"
lipo -create "$APP/MoleWorldHD.thin" -output "$APP/MoleWorldHD" && rm -f "$APP/MoleWorldHD.thin"
chmod +x "$APP/MoleWorldHD"

echo "[2] 重签 .app(--force 重建 CodeResources + 给主程序挂 entitlements)"
codesign --force --sign "$CERT" --entitlements "$ENT" "$APP" 2>&1 | tail -2
codesign --verify --verbose=1 "$APP" 2>&1 | tail -1 || echo "(verify 警告)"

echo "[3] 打包 IPA"
rm -f /tmp/mw_realapp.ipa
( cd "$STAGE" && zip -r -X -q /tmp/mw_realapp.ipa Payload )

echo "[4] 装机"
xcrun devicectl device install app --device "$DEV_CD" /tmp/mw_realapp.ipa 2>&1 | grep -iE "Installing|complete|error|App installed|installationURL" | tail -3

echo "[5] 普通启动(--terminate-existing,无调试器)"
xcrun devicectl device process launch --device "$DEV_CD" --terminate-existing "$BID" 2>&1 | grep -iE "Launched|processIdentifier|error" | tail -2

echo "[6] 等 ${WAIT}s 让 app 跑"; S "$WAIT"

echo "[7] 拉日志(Documents/touchHLE_log.txt)"
LOG=/tmp/th_log_16pm.txt; rm -f "$LOG"
xcrun devicectl device copy from --device "$DEV_CD" \
  --domain-type appDataContainer --domain-identifier "$BID" \
  --source 'Documents/touchHLE_log.txt' --destination "$LOG" 2>&1 | tail -1

echo "===== 日志尾部 30 行 ====="
tail -30 "$LOG" 2>/dev/null
echo "===== 拦路诊断 ====="
if grep -qE "\[INTERP-UNIMPL\]" "$LOG" 2>/dev/null; then
  echo "▼ 未实现指令(最后一条 = 下一个要做的):"
  grep -E "\[INTERP-UNIMPL\]" "$LOG" | tail -3
elif grep -qE "\[DERAIL\]" "$LOG" 2>/dev/null; then
  echo "▼ 控制流脱轨(跳进零页):"
  grep -E "\[DERAIL\]" "$LOG" | tail -2
elif grep -qE "ios-present|\[splash\]|Renderer|CADisplayLink|present_frame" "$LOG" 2>/dev/null; then
  echo "✓✓ 出现渲染/后续日志 —— 越过 CPU 模拟瓶颈了!"
else
  echo "(无 UNIMPL/DERAIL —— 看上面尾部判断卡在哪)"
fi
