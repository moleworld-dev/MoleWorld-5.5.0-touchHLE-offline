#!/usr/bin/env bash
# 构建 Gate0 解释器基准 wasm。封装 Homebrew rustc 抢 PATH 无 wasm std 的坑:
# 必须显式把 RUSTC 指向 rustup stable(它装了 wasm32 std)。
set -euo pipefail
cd "$(dirname "$0")"

STABLE="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin"
if [ ! -x "$STABLE/rustc" ]; then
  echo "找不到 rustup stable rustc;请先 rustup toolchain install stable + rustup target add wasm32-unknown-unknown" >&2
  exit 1
fi
export RUSTC="$STABLE/rustc"
export CARGO="$STABLE/cargo"

echo "[1/3] 原生基准(可选,验证语义)"
"$CARGO" run --release --bin bench --manifest-path interp-bench/Cargo.toml || true

echo "[2/3] 编 wasm · verbatim"
"$CARGO" build --release --target wasm32-unknown-unknown --lib --manifest-path interp-bench/Cargo.toml
cp interp-bench/target/wasm32-unknown-unknown/release/interp_bench.wasm web/interp_bench.wasm

echo "[3/3] 编 wasm · strip_debug(生产天花板)"
"$CARGO" build --release --target wasm32-unknown-unknown --lib --features strip_debug --manifest-path interp-bench/Cargo.toml
cp interp-bench/target/wasm32-unknown-unknown/release/interp_bench.wasm web/interp_bench_stripped.wasm

echo "完成。web/interp_bench.wasm + web/interp_bench_stripped.wasm 已更新。"
echo "本地演示: python3 -m http.server 8777 --directory web  然后开 http://127.0.0.1:8777/index.html"
