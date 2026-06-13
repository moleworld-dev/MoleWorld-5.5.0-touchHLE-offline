//! 原生基准 runner —— 测这台机器上解释器的 native MIPS 基线。
//! 与 wasm 版跑同一段 blob、同一驱动,wasm/native 比值 = wasm 折损。

use interp_bench::blobs;
use std::time::Instant;

fn main() {
    println!("== touchHLE ARMv7 解释器 · 原生基准 ==\n");

    // ---- 语义自检:r1=100 的循环,r0 应为 1+2+...+100 = 5050 ----
    let st = blobs::run(blobs::ALU_LOOP, blobs::alu_regs(100), blobs::THUMB_CPSR, 10_000);
    println!(
        "自检 sum(1..=100): r0={} (应为 5050), 提前停={}, 执行指令数={}",
        st.r0, st.halted, st.instructions
    );
    assert_eq!(st.r0, 5050, "❌ 解释器语义自检失败!分支偏移或标志位逻辑有问题");
    assert!(st.halted, "❌ 自检循环应当遇到 SVC 终止");
    println!("✅ 语义自检通过\n");

    // ---- 吞吐基准 ----
    let budget: u64 = 1_000_000_000; // 10 亿条指令
    let cases: &[(&str, &[u8], [u32; 16])] = &[
        ("ALU  (ADD/SUBS/BNE, 纯派发上界)", blobs::ALU_LOOP, blobs::alu_regs(0x4000_0001)),
        ("LDR  (LDR/ADD/SUBS/BNE, load 密集)", blobs::LDR_LOOP, blobs::ldr_regs(0)),
    ];

    for (name, blob, regs) in cases {
        let t = Instant::now();
        let res = blobs::run(blob, *regs, blobs::THUMB_CPSR, budget);
        let secs = t.elapsed().as_secs_f64();
        let mips = res.instructions as f64 / 1e6 / secs;
        println!(
            "{name}\n    {} 条指令 / {:.3}s = {:.1} MIPS   (r0={:#010x}, 提前停={})",
            res.instructions, secs, mips, res.r0, res.halted
        );
    }
}
