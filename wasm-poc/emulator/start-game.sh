#!/usr/bin/env bash
# 摩尔庄园 浏览器版 一键启动:构建(增量)→ 放启动页 → 起服务器 → 开浏览器。
#   ./start-game.sh             debug 构建后启动(默认,改代码调试用,慢)
#   ./start-game.sh --no-build  跳过构建直接启动(已构建过)
#   ./start-game.sh --release   release 构建后启动(性能版,首次编译久但跑得快很多)
#   PORT=9000 ./start-game.sh   换端口
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$(cd "$HERE/../../fresh-port/20-touchHLE-src/touchHLE" && pwd)"
STABLE="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
PORT="${PORT:-8800}"

MODE="debug"; BUILD=1
for a in "$@"; do
  case "$a" in
    --release) MODE="release" ;;
    --no-build) BUILD=0 ;;
  esac
done
OUT="$CRATE/target/wasm32-unknown-emscripten/$MODE"

# 1) 工具链:Homebrew rustc 没有 wasm std 且抢 PATH,必须显式指 rustup stable + 加载 emsdk。
if ! command -v emcc >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  source "$HOME/emsdk/emsdk_env.sh" >/dev/null 2>&1 || { echo "✗ 找不到 emsdk(~/emsdk)" >&2; exit 1; }
fi
export RUSTC="$STABLE/rustc" CARGO="$STABLE/cargo"

# 2) 构建(增量;debug 改完约 40s,release 首次很久但跑得快)
if [ "$BUILD" = 1 ]; then
  REL=""; [ "$MODE" = "release" ] && REL="--release"
  echo "[*] 构建 wasm($MODE,增量)…"
  ( cd "$CRATE" && "$CARGO" build --bin touchHLE $REL \
      --target wasm32-unknown-emscripten --no-default-features --features cpu_interpreter )
fi

[ -f "$OUT/touchHLE.js" ] || { echo "✗ 没有 $OUT/touchHLE.js —— 先构建一次(去掉 --no-build,或换 --release)" >&2; exit 1; }

# emscripten 的 preload 数据(touchHLE.data,字体/dylib)有时只落在 deps/ 而没拷到 profile
# 根目录(release 尤其),loader fetch touchHLE.data 会 404 卡在加载。这里补齐到根目录。
if [ ! -f "$OUT/touchHLE.data" ] && [ -f "$OUT/deps/touchHLE.data" ]; then
  cp "$OUT/deps/touchHLE.data" "$OUT/touchHLE.data"
  echo "[*] 补齐 touchHLE.data 到 $MODE 根目录"
fi

# 3) 放启动页;IPA 没有就从 debug 目录借一份(两个 mode 用同一个 IPA)
cp "$HERE/play.html" "$OUT/index.html"
if [ ! -f "$OUT/MoleWorld.ipa" ]; then
  ALT="$CRATE/target/wasm32-unknown-emscripten/debug/MoleWorld.ipa"
  if [ -f "$ALT" ]; then cp "$ALT" "$OUT/"; echo "[*] 从 debug 目录拷贝 MoleWorld.ipa"
  else
    echo "⚠ 没找到 $OUT/MoleWorld.ipa —— 请把解密后的 MoleWorld.ipa 拷到该目录:" >&2
    echo "    cp '路径/MoleWorld.ipa' '$OUT/'" >&2
  fi
fi

# 4) 起服务器 + 自动开浏览器
echo "[*] 打开 http://127.0.0.1:$PORT/"
( sleep 1.2; open "http://127.0.0.1:$PORT/" >/dev/null 2>&1 || true ) &
exec python3 "$HERE/serve.py" "$PORT" "$OUT"
