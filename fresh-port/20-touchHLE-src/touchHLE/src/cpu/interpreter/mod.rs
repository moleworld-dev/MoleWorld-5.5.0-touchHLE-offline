/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Pure-Rust ARMv7 interpreter CPU backend (used on iOS, where JIT is
//! impossible — iOS 18.4+/TXM forbids executing JIT pages even with a debugger
//! attached; see src/cpu.rs).
//!
//! P0 skeleton: instruction fetch (ARM + Thumb/Thumb-2 length decode), PC
//! advance, the host-call SVC trap (so `dyld`/`abi`/`environment` need no
//! changes), and null-page / undefined-instruction halts. Every other
//! instruction halts with [CpuError::UndefinedInstruction] after logging its
//! encoding — this `[INTERP-UNIMPL]` log is the work queue driving P1.

use crate::cpu::{CpuError, CpuState};
use crate::mem::{ConstVoidPtr, Mem, Ptr};

/// Differential test harness (only active with both CPU backends; see diff.rs).
#[allow(dead_code)]
mod diff;
mod arm;
mod thumb16;
mod thumb32;
mod vfp;

const CPSR_THUMB: u32 = 0x0000_0020;
const CPSR_USER_MODE: u32 = 0x0000_0010;
const PC: usize = 15;

/// CPU context for guest thread switches. Layout is the interpreter's own (only
/// the interpreter reads it); when the dynarmic backend is also compiled (P1
/// differential harness) this must be made bit-compatible with that backend's
/// context.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CpuContext {
    pub regs: [u32; 16],
    pub extregs: [u32; 64],
    pub cpsr: u32,
    pub fpscr: u32,
}

// `[u32; 64]` doesn't implement `Default` (std only does arrays up to 32), so
// derive won't work — implement it by hand.
impl Default for CpuContext {
    fn default() -> Self {
        CpuContext {
            regs: [0; 16],
            extregs: [0; 64],
            cpsr: 0,
            fpscr: 0,
        }
    }
}

impl CpuContext {
    pub fn new() -> Self {
        Self::default()
    }
}

pub struct InterpreterCpu {
    regs: [u32; 16],
    /// VFP/NEON register file (s0-s31 / d0-d31 alias), as raw words.
    extregs: [u32; 64],
    cpsr: u32,
    fpscr: u32,
    /// Bytes below this address are the guest null segment; any access faults.
    null_segment_size: u32,
    /// Local exclusive monitor address (LDREX/STREX). Single host thread, so we
    /// only track the address; STREX succeeds iff it matches a prior LDREX.
    excl_addr: Option<u32>,
    /// [P1 debug] ring buffer of the last executed (pc, insn) pairs, dumped when
    /// a fatal CPU error happens so we can see the trail INTO a bad address
    /// (a derail can run sequentially through garbage before faulting).
    trace: [(u32, u32); 64],
    trace_pos: usize,
    /// [P1 debug] instruction counter + previous (pc, insn), as plain fields
    /// (execution is single-threaded at any instant, so no atomics needed — this
    /// is a hot path, ~1 instruction's worth of work must stay cheap).
    dbg_n: u64,
    dbg_last_pc: u32,
    dbg_last_insn: u32,
    /// [hang debug] CCNode::visit 入口探针:命中次数 + 最近 16 个被 visit 的节点指针环。
    /// 卡死时 dump:环里反复出现同一批指针=循环遍历(cyclic/重复渲染),全是新指针=节点爆炸。
    visit_n: u64,
    visit_ring: [u32; 16],
    /// [hang debug] 0x20cc7a(vcmpe.f32 s18,#0 处)浮点探针计数:看 s22/s18(尺寸/缩放)
    /// 是不是真机上算成 0 或 NaN,从而 `bls` 走错分支导致死循环。
    fprobe_n: u64,
    /// [MoleWorld iOS] 实时对拍状态(仅诊断构建:两后端都编译)。保存上一条被武装指令的前态,
    /// 在下一条 step 顶部与解释器后态比对(见 diff::live_check)。
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_pre_regs: [u32; 16],
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_pre_cpsr: u32,
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_pre_extregs: [u32; 64],
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_pre_fpscr: u32,
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_insn: u32,
    #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
    diff_pending: bool,
    // P1: ITSTATE cache + PC->decoded-instruction cache.
}

impl InterpreterCpu {
    pub fn new(null_page_count: u32) -> Box<Self> {
        Box::new(InterpreterCpu {
            regs: [0; 16],
            extregs: [0; 64],
            cpsr: CPSR_USER_MODE,
            fpscr: 0,
            null_segment_size: null_page_count * 0x1000,
            excl_addr: None,
            trace: [(0, 0); 64],
            trace_pos: 0,
            dbg_n: 0,
            dbg_last_pc: 0,
            dbg_last_insn: 0,
            visit_n: 0,
            visit_ring: [0; 16],
            fprobe_n: 0,
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_pre_regs: [0; 16],
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_pre_cpsr: 0,
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_pre_extregs: [0; 64],
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_pre_fpscr: 0,
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_insn: 0,
            #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
            diff_pending: false,
        })
    }

