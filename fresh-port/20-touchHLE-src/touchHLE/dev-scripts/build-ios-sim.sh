#!/bin/bash
set -euo pipefail
TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"; cd "$TOUCHHLE_DIR"
SB="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
RT=$(ls /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/*/lib/darwin/libclang_rt.iossim.a 2>/dev/null | head -1)
export RUSTC="$SB/rustc"
export CARGO_TARGET_AARCH64_APPLE_IOS_SIM_RUSTFLAGS="-C link-arg=$RT"
export IPHONEOS_DEPLOYMENT_TARGET=13.0 CMAKE_POLICY_VERSION_MINIMUM=3.5 BOOST_ROOT=/opt/homebrew CMAKE_PREFIX_PATH=/opt/homebrew
echo "sim 构建开始 $(date '+%H:%M:%S')"
"$SB/cargo" build --release --target aarch64-apple-ios-sim --no-default-features --features static,cpu_interpreter --bin touchHLE
echo "sim 构建完成 $(date '+%H:%M:%S')"
