#!/bin/bash
# build-ios.sh — 交叉编译 touchHLE iOS arm64(解释器后端,dynarmic 在 Xcode26 SDK 下编不过)。
set -euo pipefail
TOUCHHLE_DIR="$(cd "$(dirname "$0")/.." && pwd)"; cd "$TOUCHHLE_DIR"
SB="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
RT=$(ls /Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/lib/clang/*/lib/darwin/libclang_rt.ios.a 2>/dev/null | head -1)
export RUSTC="$SB/rustc"
export CARGO_TARGET_AARCH64_APPLE_IOS_RUSTFLAGS="-C link-arg=$RT"
export IPHONEOS_DEPLOYMENT_TARGET=13.0
export CMAKE_POLICY_VERSION_MINIMUM=3.5
export BOOST_ROOT=/opt/homebrew
export CMAKE_PREFIX_PATH=/opt/homebrew
echo "iOS 构建开始 $(date '+%H:%M:%S')  (libclang_rt=$RT)"
"$SB/cargo" build --release --target aarch64-apple-ios \
  --no-default-features --features static,cpu_interpreter --bin touchHLE
echo "iOS 构建完成 $(date '+%H:%M:%S')"
ls -la target/aarch64-apple-ios/release/touchHLE