    // Exclusive monitor (LDREX/STREX), single-thread semantics.
    pub(super) fn excl_set(&mut self, addr: u32) {
        self.excl_addr = Some(addr);
    }
    /// STREX: returns true (store should proceed, Rd=0) iff a prior LDREX marked
    /// this address. Always clears the monitor.
    pub(super) fn excl_check_clear(&mut self, addr: u32) -> bool {
        let ok = self.excl_addr == Some(addr);
        self.excl_addr = None;
        ok
    }
    pub(super) fn excl_clear(&mut self) {
        self.excl_addr = None;
    }

    pub fn regs(&self) -> &[u32; 16] {
        &self.regs
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        &mut self.regs
    }
    pub fn cpsr(&self) -> u32 {
        self.cpsr
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.cpsr = cpsr;
    }
    /// Extension (VFP/NEON) register file accessors — used by the diff harness
    /// to set up and compare VFP state against the dynarmic oracle.
    #[allow(dead_code)]
    pub fn extregs(&self) -> &[u32; 64] {
        &self.extregs
    }
    #[allow(dead_code)]
    pub fn extregs_mut(&mut self) -> &mut [u32; 64] {
        &mut self.extregs
    }
    #[allow(dead_code)]
    pub fn fpscr(&self) -> u32 {
        self.fpscr
    }
    #[allow(dead_code)]
    pub fn set_fpscr(&mut self, v: u32) {
        self.fpscr = v;
    }
    pub fn invalidate_cache_range(&mut self, _base: u32, _size: u32) {
        // P0: no-op. P1: clears the PC->decoded-instruction cache (dyld rewrites
        // stubs to SVCs and calls this).
    }

    pub fn swap_context(&mut self, ctx: &mut CpuContext) {
        std::mem::swap(&mut self.regs, &mut ctx.regs);
        std::mem::swap(&mut self.extregs, &mut ctx.extregs);
        std::mem::swap(&mut self.cpsr, &mut ctx.cpsr);
        std::mem::swap(&mut self.fpscr, &mut ctx.fpscr);
    }

    fn is_thumb(&self) -> bool {
        self.cpsr & CPSR_THUMB != 0
    }

    // ===== P1 flag helpers (ARM ARM pseudocode) =====
    /// AddWithCarry: returns (result, carry_out, overflow). SUB uses (x, !y, true).
    fn add_with_carry(x: u32, y: u32, carry_in: bool) -> (u32, bool, bool) {
        let usum = x as u64 + y as u64 + carry_in as u64;
        let result = usum as u32;
        let carry_out = (usum >> 32) & 1 != 0;
        let ssum = (x as i32 as i64) + (y as i32 as i64) + (carry_in as i64);
        let overflow = (result as i32 as i64) != ssum;
        (result, carry_out, overflow)
    }
    /// Set N,Z from result; leave C,V.
    fn set_nz(&mut self, result: u32) {
        self.cpsr &= !(0b11 << 30);
        self.cpsr |= (((result >> 31) & 1) << 31) | (((result == 0) as u32) << 30);
    }
    /// Set N,Z,C,V.
    fn set_nzcv(&mut self, result: u32, c: bool, v: bool) {
        self.cpsr &= !(0b1111 << 28);
        self.cpsr |= (((result >> 31) & 1) << 31)
            | (((result == 0) as u32) << 30)
            | ((c as u32) << 29)
            | ((v as u32) << 28);
    }
    /// Like [set_nzcv]/[set_nz] but a no-op inside an IT block. The 16-bit Thumb
    /// data-processing instructions (other than the explicit compares CMP/TST/
    /// CMN) have `setflags = !InITBlock()` — they must NOT touch the flags inside
    /// an IT block, or a flag-dependent later instruction in the same block (e.g.
    /// the second half of a stack-canary `cmp; itttt eq; …; itt eq; popeq`) sees
    /// the wrong condition.
    fn set_nzcv_dp(&mut self, result: u32, c: bool, v: bool) {
        if !self.in_it_block() {
            self.set_nzcv(result, c, v);
        }
    }
    fn set_nz_dp(&mut self, result: u32) {
        if !self.in_it_block() {
            self.set_nz(result);
        }
    }

    // ===== P1 foundation: register access, flags, shifts, conditions, memory =====
    // Shared by all instruction-group executors (thumb16/thumb32/arm/vfp).

    /// Read a register. R15 reads as PC + (4 in Thumb, 8 in ARM) per ARM ARM.
    #[allow(dead_code)]
    pub(super) fn get_reg(&self, n: usize) -> u32 {
        if n == 15 {
            self.regs[15].wrapping_add(if self.is_thumb() { 4 } else { 8 })
        } else {
            self.regs[n]
        }
    }
    /// Word-aligned R15 (for PC-relative loads): Align(PC, 4).
    #[allow(dead_code)]
    pub(super) fn get_reg_align(&self, n: usize) -> u32 {
        let v = self.get_reg(n);
        if n == 15 {
            v & !3
        } else {
            v
        }
    }
    #[allow(dead_code)]
    pub(super) fn set_reg(&mut self, n: usize, val: u32) {
        self.regs[n] = val;
    }

