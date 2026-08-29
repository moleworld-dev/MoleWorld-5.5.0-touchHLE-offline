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

        // [P1 debug] log first few instructions, and any jump into the stack
        // region (control-flow bug) together with the PREVIOUS instruction (the
        // culprit that wrote the bad PC).
        {
            self.dbg_n = self.dbg_n.wrapping_add(1);
            let _n = self.dbg_n;
            // [P1 debug] heartbeat: every ~4M instructions, print where the CPU
            // is. A steady stream with the PC cycling in a small range means the
            // guest is alive but spinning (a hang); the dumped trace then shows
            // the loop body.
            if _n & 0x003f_ffff == 0 {
                echo!(
                    "[HEARTBEAT] n={:#x} pc={:#010x} r4={:#x} itstate={:#04x} inIT={} z={}",
                    _n, pc, self.regs[4], self.itstate(), self.in_it_block(), self.flag_z()
                );
            }
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
