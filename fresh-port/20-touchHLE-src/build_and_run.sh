#!/bin/bash
# 摩尔庄园 5.5.0 × touchHLE(离线移植)—— 一键构建 + 运行
#
# 用法:
#   bash build_and_run.sh            # 构建 + 运行(窗口,45秒后自动结束)
#   bash build_and_run.sh build      # 只构建
#   bash build_and_run.sh run [秒数] # 只运行
#
# 依赖:Homebrew 的 rust(cargo 1.95+,需支持 edition2024)、cmake、boost。
#   brew install rust cmake boost
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/touchHLE"
APP="$HERE/../01-cracked/Payload/MoleWorld.app"
CARGO=/opt/homebrew/bin/cargo

build() {
  echo "=== 构建 touchHLE (arm64, offline patch set) ==="
  ( cd "$SRC" \
    && CMAKE_PREFIX_PATH=/opt/homebrew BOOST_ROOT=/opt/homebrew \
       CMAKE_POLICY_VERSION_MINIMUM=3.5 RUSTUP_TOOLCHAIN= \
       "$CARGO" build --release --offline )
  local rc=$?
  if [ $rc -eq 0 ]; then echo "OK -> $SRC/target/release/touchHLE"; fi
  return $rc
}

run() {
  local secs="${1:-45}"
  local bin="$SRC/target/release/touchHLE"
  [ -x "$bin" ] || { echo "先构建:bash build_and_run.sh build"; exit 2; }
  cd "$SRC" || exit 3   # touchHLE 需 CWD 下有 touchHLE_dylibs/ touchHLE_fonts/
  echo "=== 运行 摩尔庄园(纯离线,${secs}s)==="
  "$bin" "$APP" --landscape-right --device-family=ipad &
  local pid=$!
  local n=0
  while kill -0 "$pid" 2>/dev/null && [ $n -lt "$secs" ]; do sleep 1; n=$((n+1)); done
  kill -INT "$pid" 2>/dev/null; sleep 1; kill -9 "$pid" 2>/dev/null
}

case "${1:-all}" in
  build) build ;;
  run)   run "${2:-45}" ;;
  all)   build && run "${2:-45}" ;;
  *)     echo "用法: bash build_and_run.sh [build|run [秒数]|all]" ;;
esac
