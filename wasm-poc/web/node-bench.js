// node harness:在 V8(Chrome 同款引擎)里跑碎出的 touchHLE ARMv7 解释器,实测 wasm MIPS。
// 用法: node web/node-bench.js [百万指令数]
const fs = require("fs");
const path = require("path");

const WASM = path.join(__dirname, "..", "interp-bench", "target", "wasm32-unknown-unknown", "release", "interp_bench.wasm");
const MILLIONS = parseInt(process.argv[2] || "1000", 10); // 默认 10 亿条指令

(async () => {
  const bytes = fs.readFileSync(WASM);
  const { instance } = await WebAssembly.instantiate(bytes, {});
  const x = instance.exports;

  console.log(`== touchHLE ARMv7 解释器 · WASM 基准 (V8 / node ${process.version}) ==`);
  console.log(`wasm 模块大小: ${(bytes.length / 1024).toFixed(1)} KiB\n`);

  // ---- 语义自检:wasm 里跑 sum(1..=100) 必须 == 5050 ----
  const sum = x.selftest_sum_100();
  const ok = sum === 5050;
  console.log(`自检 sum(1..=100) = ${sum} (应为 5050) ${ok ? "✅" : "❌"}`);
  if (!ok) { console.error("解释器在 wasm 下语义错误,终止"); process.exit(1); }
  console.log("");

  const run = (name, fn) => {
    // 预热一次(让 V8 把 wasm 编成优化机器码)
    fn(50);
    const t0 = performance.now();
    const r0 = fn(MILLIONS);
    const secs = (performance.now() - t0) / 1000;
    const instrs = MILLIONS * 1e6;
    const mips = instrs / 1e6 / secs;
    console.log(`${name}\n    ${instrs.toLocaleString()} 条指令 / ${secs.toFixed(3)}s = ${mips.toFixed(1)} MIPS   (r0=0x${(r0>>>0).toString(16).padStart(8,"0")})`);
    return mips;
  };

  const aluMips = run("ALU  (ADD/SUBS/BNE, 纯派发上界)", x.bench_alu);
  const ldrMips = run("LDR  (LDR/ADD/SUBS/BNE, load 密集)", x.bench_ldr);

  console.log(`\n== 小结 ==`);
  console.log(`ALU ${aluMips.toFixed(0)} MIPS / LDR ${ldrMips.toFixed(0)} MIPS (V8 wasm)`);
})();
