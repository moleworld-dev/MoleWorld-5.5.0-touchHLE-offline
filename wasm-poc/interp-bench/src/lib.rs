//! touchHLE ARMv7 解释器 —— WASM 可行性 PoC(Gate 0)。
//!
//! 本 crate 把原版 touchHLE 的纯 Rust 解释器(src/cpu/interpreter/ 逐字复制)碎出成一个
//! 独立单元,配一个 Vec-backed 的最小 Mem,验证:
//!   (1) 解释器能否原样编译到 wasm32;
//!   (2) 在 wasm 里跑紧凑 ARM 循环的吞吐(MIPS)。
//!
//! 解释器源文件(cpu/interpreter/*.rs)与原仓一字不差;此处仅提供编译所需的最小外围
//! (mem / cpu 枚举 / echo! 桩)和基准入口。不修改原仓任何文件。

#[macro_use]
mod macros;

pub mod blobs;
pub mod cpu;
pub mod mem;

// ===========================================================================
// 给 WASM/JS 用的 C-ABI 导出入口。整数进整数出,无需 wasm-bindgen。
// JS 侧用 performance.now() 计时,MIPS = (百万指令数 * 1e6) / 秒。
// 返回 r0 作为校验和,防止任何一侧把循环优化掉。
// ===========================================================================

/// 跑纯 ALU 循环 `million_instrs` 百万条指令,返回最终 r0(校验和)。
#[no_mangle]
pub extern "C" fn bench_alu(million_instrs: u32) -> u32 {
    let budget = million_instrs as u64 * 1_000_000;
    // r1 = 一个大奇数,使 r0 不断累加变化(强校验和),且循环不会在预算内终止。
    let res = blobs::run(blobs::ALU_LOOP, blobs::alu_regs(0x4000_0001), blobs::THUMB_CPSR, budget);
    res.r0
}

/// 跑 load 密集循环 `million_instrs` 百万条指令,返回最终 r0(校验和)。
#[no_mangle]
pub extern "C" fn bench_ldr(million_instrs: u32) -> u32 {
    let budget = million_instrs as u64 * 1_000_000;
    let res = blobs::run(blobs::LDR_LOOP, blobs::ldr_regs(0), blobs::THUMB_CPSR, budget);
    res.r0
}

/// 自检:跑一个会终止的 ALU 循环(r1=100),正确结果 r0 应为 5050。
/// 返回 r0;JS/原生侧断言 == 5050 即证明解释器在该环境语义正确。
#[no_mangle]
pub extern "C" fn selftest_sum_100() -> u32 {
    let res = blobs::run(blobs::ALU_LOOP, blobs::alu_regs(100), blobs::THUMB_CPSR, 10_000);
    res.r0
}