    pub(super) fn flag_n(&self) -> bool {
        self.cpsr & (1 << 31) != 0
    }
    pub(super) fn flag_z(&self) -> bool {
        self.cpsr & (1 << 30) != 0
    }
    pub(super) fn flag_c(&self) -> bool {
        self.cpsr & (1 << 29) != 0
    }
    pub(super) fn flag_v(&self) -> bool {
        self.cpsr & (1 << 28) != 0
    }
    pub(super) fn set_c_flag(&mut self, c: bool) {
        if c {
            self.cpsr |= 1 << 29;
        } else {
            self.cpsr &= !(1 << 29);
        }
    }

    /// LSL with carry-out (amount >= 1).
    pub(super) fn lsl_c(x: u32, n: u32) -> (u32, bool) {
        if n >= 32 {
            (0, if n == 32 { x & 1 != 0 } else { false })
        } else {
            (x << n, (x >> (32 - n)) & 1 != 0)
        }
    }
    /// LSR with carry-out (amount >= 1).
    pub(super) fn lsr_c(x: u32, n: u32) -> (u32, bool) {
        if n >= 32 {
            (0, if n == 32 { (x >> 31) & 1 != 0 } else { false })
        } else {
            (x >> n, (x >> (n - 1)) & 1 != 0)
        }
    }
    /// ASR with carry-out (amount >= 1).
    pub(super) fn asr_c(x: u32, n: u32) -> (u32, bool) {
        if n >= 32 {
            let r = (x as i32 >> 31) as u32;
            (r, (x >> 31) & 1 != 0)
        } else {
            ((x as i32 >> n) as u32, (x >> (n - 1)) & 1 != 0)
        }
    }
    /// ROR with carry-out (amount != 0).
    pub(super) fn ror_c(x: u32, n: u32) -> (u32, bool) {
        let m = n & 31;
        if m == 0 {
            (x, (x >> 31) & 1 != 0)
        } else {
            let r = x.rotate_right(m);
            (r, (r >> 31) & 1 != 0)
        }
    }
    /// RRX with carry-out.
    pub(super) fn rrx_c(x: u32, carry_in: bool) -> (u32, bool) {
        let r = (x >> 1) | ((carry_in as u32) << 31);
        (r, x & 1 != 0)
    }
    /// Generic Shift_C. stype: 0=LSL 1=LSR 2=ASR 3=ROR. amount==0 → (x, carry_in).
    /// (RRX is stype==3 with amount==0 handled by caller via rrx_c.)
    pub(super) fn shift_c(x: u32, stype: u32, amount: u32, carry_in: bool) -> (u32, bool) {
        if amount == 0 {
            return (x, carry_in);
        }
        match stype & 3 {
            0 => Self::lsl_c(x, amount),
            1 => Self::lsr_c(x, amount),
            2 => Self::asr_c(x, amount),
            _ => Self::ror_c(x, amount),
        }
    }

    /// Evaluate an ARM condition code against current NZCV.
    pub(super) fn cond_passed(&self, cond: u32) -> bool {
        let (n, z, c, v) = (self.flag_n(), self.flag_z(), self.flag_c(), self.flag_v());
        match cond & 0xF {
            0x0 => z,
            0x1 => !z,
            0x2 => c,
            0x3 => !c,
            0x4 => n,
            0x5 => !n,
            0x6 => v,
            0x7 => !v,
            0x8 => c && !z,
            0x9 => !c || z,
            0xA => n == v,
            0xB => n != v,
            0xC => !z && (n == v),
            0xD => z || (n != v),
            _ => true, // AL (0xE) and 0xF
        }
    }

    // ===== P1 Group 4: Thumb IT-block (ITSTATE in CPSR[15:10] + CPSR[26:25]) =====
    /// Read ITSTATE[7:0]: [1:0] = CPSR[26:25], [7:2] = CPSR[15:10].
    pub(super) fn itstate(&self) -> u8 {
        let lo = (self.cpsr >> 25) & 0b11;
        let hi = (self.cpsr >> 10) & 0b11_1111;
        ((hi << 2) | lo) as u8
    }
    pub(super) fn set_itstate(&mut self, it: u8) {
        let it = it as u32;
        self.cpsr &= !((0b11_1111 << 10) | (0b11 << 25));
        self.cpsr |= ((it >> 2) & 0b11_1111) << 10;
        self.cpsr |= (it & 0b11) << 25;
    }
    /// In an IT block iff the low 4 bits of ITSTATE are nonzero.
    pub(super) fn in_it_block(&self) -> bool {
        self.itstate() & 0x0f != 0
    }
    /// ITAdvance() per ARM ARM: shift ITSTATE[4:0] left, or clear when done.
    pub(super) fn it_advance(&mut self) {
        let it = self.itstate();
        if it & 0b111 == 0 {
            self.set_itstate(0);
        } else {
            let new = (it & 0b1110_0000) | ((it << 1) & 0b0001_1111);
            self.set_itstate(new);
        }
    }

    /// Set PC from a value, switching ARM/Thumb by bit0 (BX/BLX/POP{pc}/etc).
    pub(super) fn bx_write_pc(&mut self, val: u32) {
        if val & 1 != 0 {
            self.cpsr |= CPSR_THUMB;
        } else {
            self.cpsr &= !CPSR_THUMB;
        }
        self.regs[PC] = val & !1;
    }

