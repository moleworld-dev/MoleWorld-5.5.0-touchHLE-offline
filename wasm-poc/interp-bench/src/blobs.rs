//! 基准 workload(手工汇编的 Thumb-16 机器码)+ 驱动解释器执行的 harness。
//!
//! 这是本 PoC 新写的测试驱动,不是原仓代码。驱动模式抄自原版 diff.rs 的 run_interp:
//! 写指令到内存 → 设 PC=CODE_BASE → set_cpsr(thumb) → run_or_step(Some(budget))。

use crate::cpu::interpreter::InterpreterCpu;
use crate::cpu::CpuState;
use crate::mem::Mem;

pub const MEM_SIZE: usize = 16 * 1024 * 1024; // 16 MiB,够放代码+栈+数据
pub const NULL_PAGES: u32 = 0; // 与 diff.rs 一致:无 null 陷阱页
pub const CODE_BASE: u32 = 0x0001_0000; // 代码放这里(远离低地址)
pub const STACK_TOP: u32 = 0x0010_0000; // 1 MiB 处的栈顶
pub const DATA_BASE: u32 = 0x0020_0000; // 2 MiB 处的数据区
pub const CPSR_THUMB: u32 = 0x0000_0020; // CPSR.T
pub const CPSR_USER_MODE: u32 = 0x0000_0010; // User mode bits
/// 跑 thumb 代码用的初始 CPSR(User mode + Thumb,标志位清零)。
pub const THUMB_CPSR: u32 = CPSR_USER_MODE | CPSR_THUMB;

/// 纯 ALU + 分支循环(派发上界,无访存):
///   loop: ADD r0,r0,r1 ; SUBS r1,r1,#1 ; BNE loop
/// 每轮迭代 3 条指令。r1=0 时 SUBS 下溢成大数,循环不会在预算内结束。
#[rustfmt::skip]
pub const ALU_LOOP: &[u8] = &[
    0x08, 0x18, // 0x10000  ADD  r0, r0, r1   (0x1808)
    0x01, 0x39, // 0x10002  SUBS r1, r1, #1   (0x3901)
    0xFC, 0xD1, // 0x10004  BNE  0x10000      (0xD1FC, offset -8 from PC+4)
    0x00, 0xDF, // 0x10006  SVC  #0           (0xDF00) —— 终止哨兵
];

/// load 密集循环(模拟读内存的真实负载,固定地址不越界):
///   loop: LDR r3,[r2] ; ADD r0,r0,r3 ; SUBS r1,r1,#1 ; BNE loop
/// 每轮 4 条指令,其中 1 条是内存读。r2 指向 DATA_BASE。
#[rustfmt::skip]
pub const LDR_LOOP: &[u8] = &[
    0x13, 0x68, // 0x10000  LDR  r3, [r2, #0] (0x6813)
    0xC0, 0x18, // 0x10002  ADD  r0, r0, r3   (0x18C0)
    0x01, 0x39, // 0x10004  SUBS r1, r1, #1   (0x3901)
    0xFB, 0xD1, // 0x10006  BNE  0x10000      (0xD1FB, offset -10 from PC+4)
    0x00, 0xDF, // 0x10008  SVC  #0           (0xDF00)
];

pub struct BenchResult {
    pub r0: u32,
    pub instructions: u64,
    pub halted: bool, // true 表示遇到 SVC/Error 提前停了(用于正确性自检)
}

/// 通用驱动:把 blob 放到 CODE_BASE,设好初始寄存器/CPSR,跑最多 `budget` 条指令。
pub fn run(blob: &[u8], mut init_regs: [u32; 16], cpsr: u32, budget: u64) -> BenchResult {
    let mut mem = Mem::new(MEM_SIZE, NULL_PAGES);
    mem.write_blob(CODE_BASE, blob);
    // LDR_LOOP 会从 DATA_BASE 读;放一个固定值进去。
    mem.write_blob(DATA_BASE, &0x1234_5678u32.to_le_bytes());

    let mut cpu = InterpreterCpu::new(NULL_PAGES);
    init_regs[15] = CODE_BASE; // PC
    *cpu.regs_mut() = init_regs;
    cpu.set_cpsr(cpsr);

    let mut budget_left = budget;
    let state = cpu.run_or_step(&mut mem, Some(&mut budget_left));
    let executed = budget - budget_left;
    let halted = !matches!(state, CpuState::Normal) || budget_left > 0;

    BenchResult {
        r0: cpu.regs()[0],
        instructions: executed,
        halted,
    }
}

/// 构造 ALU 基准的初始寄存器。`r1` 决定循环计数(0 = 跑满预算)。
pub fn alu_regs(r1: u32) -> [u32; 16] {
    let mut regs = [0u32; 16];
    regs[1] = r1;
    regs[13] = STACK_TOP;
    regs
}

/// 构造 LDR 基准的初始寄存器。r2 指向数据区。
pub fn ldr_regs(r1: u32) -> [u32; 16] {
    let mut regs = [0u32; 16];
    regs[1] = r1;
    regs[2] = DATA_BASE;
    regs[13] = STACK_TOP;
    regs
}
