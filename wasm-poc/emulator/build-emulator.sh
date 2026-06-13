#!/usr/bin/env bash
# 构建完整 touchHLE 模拟器到 wasm32-unknown-emscripten(M0/M0.5)。
# 产物:touchHLE.js(加载器)+ touchHLE.wasm。用 loader.html 在浏览器加载。
#
# 前置:
#   - emsdk 装在 ~/emsdk(emcc),先 `source ~/emsdk/emsdk_env.sh`
#   - rustup stable + `rustup target add wasm32-unknown-emscripten`
#   - ⚠️ Homebrew rustc 抢 PATH 且无 wasm std → 必须显式 RUSTC=rustup stable
set -euo pipefail

CRATE_DIR="$(cd "$(dirname "$0")/../../fresh-port/20-touchHLE-src/touchHLE" && pwd)"
STABLE="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"

if ! command -v emcc >/dev/null 2>&1; then
  echo "emcc 不在 PATH;请先: source ~/emsdk/emsdk_env.sh" >&2
  exit 1
fi
export RUSTC="$STABLE/rustc"
export CARGO="$STABLE/cargo"

cd "$CRATE_DIR"
echo "[*] 构建 touchHLE.wasm(emscripten + cpu_interpreter)…"
"$CARGO" build --bin touchHLE \
  --target wasm32-unknown-emscripten \
  --no-default-features --features cpu_interpreter \
  "${@}"   # 传 --release 可出优化版(小很多)

OUT="$CRATE_DIR/target/wasm32-unknown-emscripten/debug"
[ "${1:-}" = "--release" ] && OUT="$CRATE_DIR/target/wasm32-unknown-emscripten/release"
echo "[*] 产物:"
ls -lah "$OUT/touchHLE.js" "$OUT/touchHLE.wasm"
echo
echo "本地加载:"
echo "  cp $(dirname "$0")/loader.html $OUT/index.html"
echo "  python3 -m http.server 8785 --directory $OUT"
echo "  打开 http://127.0.0.1:8785/index.html"
