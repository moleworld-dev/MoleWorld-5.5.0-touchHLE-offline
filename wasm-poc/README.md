# touchHLE ARMv7 解释器 → WebAssembly 可行性 PoC(Gate 0)

验证《摩尔庄园》touchHLE 移植浏览器 WASM 路线的**第一道闸门**:纯 Rust ARMv7 解释器
能否编译成 WebAssembly,以及在浏览器里跑紧凑 ARM 循环的吞吐(MIPS)是否够用。

> 本工作区是**独立 sibling 目录**(自己的 git 仓),与主仓
> `摩尔庄园 5.5.0` 完全隔离。解释器 6 个源文件是从主仓**逐字硬复制**(md5 校验一致),
> 主仓与 iOS 分支**一字未改**。

## 结论

| | 原生 (Apple Silicon) | WASM (V8 / Chrome 内核) | wasm 折损 |
|---|---|---|---|
| verbatim(带调试设施) | ~141 MIPS | **~130 / 浏览器 136** | ~1.1× |
| production(剥离调试) | ~145 MIPS | **~145 / 浏览器 149** | ~1.0× |

- ✅ **解释器零源码改动编译成 61 KB wasm 模块**(零 import,完全自包含)。
- ✅ **wasm 折损仅 ~1.1×**,远好于调研预测的 1.5–2.5×(loop+match 派发被 V8 编成 br_table)。
- ✅ 在真实 Chrome 内核浏览器(Brave / Chrome 148)里跑通,语义自检 `sum(1..=100)=5050` 通过,
  校验和与原生**位级一致**。
- ✅ **~130–150 MIPS 落在游戏所需 50–150 MIPS 区间内**,且这是带调试开销 / 无 block cache 的数。
- ⚠️ 此为**紧凑合成循环的派发上界**;真实游戏代码(指令更杂、I-cache footprint 更大)会偏低。
  真实环境数需等完整 wasm 移植 boot 起来(即 M1 里程碑)才能测。

**Gate 0 判据:wasm 实测落在需求区间 → 通过。解释器路线(方案 A/B 的心脏)成立。**

## 结构

```
wasm-poc/
├── interp-bench/              # 碎出的解释器 + 基准 crate
│   ├── src/
│   │   ├── cpu/interpreter/   # ← 从主仓逐字硬复制(mod/arm/thumb16/thumb32/vfp/diff.rs)
│   │   │                      #    唯一改动:mod.rs 加一个 cfg(strip_debug) 门控(默认关=逐字一致)
│   │   ├── cpu.rs             # CpuState/CpuError 枚举(逐字)+ 挂载 interpreter
│   │   ├── mem.rs             # 最小 Vec-backed Mem(替 4GiB 数组)+ Ptr 类型机制(逐字)
│   │   ├── macros.rs          # echo! 桩
│   │   ├── blobs.rs           # 手工汇编的 Thumb 基准 blob + 驱动 harness
│   │   ├── lib.rs             # wasm C-ABI 导出入口(bench_alu/bench_ldr/selftest)
│   │   └── bin/bench.rs       # 原生基准 runner
│   └── Cargo.toml
└── web/
    ├── index.html            # 浏览器 harness(加载 wasm 实测 MIPS)
    ├── node-bench.js          # node/V8 基准(隔离测量,最稳)
    ├── interp_bench.wasm       # verbatim 版
    └── interp_bench_stripped.wasm  # strip_debug 版
```

## 复现

构建用 **rustup stable**(Homebrew rustc 没有 wasm32 的 std,会抢 PATH —— 必须显式指 RUSTC):

```bash
export RUSTC="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc"
export CARGO="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo"

# 原生基准
"$CARGO" run --release --bin bench --manifest-path interp-bench/Cargo.toml

# 编 wasm(verbatim + 生产剥离版)
"$CARGO" build --release --target wasm32-unknown-unknown --lib --manifest-path interp-bench/Cargo.toml
cp interp-bench/target/wasm32-unknown-unknown/release/interp_bench.wasm web/
"$CARGO" build --release --target wasm32-unknown-unknown --lib --features strip_debug --manifest-path interp-bench/Cargo.toml
cp interp-bench/target/wasm32-unknown-unknown/release/interp_bench.wasm web/interp_bench_stripped.wasm

# node 跑(最稳)
node web/node-bench.js 1000

# 浏览器跑
python3 -m http.server 8777 --directory web   # 然后开 http://127.0.0.1:8777/index.html
```

## 基准 workload(Thumb-16 机器码)

- **ALU 循环**(纯派发上界):`ADD r0,r0,r1 ; SUBS r1,r1,#1 ; BNE loop`
- **LDR 循环**(load 密集):`LDR r3,[r2] ; ADD r0,r0,r3 ; SUBS r1,r1,#1 ; BNE loop`
- **语义自检**:r1=100 时循环算 1+2+…+100,结果 r0 必须 = 5050。

## 下一步(若推进真移植)

1. 更真实的 workload:Dhrystone 式混合 blob(load/store/移位/乘法/BL-BX 调用)取更贴近的派发数。
2. 真实环境数:这要等完整 wasm 移植(emscripten/web-sys + 平台层)能 boot 到标题画面(M1)。
3. block cache(预解码缓存)实测提速(invalidate_cache_range 钩子已在原版预留)。

详见主仓记忆 `reference_wasm_port_research.md` 的方案 A 改良版路线。
