#!/usr/bin/env bash
# 摩尔庄园 浏览器版 一键启动:构建(增量)→ 放启动页 → 起服务器 → 开浏览器。
#   ./start-game.sh             构建后启动
#   ./start-game.sh --no-build  跳过构建直接启动(已构建过)
#   PORT=9000 ./start-game.sh   换端口
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$(cd "$HERE/../../fresh-port/20-touchHLE-src/touchHLE" && pwd)"
OUT="$CRATE/target/wasm32-unknown-emscripten/debug"
STABLE="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
PORT="${PORT:-8800}"

# 1) 工具链:Homebrew rustc 没有 wasm std 且抢 PATH,必须显式指 rustup stable + 加载 emsdk。
if ! command -v emcc >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  source "$HOME/emsdk/emsdk_env.sh" >/dev/null 2>&1 || { echo "✗ 找不到 emsdk(~/emsdk)" >&2; exit 1; }
fi
export RUSTC="$STABLE/rustc" CARGO="$STABLE/cargo"

# 2) 构建(增量,改了代码会重编;首次约几分钟,之后约 40s)
if [ "${1:-}" != "--no-build" ]; then
  echo "[*] 构建 wasm(增量)…"
  ( cd "$CRATE" && "$CARGO" build --bin touchHLE \
      --target wasm32-unknown-emscripten --no-default-features --features cpu_interpreter )
fi

[ -f "$OUT/touchHLE.js" ] || { echo "✗ 没有 $OUT/touchHLE.js —— 先构建一次(去掉 --no-build)" >&2; exit 1; }

# 3) 放启动页;检查 IPA
cp "$HERE/play.html" "$OUT/index.html"
if [ ! -f "$OUT/MoleWorld.ipa" ]; then
  echo "⚠ 没找到 $OUT/MoleWorld.ipa" >&2
  echo "  请把解密后的 MoleWorld.ipa 拷到这个目录,例如:" >&2
  echo "    cp '路径/MoleWorld.ipa' '$OUT/'" >&2
fi

# 4) 起服务器 + 自动开浏览器
echo "[*] 打开 http://127.0.0.1:$PORT/"
( sleep 1.2; open "http://127.0.0.1:$PORT/" >/dev/null 2>&1 || true ) &
exec python3 "$HERE/serve.py" "$PORT" "$OUT"