    // ----- data memory access (fault-aware; None/false = MemoryError) -----
    pub(super) fn data_r_u32(&self, mem: &Mem, addr: u32) -> Option<u32> {
        if addr < self.null_segment_size {
            return None;
        }
        let b = mem.get_bytes_fallible(Ptr::from_bits(addr), 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub(super) fn data_r_u16(&self, mem: &Mem, addr: u32) -> Option<u16> {
        if addr < self.null_segment_size {
            return None;
        }
        let b = mem.get_bytes_fallible(Ptr::from_bits(addr), 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    pub(super) fn data_r_u8(&self, mem: &Mem, addr: u32) -> Option<u8> {
        if addr < self.null_segment_size {
            return None;
        }
        let b = mem.get_bytes_fallible(Ptr::from_bits(addr), 1)?;
        Some(b[0])
    }
    pub(super) fn data_w_u32(&self, mem: &mut Mem, addr: u32, val: u32) -> bool {
        if addr < self.null_segment_size {
            return false;
        }
        match mem.get_bytes_fallible_mut(Ptr::from_bits(addr), 4) {
            Some(b) => {
                b.copy_from_slice(&val.to_le_bytes());
                true
            }
            None => false,
        }
    }
    pub(super) fn data_w_u16(&self, mem: &mut Mem, addr: u32, val: u16) -> bool {
        if addr < self.null_segment_size {
            return false;
        }
        match mem.get_bytes_fallible_mut(Ptr::from_bits(addr), 2) {
            Some(b) => {
                b.copy_from_slice(&val.to_le_bytes());
                true
            }
            None => false,
        }
    }
    pub(super) fn data_w_u8(&self, mem: &mut Mem, addr: u32, val: u8) -> bool {
        if addr < self.null_segment_size {
            return false;
        }
        match mem.get_bytes_fallible_mut(Ptr::from_bits(addr), 1) {
            Some(b) => {
                b[0] = val;
                true
            }
            None => false,
        }
    }

    // ----- VFP/NEON extension register access -----
    // extregs[64] models d0..d31 (VFPv3-D32). s_n = extregs[n] for n<32; d_n =
    // (extregs[2n] low, extregs[2n+1] high), so d0..d15 alias s0..s31.
    pub(super) fn get_sreg(&self, n: usize) -> u32 {
        self.extregs[n]
    }
    pub(super) fn set_sreg(&mut self, n: usize, v: u32) {
        self.extregs[n] = v;
    }
    pub(super) fn get_dreg(&self, n: usize) -> u64 {
        (self.extregs[2 * n] as u64) | ((self.extregs[2 * n + 1] as u64) << 32)
    }
    pub(super) fn set_dreg(&mut self, n: usize, v: u64) {
        self.extregs[2 * n] = v as u32;
        self.extregs[2 * n + 1] = (v >> 32) as u32;
    }
    pub(super) fn get_s_f32(&self, n: usize) -> f32 {
        f32::from_bits(self.extregs[n])
    }
    pub(super) fn set_s_f32(&mut self, n: usize, v: f32) {
        self.extregs[n] = v.to_bits();
    }
    pub(super) fn get_d_f64(&self, n: usize) -> f64 {
        f64::from_bits(self.get_dreg(n))
    }
    pub(super) fn set_d_f64(&mut self, n: usize, v: f64) {
        self.set_dreg(n, v.to_bits());
    }

    /// Fetch a code halfword. `None` = fetch fault (null page / unmapped).
    fn read_code_u16(&self, mem: &Mem, addr: u32) -> Option<u16> {
        if addr < self.null_segment_size {
            return None;
        }
        let p: ConstVoidPtr = Ptr::from_bits(addr);
        let b = mem.get_bytes_fallible(p, 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }
    fn read_code_u32(&self, mem: &Mem, addr: u32) -> Option<u32> {
        if addr < self.null_segment_size {
            return None;
        }
        let p: ConstVoidPtr = Ptr::from_bits(addr);
        let b = mem.get_bytes_fallible(p, 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn run_or_step(&mut self, mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        match ticks {
            None => self.step_one(mem),
            Some(budget) => loop {
                let st = self.step_one(mem);
                *budget = budget.saturating_sub(1);
                match st {
                    CpuState::Normal if *budget > 0 => continue,
                    CpuState::Normal => return CpuState::Normal,
                    halt => return halt, // Svc / Error: return immediately
                }
            },
        }
    }

    /// [P1 debug] dump the recent-instruction ring buffer (oldest → newest).
    pub fn dump_trace(&self) {
        echo!("[TRACE] last {} executed insns (old → new):", self.trace.len());
        for i in 0..self.trace.len() {
            let (p, ins) = self.trace[(self.trace_pos + i) % self.trace.len()];
            if p != 0 {
                echo!("  {:#010x} : {:#010x}", p, ins);
            }
        }
    }

    fn step_one(&mut self, mem: &mut Mem) -> CpuState {
        let pc = self.regs[PC];
        let thumb = self.is_thumb();

        // ---- instruction fetch (variable length) ----
        let (insn, len): (u32, u32) = if thumb {
            let Some(hw0) = self.read_code_u16(mem, pc) else {
                return CpuState::Error(CpuError::MemoryError);
            };
            // Thumb-2 32-bit if top 5 bits are 0b11101 / 0b11110 / 0b11111.
            let is32 = (hw0 & 0xf800) >= 0xe800;
            if is32 {
                let Some(hw1) = self.read_code_u16(mem, pc + 2) else {
                    return CpuState::Error(CpuError::MemoryError);
                };
                ((hw0 as u32) << 16 | hw1 as u32, 4)
            } else {
                (hw0 as u32, 2)
            }
        } else {
            let Some(w) = self.read_code_u32(mem, pc) else {
                return CpuState::Error(CpuError::MemoryError);
            };
            (w, 4)
        };

        // ---- P0: recognise only the host-call ARM SVC ----
        // dyld encodes host functions as `svc #imm` (encode_a32_svc = imm |
        // 0xef000000). Do NOT execute any syscall semantics here — just stash
        // the svc number and halt; environment::handle_cpu_state dispatches it
        // (and reconstructs the svc address via PC-4, so PC must be past it).
        if !thumb && (insn & 0xff00_0000) == 0xef00_0000 {
            let imm24 = insn & 0x00ff_ffff;
            self.regs[PC] = pc.wrapping_add(4);
            return CpuState::Svc(imm24);
        }

        // [TEX-FASTPATH] 摩尔庄园专用:-[CCTexture2D initWithImage:resolutionType:] 的
        // RGBA8888->RGBA4444 转换内循环(0x2e7102: ldr.w r6,[r3],#4 ... strh r0,[r5],#2 / bne)。
        // 该循环按像素逐次跑,几百张大纹理(单张达 2048²=4M 像素)累计上亿次迭代,在解释器上
        // = 启动黑屏数分钟。检测到该循环(精确 PC + 精确指令字 + 整段范围预校验)就用原生 Rust
        // 整段转换(位运算逐位复刻 0x2e7108-0x2e711c 的 and/orr/移位,bit-for-bit 等价),约 100x。
        // 任一守卫不符则落普通解释。属游戏特定优化(硬编码 PC),仅对本游戏二进制生效。
        if pc == 0x002e7102 && insn == 0xf853_6b04 {
            let count = self.regs[4];
            let src0 = self.regs[3];
            let dst0 = self.regs[5];
            let src_ok = src0 >= self.null_segment_size
                && (src0 as u64) + 4u64 * (count as u64) <= 0x1_0000_0000;
            let dst_ok = dst0 >= self.null_segment_size
                && (dst0 as u64) + 2u64 * (count as u64) <= 0x1_0000_0000;
            if count > 0 && src_ok && dst_ok {
                let mut src = src0;
                let mut dst = dst0;
                let mut r6 = 0u32;
                let mut r0 = 0u32;
                let mut r1 = 0u32;
                for _ in 0..count {
                    r6 = self.data_r_u32(mem, src).unwrap_or(0);
                    r0 = (r6 << 8) & 0xf000;
                    r1 = (r6 >> 4) & 0xf00;
                    r1 |= r6 >> 28;
                    r0 |= r1;
                    r1 = (r6 >> 16) & 0xf0;
                    r0 |= r1;
                    self.data_w_u16(mem, dst, r0 as u16);
                    src = src.wrapping_add(4);
                    dst = dst.wrapping_add(2);
                }
                // 复刻循环退出态:r3/r5 推进、r4=0、r6/r0/r1=末轮;末轮 subs r4(1->0)留 Z=1/C=1;
                // PC 落 bne 之后的 0x2e7126(b 0x2e71b2)。
                self.regs[3] = src;
                self.regs[5] = dst;
                self.regs[4] = 0;
                self.regs[6] = r6;
                self.regs[0] = r0;
                self.regs[1] = r1;
                self.set_nzcv(0, true, false);
                self.regs[PC] = 0x002e7126;
                return CpuState::Normal;
            }
        }

        // [MoleWorld iOS] 实时 lockstep 对拍(仅诊断构建:cpu_dynarmic+cpu_interpreter)。
        // 被键武装后,对每条游戏代码区(0x100000–0x900000,排除纹理快路径 0x2e7102)的指令:
        // 在【下一条 step 顶部】用 dynarmic 从上一条的前态重跑、与解释器后态比寄存器+标志位,
        // 分歧打印 [LIVE-DIFF]——精准抓出 iOS-only 解释器算错的那一条。fresh dynarmic 每条很慢,
        // 故只在武装+游戏区时跑;用户在好友界面按键武装、点丝尔特即可。
        #[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
        {
            // 无 GUI 插桩的武装方式:每 ~1M 条指令探一次文件,`touch /tmp/touchHLE_diff_arm`
            // 即武装、`rm` 即解除。用户在好友界面 touch 该文件、稍等一下再点丝尔特即可。
            {
                static CHK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
                if CHK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) & 0x000f_ffff == 0 {
                    let armed = std::path::Path::new("/tmp/touchHLE_diff_arm").exists();
                    diff::DIFF_ARMED.store(armed, std::sync::atomic::Ordering::Relaxed);
                }
            }
            if self.diff_pending {
                self.diff_pending = false;
                let pre_regs = self.diff_pre_regs;
                let pre_cpsr = self.diff_pre_cpsr;
                let pre_extregs = self.diff_pre_extregs;
                let pre_fpscr = self.diff_pre_fpscr;
                let prev_insn = self.diff_insn;
                let post_regs = self.regs;
                let post_cpsr = self.cpsr();
                let post_extregs = self.extregs;
                let post_fpscr = self.fpscr;
                diff::live_check(
                    mem,
                    &pre_regs,
                    pre_cpsr,
                    &pre_extregs,
                    pre_fpscr,
                    &post_regs,
                    post_cpsr,
                    &post_extregs,
                    post_fpscr,
                    prev_insn,
                );
            }
            if diff::DIFF_ARMED.load(std::sync::atomic::Ordering::Relaxed)
                && (0x0010_0000..0x0090_0000).contains(&pc)
                && pc != 0x002e_7102
            {
                self.diff_pre_regs = self.regs;
                self.diff_pre_cpsr = self.cpsr();
                self.diff_pre_extregs = self.extregs;
                self.diff_pre_fpscr = self.fpscr;
                self.diff_insn = insn;
                self.diff_pending = true;
            }
        }

        // [MoleWorld iOS · P0 修复] 头像动画"追帧"死循环防护。
        // AnimPlayer::updateDt:(0x20f4e4)有个追帧循环:
        //   while timeAcc >= frameDuration { timeAcc -= frameDuration; 推进帧+建精灵; frameDuration = getDuration() }
        // 终止唯一条件是 frameDuration > timeAcc。真机(原生 GLES1)下点好友渲染头像时,某帧的
        // frameDuration([ASprite GetAFrameTime:aframe:] 经 getDuration)被取成 0:于是 timeAcc-=0
        // 永不减小、timeAcc>=0 恒真 → 循环永不退出,每轮 wrap 帧并新建精灵 → 单个 drawScene 永不返回、
        // 从不 present、精灵无限累积 → 整局冻死。桌面(GLES1-on-GL2)同一份解释器/数据不触发。
        // 0 或负的"帧时长"语义上非法;在循环的两个比较点(0x20f540 入口、0x20f5d2 回边,均 r0=frameDuration、
        // r1=timeAcc、r4=self、r5=timeAcc 的 ivar 偏移)把它钳到 ≥1,保证 timeAcc 每轮至少减 1、循环必然终止。
        // 同时在入口把异常巨大的累积时间封顶(防 dt 爆炸的超长追帧),只丢弃一次积压(跳一帧动画,无害)。
        // 命中范围极窄(仅该函数这两条指令),对其他指令/游戏零影响。这是 touchHLE 侧根因级防护,
        // 编进所有构建,无需改 guest 二进制。
        if pc & 0xffff_ff00 == 0x0020_f500 && (pc == 0x0020_f540 || pc == 0x0020_f5d2) {
            if (self.regs[0] as i32) <= 0 {
                self.regs[0] = 1; // frameDuration 钳到 ≥1
            }
            // 入口点:累积时间封顶(正常仅几个帧单位;>0x800 视为异常 dt 积压)。
            if pc == 0x0020_f540 && self.regs[1] > 0x0000_0800 {
                self.regs[1] = self.regs[0]; // timeAcc←frameDuration,循环只跑一轮即退出
            }
        }

        // [hang debug] 轻量心跳:每 ~4M 条指令打印 CPU 当前位置(pc/lr/sp)。卡死时连续
        // 心跳的 pc 会聚在一个小范围=循环体,配 lr 可定位到具体函数。只 +1 计数 +1 分支,
        // ~1.05x(对比 interp_debug 的 ~40x,因为不写 trace 环、不做 DERAIL 检测)。
        // 用 `--features interp_hb` 单开抓卡死,不拖慢正常 release。
        #[cfg(any(feature = "interp_debug", feature = "interp_hb", debug_assertions))]
        {
            self.dbg_n = self.dbg_n.wrapping_add(1);
            // [hang debug] 影子调用栈 = 穿透 objc 跳板还原 guest 真实调用链。
            // 思路:每条指令跨步比对——若【上一条】把 LR 设成了"自己之后"(ppc+2/+4)且现在 pc 跳走了,
            // 上一条就是 BL/BLX(调用)→ push 返回地址。控制流回到任一已 push 的返回地址(bx lr / pop pc /
            // 甚至 msgSend 跳板续跑回调用点)→ pop。如此维护的栈条目全是【真·游戏代码返回地址】(不是
            // 0x3000a000 跳板)。心跳时 dump:死循环期间多条心跳的【共同稳定底层帧】= 驱动那个 while 的函数链。
            {
                use std::cell::{Cell, RefCell};
                thread_local! {
                    static SH: RefCell<Vec<u32>> = RefCell::new(Vec::new());
                    static SH_PREVPC: Cell<u32> = const { Cell::new(0) };
                    static SH_PREVLR: Cell<u32> = const { Cell::new(0) };
                }
                let ppc = SH_PREVPC.with(|c| c.replace(pc));
                let _plr = SH_PREVLR.with(|c| c.replace(self.regs[14]));
                // ★thumb 返回地址带 bit0=1(LR=(pc+len)|1),必须 &!1 再比,否则永远不命中=栈空。
                let lr = self.regs[14] & !1u32;
                // 上一条 = 调用(BL/BLX):LR 现在 = ppc 之后(ppc+2 或 ppc+4),且本条 pc 跳走了。
                let seq = pc == ppc.wrapping_add(2) || pc == ppc.wrapping_add(4);
                let lr_is_ret = lr == ppc.wrapping_add(2) || lr == ppc.wrapping_add(4);
                if ppc != 0 && lr_is_ret && !seq {
                    SH.with(|s| {
                        let mut s = s.borrow_mut();
                        if s.len() < 1024 {
                            s.push(lr);
                        }
                    });
                }
                // 返回:控制回到某个已 push 的返回地址 → 弹出(可一次弹多层,处理跳板/longjmp)。
                SH.with(|s| {
                    let mut s = s.borrow_mut();
                    let mut hops = 0;
                    while let Some(&top) = s.last() {
                        if top == pc && hops < 64 {
                            s.pop();
                            hops += 1;
                        } else {
                            break;
                        }
                    }
                });
                if self.dbg_n & 0x003f_ffff == 0 {
                    let (depth, chain) = SH.with(|s| {
                        let s = s.borrow();
                        let depth = s.len();
                        let chain = s
                            .iter()
                            .rev()
                            .take(20)
                            .map(|a| format!("{:#x}", a))
                            .collect::<Vec<_>>()
                            .join(" ");
                        (depth, chain)
                    });
                    echo!("[SHADOW] pc={:#010x} depth={} chain={}", pc, depth, chain);
                }
            }
            // [hang debug] CCNode::visit(0x2d30cc)入口探针:把 self(r0)塞进 16 槽环,
            // 每 0x40000 次入口 dump 环 + 去重计数。uniq 小=反复 visit 同一批节点(cyclic/
            // 重复渲染),uniq=16=每次都是新节点(树爆炸/不断新建)。直接判定死循环形态。
            if pc == 0x002d_30cc {
                let slot = (self.visit_n as usize) & 15;
                self.visit_ring[slot] = self.regs[0];
                self.visit_n = self.visit_n.wrapping_add(1);
                if self.visit_n & 0x0003_ffff == 0 {
                    let mut uniq = 0u32;
                    for i in 0..16 {
                        if !self.visit_ring[..i].contains(&self.visit_ring[i]) {
                            uniq += 1;
                        }
                    }
                    echo!(
                        "[VISIT] n={:#x} uniq={}/16 ring={:08x?}",
                        self.visit_n, uniq, self.visit_ring
                    );
                }
            }
            // [hang debug] 0x20cc7a 处 s22(0x20cc66 vcvt 出)与 s18(0x20cc76 vcvt 出)都已就绪,
            // 正是两个 `vcmpe.f32 ...,#0` 尺寸守卫的操作数。真机上若为 0/NaN → bls 走错 → 卡死。
            if pc == 0x0020_cc7a {
                self.fprobe_n = self.fprobe_n.wrapping_add(1);
                if self.fprobe_n & 0x0003_ffff == 0 {
                    echo!(
                        "[FPROBE] n={:#x} s22={}({:#010x}) s18={}({:#010x})",
                        self.fprobe_n,
                        f32::from_bits(self.extregs[22]),
                        self.extregs[22],
                        f32::from_bits(self.extregs[18]),
                        self.extregs[18]
                    );
                }
            }
            // [hang debug] AnimPlayer::updateDt:(0x20f4e4)追帧循环探针。
            // 0x20f540 = 进循环前的初始 cmp:r0=frameDuration(getDuration返回), r1=timeAcc,
            //            r2=dt(本次帧增量), r4=self。一次性打印 → 看 dt 是否巨大、frameDuration 是否 0。
            if pc == 0x0020_f540 {
                echo!(
                    "[ANIMDT] enter self={:#x} dt(r2)={} timeAcc(r1)={} frameDur(r0)={}",
                    self.regs[4], self.regs[2] as i32, self.regs[1] as i32, self.regs[0] as i32
                );
            }
            // 0x20f5d2 = 循环回边前的 cmp:r0=frameDuration(刚 getDuration), r1=timeAcc。
            // 每 0x40000 次打印 → 看死循环里 frameDuration 是否恒 0、timeAcc 怎么变。
            if pc == 0x0020_f5d2 {
                self.fprobe_n = self.fprobe_n.wrapping_add(1);
                if self.fprobe_n & 0x0003_ffff == 0 {
                    echo!(
                        "[ANIMLOOP] n={:#x} timeAcc(r1)={} frameDur(r0)={} curFrame?={:#x}",
                        self.fprobe_n, self.regs[1] as i32, self.regs[0] as i32, self.regs[11]
                    );
                }
            }
            if self.dbg_n & 0x003f_ffff == 0 {
                echo!(
                    "[HEARTBEAT] n={:#x} pc={:#010x} lr={:#x} sp={:#x} r4={:#x} itstate={:#04x} inIT={} z={}",
                    self.dbg_n, pc, self.regs[14], self.regs[13], self.regs[4],
                    self.itstate(), self.in_it_block(), self.flag_z()
                );
                // [hang debug] 走 Apple ARM 帧指针(r7)链,打印返回地址栈。卡死时
                // 多条心跳的栈外层(共同后缀)= 稳定调用路径 → 直接定位反复调用渲染的
                // 驱动者函数(比单层 lr 强得多)。
                let mut fp = self.regs[7];
                let mut chain = String::new();
                for _ in 0..24 {
                    if fp < 0x1000 || (fp & 3) != 0 {
                        break;
                    }
                    let Some(ret) = self.data_r_u32(mem, fp.wrapping_add(4)) else {
                        break;
                    };
                    let Some(next) = self.data_r_u32(mem, fp) else {
                        break;
                    };
                    chain.push_str(&format!(" {:#x}", ret));
                    if next <= fp {
                        break;
                    }
                    fp = next;
                }
                echo!("[STACK]{}", chain);
                // [hang debug] r7 链全是 msgSend 跳板(0x3000a000)时没用。直接从 sp 往上扫
                // 原始栈内存,挑落在 __text [0x4000,0x9c8000) 且 bit0=1(thumb 返回地址)的字
                // = 真实调用链。多条心跳的共同返回地址 = 反复执行的循环驱动函数。
                let mut scan = String::new();
                let mut a = self.regs[13] & !3;
                let top = a.wrapping_add(0xc00);
                let mut found = 0;
                while a < top && found < 32 {
                    if let Some(w) = self.data_r_u32(mem, a) {
                        if (0x4000..0x009c_8000).contains(&w) && (w & 1) == 1 {
                            scan.push_str(&format!(" {:#x}", w));
                            found += 1;
                        }
                    }
                    a = a.wrapping_add(4);
                }
                echo!("[STACKSCAN]{}", scan);
            }
        }

        // [P1 debug] log first few instructions, and any jump into the stack
        // region (control-flow bug) together with the PREVIOUS instruction.
        #[cfg(any(feature = "interp_debug", debug_assertions))]
        {
            let lpc = self.dbg_last_pc;
            let linsn = self.dbg_last_insn;
            self.dbg_last_pc = pc;
            self.dbg_last_insn = insn;
            // Ring buffer of recent instructions (dumped on fatal error below).
            let tp = self.trace_pos;
            self.trace[tp] = (pc, insn);
            self.trace_pos = (tp + 1) % self.trace.len();
            // Catch a NON-sequential control-flow change (branch/return) that lands
            // somewhere it can't be real code:
            //   * a ZERO word = jumped into uninitialized memory, or
            //   * the high stack region (>= 0xe000_0000) = a corrupted return
            //     address / function pointer (the guest stack lives at the very
            //     top of the 32-bit space; no code maps there).
            // Valid code (insn != 0) at any lower address is fine — libraries
            // load at both low (~0x001f_xxxx) and high (~0x3748_xxxx) addresses.
            // A derail here is NOT a wild jump but a fall-through: control ran
            // off the end of real code into zero padding and NOP-slides upward
            // (0x00000000 = ARM `andeq r0,r0,r0`, executed as a conditional
            // no-op), so it's SEQUENTIAL and `!seq` would miss it. Catch the
            // very first transition from a real instruction (linsn != 0) onto a
            // zero word (insn == 0): linsn/lpc is then the culprit — the branch
            // or return that should have redirected control but didn't. Also
            // backstop on pc reaching the unmapped high region.
            if (insn == 0 && linsn != 0 && lpc != 0) || pc >= 0xe000_0000 {
                echo!(
                    "[DERAIL] pc={:#010x} insn={:#x} | CULPRIT lpc={:#010x} linsn={:#x} sp={:#x} lr={:#x} r7={:#x}",
                    pc, insn, lpc, linsn, self.regs[13], self.regs[14], self.regs[7]
                );
                self.dump_trace();
                self.regs[PC] = pc;
                return CpuState::Error(CpuError::UndefinedInstruction);
            }
        }

        // ---- P1 Group 4: Thumb IT-block ----
        // The IT instruction itself (1011 1111 firstcond mask, mask != 0) sets up
        // the block. Hints (mask == 0: NOP/YIELD/...) fall through to exec_thumb16.
        if thumb && len == 2 && (insn & 0xff00) == 0xbf00 && (insn & 0x000f) != 0 {
            self.set_itstate((insn & 0x00ff) as u8);
            self.regs[PC] = pc.wrapping_add(2);
            return CpuState::Normal;
        }
        // Instructions inside an IT block take their condition from ITSTATE[7:4].
        let in_it = self.in_it_block();
        if in_it && !self.cond_passed((self.itstate() >> 4) as u32 & 0xf) {
            // Condition false: skip (advance PC + ITSTATE), do not execute.
            self.regs[PC] = pc.wrapping_add(len);
            self.it_advance();
            return CpuState::Normal;
        }

        // ---- P1: dispatch to executors ----
        let handled = if thumb {
            if len == 2 {
                self.exec_thumb16(insn as u16, pc, mem)
            } else {
                self.exec_thumb32((insn >> 16) as u16, insn as u16, pc, mem)
            }
        } else {
            self.exec_arm(insn, pc, mem)
        };
        if let Some(st) = handled {
            // Advance ITSTATE after a normally-executed in-IT-block instruction.
            if in_it && matches!(st, CpuState::Normal) {
                self.it_advance();
            }
            return st;
        }

        // ---- everything else: not yet implemented ----
        // Advance past this instruction first; for UDF/breakpoint,
        // environment::debug_cpu_error rewinds PC by 2/4 depending on Thumb.
        self.regs[PC] = pc.wrapping_add(len);
        echo!(
            "[INTERP-UNIMPL] pc={:#010x} thumb={} len={} insn={:#010x}",
            pc,
            thumb as u8,
            len,
            insn
        );
        self.dump_trace();
        CpuState::Error(CpuError::UndefinedInstruction)
    }
}
