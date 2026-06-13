/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Differential test harness (P0.5).
//!
//! Runs a single instruction on BOTH the dynarmic JIT (the oracle) and the
//! pure-Rust interpreter from identical state, then compares the resulting
//! registers / CPSR (and, for stores, a window of memory). This is the only
//! reliable way to catch silent flag-bit bugs (NZCV / carry-out / shifter
//! carry), which the `[INTERP-UNIMPL]` log can't see.
//!
//! Only compiled when BOTH backends are enabled:
//!   cargo test --no-default-features \
//!       --features static,cpu_dynarmic,cpu_interpreter cpu::interpreter::diff
#![cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]

use super::InterpreterCpu;
use crate::mem::{Mem, MutPtr, Ptr};
use touchHLE_dynarmic_wrapper::*;

/// Where the instruction under test is written. Far from the (absent) null page.
const CODE_BASE: u32 = 0x0001_0000;
/// A scratch stack region for PUSH/POP/LDR/STR tests.
pub const STACK_TOP: u32 = 0x0010_0000;
const CPSR_USER_MODE: u32 = 0x0000_0010;
const CPSR_THUMB: u32 = 0x0000_0020;
/// Only these CPSR bits are compared (NZCVQ + Thumb + GE + mode). dynarmic and
/// the interpreter may differ in reserved/unused bits we don't model.
const CPSR_MASK: u32 = 0xF80F_0040 | CPSR_THUMB | 0x1F;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct St {
    pub regs: [u32; 16],
    pub cpsr: u32,
}

impl std::fmt::Debug for St {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cpsr={:#010x}[{}] regs={:08x?}", self.cpsr, nzcv(self.cpsr), self.regs)
    }
}

fn nzcv(c: u32) -> String {
    format!(
        "N{}Z{}C{}V{}T{}",
        (c >> 31) & 1,
        (c >> 30) & 1,
        (c >> 29) & 1,
        (c >> 28) & 1,
        (c >> 5) & 1
    )
}

fn write_insn(mem: &mut Mem, insn: u32, len: u32, thumb: bool) {
    let ptr: MutPtr<u8> = Ptr::from_bits(CODE_BASE);
    let bytes = mem.bytes_at_mut(ptr, len);
    if len == 2 {
        bytes.copy_from_slice(&(insn as u16).to_le_bytes());
    } else if thumb {
        // A 32-bit Thumb-2 instruction is two halfwords: bits[31:16] is the
        // FIRST halfword (lower address), bits[15:0] the second. Writing the
        // u32 as one little-endian word would swap them — the fetch would then
        // read bits[15:0] as hw0 and (mis)decode a 16-bit instruction. Lay the
        // halfwords down in instruction order, each little-endian.
        bytes[0..2].copy_from_slice(&((insn >> 16) as u16).to_le_bytes());
        bytes[2..4].copy_from_slice(&(insn as u16).to_le_bytes());
    } else {
        bytes.copy_from_slice(&insn.to_le_bytes());
    }
}

/// Pre-populate words into guest memory (for LOAD tests). Each = (addr, word).
fn write_words(mem: &mut Mem, init: &[(u32, u32)]) {
    for &(addr, word) in init {
        let p: MutPtr<u8> = Ptr::from_bits(addr);
        mem.bytes_at_mut(p, 4).copy_from_slice(&word.to_le_bytes());
    }
}

/// Run one instruction on the dynarmic oracle. Returns final state + a snapshot
/// of `mem_window` bytes at `STACK_TOP - 64` (to catch stores).
fn run_dynarmic(
    insn: u32,
    len: u32,
    cpsr0: u32,
    init_regs: &[u32; 16],
    init_mem: &[(u32, u32)],
) -> (St, Vec<u8>) {
    let mut mem = Mem::new();
    write_insn(&mut mem, insn, len, cpsr0 & CPSR_THUMB != 0);
    write_words(&mut mem, init_mem);
    let dma = unsafe { mem.direct_memory_access_ptr() };
    unsafe {
        let w = touchHLE_DynarmicWrapper_new(dma, 0);
        let r = touchHLE_DynarmicWrapper_regs_mut(w) as *mut [u32; 16];
        *r = *init_regs;
        (*r)[15] = CODE_BASE;
        touchHLE_DynarmicWrapper_set_cpsr(w, cpsr0);
        let _ = touchHLE_DynarmicWrapper_run_or_step(
            w,
            &mut mem as *mut Mem as *mut touchHLE_Mem,
            None,
        );
        let rr = &*(touchHLE_DynarmicWrapper_regs_const(w) as *const [u32; 16]);
        let st = St {
            regs: *rr,
            cpsr: touchHLE_DynarmicWrapper_cpsr(w),
        };
        touchHLE_DynarmicWrapper_delete(w);
        let win = mem_window(&mem);
        (st, win)
    }
}

fn run_interp(
    insn: u32,
    len: u32,
    cpsr0: u32,
    init_regs: &[u32; 16],
    init_mem: &[(u32, u32)],
) -> (St, Vec<u8>) {
    let mut mem = Mem::new();
    write_insn(&mut mem, insn, len, cpsr0 & CPSR_THUMB != 0);
    write_words(&mut mem, init_mem);
    let mut cpu = InterpreterCpu::new(0);
    *cpu.regs_mut() = *init_regs;
    cpu.regs_mut()[15] = CODE_BASE;
    cpu.set_cpsr(cpsr0);
    let _ = cpu.run_or_step(&mut mem, None);
    let st = St {
        regs: *cpu.regs(),
        cpsr: cpu.cpsr(),
    };
    (st, mem_window(&mem))
}

fn mem_window(mem: &Mem) -> Vec<u8> {
    mem.get_bytes_fallible(Ptr::from_bits(STACK_TOP - 64), 64)
        .map(|s| s.to_vec())
        .unwrap_or_default()
}

/// Assert both backends agree for `insn` from the given initial state.
/// Panics with a register/flag diff on mismatch.
#[track_caller]
pub fn check(name: &str, insn: u32, len: u32, thumb: bool, regs: [u32; 16], cpsr: u32) {
    check_mem(name, insn, len, thumb, regs, cpsr, &[]);
}

/// Like [check] but pre-populates guest memory with `init_mem` words (for loads).
#[track_caller]
pub fn check_mem(
    name: &str,
    insn: u32,
    len: u32,
    thumb: bool,
    regs: [u32; 16],
    cpsr: u32,
    init_mem: &[(u32, u32)],
) {
    let cpsr0 = (cpsr & !CPSR_THUMB) | CPSR_USER_MODE | if thumb { CPSR_THUMB } else { 0 };
    let (d, dmem) = run_dynarmic(insn, len, cpsr0, &regs, init_mem);
    let (i, imem) = run_interp(insn, len, cpsr0, &regs, init_mem);

    let mut diffs = Vec::new();
    for n in 0..16 {
        if d.regs[n] != i.regs[n] {
            let nm = match n {
                13 => "sp".to_string(),
                14 => "lr".to_string(),
                15 => "pc".to_string(),
                _ => format!("r{n}"),
            };
            diffs.push(format!(
                "{nm}: oracle={:#010x} interp={:#010x}",
                d.regs[n], i.regs[n]
            ));
        }
    }
    if (d.cpsr & CPSR_MASK) != (i.cpsr & CPSR_MASK) {
        diffs.push(format!(
            "cpsr: oracle={:#010x}[{}] interp={:#010x}[{}]",
            d.cpsr,
            nzcv(d.cpsr),
            i.cpsr,
            nzcv(i.cpsr)
        ));
    }
    if dmem != imem {
        diffs.push(format!("mem[stack-64..]: oracle={dmem:02x?} interp={imem:02x?}"));
    }
    assert!(
        diffs.is_empty(),
        "[DIFF] {name} insn={insn:#010x} MISMATCH:\n  {}\n  ---\n  oracle: {d:?}\n  interp: {i:?}",
        diffs.join("\n  ")
    );
}

/// Differential check for VFP/NEON: also seeds and compares the extension
/// register file (extregs) + FPSCR NZCV, which [check] ignores. Runs in ARM
/// state (the test encodings are ARM cond=AL). Uses dynarmic's swap_context to
/// load/read the full {regs, extregs, cpsr, fpscr} state.
#[track_caller]
pub fn check_vfp(
    name: &str,
    insn: u32,
    regs: [u32; 16],
    ext: [u32; 64],
    init_mem: &[(u32, u32)],
) {
    let cpsr0 = CPSR_USER_MODE; // ARM mode (no Thumb bit)

    // dynarmic oracle.
    let mut mem = Mem::new();
    write_insn(&mut mem, insn, 4, false);
    write_words(&mut mem, init_mem);
    let dma = unsafe { mem.direct_memory_access_ptr() };
    let (dregs, dext, dcpsr, dfpscr) = unsafe {
        let w = touchHLE_DynarmicWrapper_new(dma, 0);
        let mut r = regs;
        r[15] = CODE_BASE;
        let mut ctx = touchHLE_DynarmicContext {
            regs: r,
            extregs: ext,
            cpsr: cpsr0,
            fpscr: 0,
        };
        touchHLE_DynarmicWrapper_swap_context(w, &mut ctx);
        let _ = touchHLE_DynarmicWrapper_run_or_step(
            w,
            &mut mem as *mut Mem as *mut touchHLE_Mem,
            None,
        );
        touchHLE_DynarmicWrapper_swap_context(w, &mut ctx);
        touchHLE_DynarmicWrapper_delete(w);
        (ctx.regs, ctx.extregs, ctx.cpsr, ctx.fpscr)
    };

    // interpreter.
    let mut mem2 = Mem::new();
    write_insn(&mut mem2, insn, 4, false);
    write_words(&mut mem2, init_mem);
    let mut cpu = InterpreterCpu::new(0);
    *cpu.regs_mut() = regs;
    cpu.regs_mut()[15] = CODE_BASE;
    *cpu.extregs_mut() = ext;
    cpu.set_cpsr(cpsr0);
    let _ = cpu.run_or_step(&mut mem2, None);
    let (iregs, iext, icpsr, ifpscr) =
        (*cpu.regs(), *cpu.extregs(), cpu.cpsr(), cpu.fpscr());

    let mut diffs = Vec::new();
    for n in 0..16 {
        if dregs[n] != iregs[n] {
            diffs.push(format!("r{n}: oracle={:#010x} interp={:#010x}", dregs[n], iregs[n]));
        }
    }
    for n in 0..64 {
        if dext[n] != iext[n] {
            diffs.push(format!("s{n}: oracle={:#010x} interp={:#010x}", dext[n], iext[n]));
        }
    }
    if (dcpsr & CPSR_MASK) != (icpsr & CPSR_MASK) {
        diffs.push(format!("cpsr: oracle={dcpsr:#010x} interp={icpsr:#010x}"));
    }
    // Only the NZCV flags of FPSCR matter to us (VCMP→VMRS); dynarmic also sets
    // cumulative exception bits (IOC/…) we don't model, so mask to [31:28].
    if (dfpscr & 0xf000_0000) != (ifpscr & 0xf000_0000) {
        diffs.push(format!(
            "fpscr.nzcv: oracle={:#x} interp={:#x}",
            dfpscr >> 28,
            ifpscr >> 28
        ));
    }
    assert!(
        diffs.is_empty(),
        "[VFP-DIFF] {name} insn={insn:#010x} MISMATCH:\n  {}",
        diffs.join("\n  ")
    );
}

// ---- Tests: one per implemented instruction. Add as P1 implements them. ----
#[cfg(test)]
mod tests {
    use super::*;

    fn r(setup: &[(usize, u32)]) -> [u32; 16] {
        let mut regs = [0u32; 16];
        regs[13] = STACK_TOP; // sane SP
        for &(i, v) in setup {
            regs[i] = v;
        }
        regs
    }

    #[test]
    fn thumb_movs_imm() {
        // MOVS r0, #1  (T1: 0x2001) — sets N,Z; leaves C,V
        check("MOVS r0,#1", 0x2001, 2, true, r(&[]), 0);
        // MOVS r3, #0  (0x2300) — Z=1
        check("MOVS r3,#0", 0x2300, 2, true, r(&[]), 0);
        // MOVS r5, #0xff (0x25ff)
        check("MOVS r5,#255", 0x25ff, 2, true, r(&[]), 0xF000_0000);
    }

    #[test]
    fn thumb_adds_reg() {
        // ADDS r0, r1, r2 (T1: 0x1888) — NZCV via AddWithCarry
        check("ADDS 1+2", 0x1888, 2, true, r(&[(1, 1), (2, 2)]), 0);
        check("ADDS ovf", 0x1888, 2, true, r(&[(1, 0x7fff_ffff), (2, 1)]), 0);
        check("ADDS carry", 0x1888, 2, true, r(&[(1, 0xffff_ffff), (2, 1)]), 0);
        check("ADDS zero", 0x1888, 2, true, r(&[(1, 0), (2, 0)]), 0);
    }

    #[test]
    fn thumb_subs_reg() {
        // SUBS r0, r1, r2 (T1: 0x1a88)
        check("SUBS 5-3", 0x1a88, 2, true, r(&[(1, 5), (2, 3)]), 0);
        check("SUBS 3-5", 0x1a88, 2, true, r(&[(1, 3), (2, 5)]), 0);
        check("SUBS eq", 0x1a88, 2, true, r(&[(1, 7), (2, 7)]), 0);
        check("SUBS ovf", 0x1a88, 2, true, r(&[(1, 0x8000_0000), (2, 1)]), 0);
    }

    #[test]
    fn thumb_cmp_reg() {
        // CMP r1, r2 (T1: 0x4291)
        check("CMP 5,3", 0x4291, 2, true, r(&[(1, 5), (2, 3)]), 0);
        check("CMP 3,5", 0x4291, 2, true, r(&[(1, 3), (2, 5)]), 0);
        check("CMP eq", 0x4291, 2, true, r(&[(1, 9), (2, 9)]), 0);
    }

    // ---- Load/store exclusive + table branch (P1 Group: synchronization) ----
    // LDREX is a plain load + monitor-set; the load result is observable, the
    // monitor is not, so a single-step diff validates the decode/value path.
    // (STREX needs a preceding LDREX in the same run, which the single-step
    // harness can't express, so its success path is covered on-device instead.)

    #[test]
    fn thumb32_ldrex_word() {
        // LDREX r0, [r1, #0]  (0xe851_0f00); r1 -> word 0xDEADBEEF
        let a = STACK_TOP - 32;
        check_mem("LDREX", 0xe851_0f00, 4, true, r(&[(1, a)]), 0, &[(a, 0xDEAD_BEEF)]);
        // With a non-zero imm8 (#4 -> imm8=1): LDREX r0, [r1, #4]  (0xe851_0f01)
        check_mem(
            "LDREX+4",
            0xe851_0f01,
            4,
            true,
            r(&[(1, a)]),
            0,
            &[(a + 4, 0x1234_5678)],
        );
    }

    #[test]
    fn thumb32_ldrex_byte_half() {
        let a = STACK_TOP - 32;
        // LDREXB r0, [r1]  (0xe8d1_0f4f) -> 0xEF
        check_mem("LDREXB", 0xe8d1_0f4f, 4, true, r(&[(1, a)]), 0, &[(a, 0xDEAD_BEEF)]);
        // LDREXH r0, [r1]  (0xe8d1_0f5f) -> 0xBEEF
        check_mem("LDREXH", 0xe8d1_0f5f, 4, true, r(&[(1, a)]), 0, &[(a, 0xDEAD_BEEF)]);
    }

    // TBB/TBH can't go through the differential harness: dynarmic refuses to
    // translate a table branch from a freshly-constructed core (its location
    // descriptor reports an in-IT-block state and it raises Unpredictable
    // Instruction), regardless of Rn. That's a harness artifact — on real
    // hardware/desktop dynarmic the location's IT state is tracked correctly
    // across the instruction stream, and on iOS the interpreter runs anyway.
    // So validate the interpreter's table-read + branch-target math directly.
    #[test]
    fn thumb32_table_branch() {
        let cpsr0 = CPSR_USER_MODE | CPSR_THUMB;
        let tbl = STACK_TOP - 32;
        // TBB [pc, r2]: table byte inline at CODE_BASE+4 == 0x04 (the form the
        // game's switch statements emit). target = PC(=CODE_BASE+4) + 2*4.
        let (st, _) = run_interp(0xe8df_f002, 4, cpsr0, &r(&[(2, 0)]), &[(CODE_BASE + 4, 0x04)]);
        assert_eq!(st.regs[15], CODE_BASE + 4 + 8, "TBB[pc] target");
        assert_eq!(st.cpsr & CPSR_THUMB, CPSR_THUMB, "TBB stays Thumb");
        // TBH [pc, r2, lsl #1]: table half inline at CODE_BASE+4 == 4.
        let (st, _) = run_interp(0xe8df_f012, 4, cpsr0, &r(&[(2, 0)]), &[(CODE_BASE + 4, 0x04)]);
        assert_eq!(st.regs[15], CODE_BASE + 4 + 8, "TBH[pc] target");
        // Register-base form TBB [r1, r2]: table byte at r1 == 0x07, index r2=1
        // -> byte read from tbl+1. Put 0x07 in the second byte: word 0x0700.
        let (st, _) = run_interp(
            0xe8d1_f002,
            4,
            cpsr0,
            &r(&[(1, tbl), (2, 1)]),
            &[(tbl, 0x0000_0700)],
        );
        assert_eq!(st.regs[15], CODE_BASE + 4 + 14, "TBB[r1,r2] target (byte=7)");
    }

    // ---- VFP (scalar floating point) — checked against dynarmic incl. extregs ----
    /// Build an initial extension-register file (s-register words).
    fn e(setup: &[(usize, u32)]) -> [u32; 64] {
        let mut x = [0u32; 64];
        for &(i, v) in setup {
            x[i] = v;
        }
        x
    }
    const F2: u32 = 0x4000_0000; // 2.0f32
    const F3: u32 = 0x4040_0000; // 3.0f32

    #[test]
    fn vfp_arith_f32() {
        // s1=2.0, s2=3.0
        let ext = e(&[(1, F2), (2, F3)]);
        check_vfp("VADD.F32 s0,s1,s2", 0xEE300A81, r(&[]), ext, &[]); // 5.0
        check_vfp("VSUB.F32 s0,s1,s2", 0xEE300AC1, r(&[]), ext, &[]); // -1.0
        check_vfp("VMUL.F32 s0,s1,s2", 0xEE200A81, r(&[]), ext, &[]); // 6.0
        check_vfp("VDIV.F32 s0,s1,s2", 0xEE800A81, r(&[]), ext, &[]); // 0.666…
    }

    #[test]
    fn vfp_arith_f64() {
        // d1=2.0, d2=3.0 (high words at odd s-indices)
        let ext = e(&[(3, 0x4000_0000), (5, 0x4008_0000)]);
        check_vfp("VADD.F64 d0,d1,d2", 0xEE310B02, r(&[]), ext, &[]); // 5.0
    }

    #[test]
    fn vfp_unary_f32() {
        let ext = e(&[(1, 0xC040_0000)]); // s1 = -3.0
        check_vfp("VABS.F32 s0,s1", 0xEEB00AE0, r(&[]), ext, &[]); // 3.0
        check_vfp("VNEG.F32 s0,s1", 0xEEB10A60, r(&[]), ext, &[]); // 3.0
        let ext2 = e(&[(1, 0x4110_0000)]); // s1 = 9.0
        check_vfp("VSQRT.F32 s0,s1", 0xEEB10AE0, r(&[]), ext2, &[]); // 3.0
    }

    #[test]
    fn vfp_vmov_imm_and_core() {
        check_vfp("VMOV.F32 s0,#1.0", 0xEEB70A00, r(&[]), e(&[]), &[]); // 1.0
        check_vfp("VMOV s0,r0", 0xEE000A10, r(&[(0, 0x4049_0FDB)]), e(&[]), &[]);
        check_vfp("VMOV r0,s0", 0xEE100A10, r(&[]), e(&[(0, 0x1234_5678)]), &[]);
    }

    #[test]
    fn vfp_cvt() {
        check_vfp("VCVT.S32.F32 s0,s1", 0xEEBD0AE0, r(&[]), e(&[(1, 0x4076_6666)]), &[]); // 3.85→3
        check_vfp("VCVT.F32.S32 s0,s1", 0xEEB80AE0, r(&[]), e(&[(1, 5)]), &[]); // 5→5.0
        check_vfp("VCVT.F64.F32 d0,s1", 0xEEB70AE0, r(&[]), e(&[(1, F2)]), &[]); // 2.0
    }

    #[test]
    fn vfp_high_registers() {
        // d16-d31 (D bit = 1) must decode to the SAME opcode as low regs — the
        // D bit is in bits[23:20] and must be masked out of opc1.
        // VCVT.F64.F32 d16, s0:  s0 (ext[0]) = 2.0f -> d16 (ext[32..34]) = 2.0
        check_vfp("VCVT.F64.F32 d16,s0", 0xeef70ac0, r(&[]), e(&[(0, F2)]), &[]);
        // VADD.F64 d16, d17, d18:  d17=2.0 (ext[35]), d18=3.0 (ext[37]) -> d16=5.0
        check_vfp(
            "VADD.F64 d16,d17,d18",
            0xee710ba2,
            r(&[]),
            e(&[(35, 0x4000_0000), (37, 0x4008_0000)]),
            &[],
        );
    }

    #[test]
    fn thumb32_reg_shift() {
        // LSL/LSR/ASR/ROR (register) .W — r5 = r4 shifted by r8[7:0].
        let rr = r(&[(4, 0x8000_00F0), (8, 4)]);
        check("LSL.W", 0xfa04_f508, 4, true, rr, 0);
        check("LSLS.W", 0xfa14_f508, 4, true, rr, 0); // sets flags
        check("LSR.W", 0xfa24_f508, 4, true, rr, 0);
        check("ASR.W", 0xfa44_f508, 4, true, rr, 0);
        check("ROR.W", 0xfa64_f508, 4, true, rr, 0);
        // shift amount 0 (r8=0): value unchanged, carry unchanged
        check("LSLS.W #0", 0xfa14_f508, 4, true, r(&[(4, 0x1234), (8, 0)]), 0x2000_0000);
        // shift amount >= 32 (r8=40)
        check("LSLS.W 40", 0xfa14_f508, 4, true, r(&[(4, 0xFF), (8, 40)]), 0);
        check("LSRS.W 32", 0xfa34_f508, 4, true, r(&[(4, 0x8000_0000), (8, 32)]), 0);
    }

    #[test]
    fn thumb32_signed_multiply() {
        // Operands chosen to exercise the high-word / halfword paths.
        let rr = r(&[(1, 0x0001_2345), (2, 0x6789_ABCD), (3, 0x0000_0007)]);
        check("SMMLA", 0xfb51_3002, 4, true, rr, 0); // r0 = r3 + hi(r1*r2)
        check("SMMUL", 0xfb51_f002, 4, true, rr, 0); // r0 = hi(r1*r2)
        check("SMMLS", 0xfb61_3002, 4, true, rr, 0); // r0 = (r3<<32 - r1*r2)>>32
        check("SMULBB", 0xfb11_f002, 4, true, rr, 0); // r0 = r1.b * r2.b
        check("SMLABB", 0xfb11_3002, 4, true, rr, 0); // r0 = r3 + r1.b*r2.b
        check("SMULWB", 0xfb31_f002, 4, true, rr, 0); // r0 = (r1 * r2.b) >> 16
        check("SMUAD", 0xfb21_f002, 4, true, rr, 0); // dual 16×16
        // negative operands
        let rn = r(&[(1, 0xFFFF_FFFF), (2, 0x8000_0001), (3, 0xFFFF_0000)]);
        check("SMMLA neg", 0xfb51_3002, 4, true, rn, 0);
        check("SMULBB neg", 0xfb11_f002, 4, true, rn, 0);
    }

    #[test]
    fn neon_fp_arith() {
        // d1 lanes = [2.0, 3.0], d2 lanes = [4.0, 5.0]  (ext[2..4], ext[4..6])
        let ext = e(&[(2, F2), (3, F3), (4, 0x4080_0000), (5, 0x40A0_0000)]);
        check_vfp("VADD.F32 d0,d1,d2", 0xf2010d02, r(&[]), ext, &[]); // [6,8]
        check_vfp("VSUB.F32 d0,d1,d2", 0xf2210d02, r(&[]), ext, &[]); // [-2,-2]
        check_vfp("VMUL.F32 d0,d1,d2", 0xf3010d12, r(&[]), ext, &[]); // [8,15]
        check_vfp("VMAX.F32 d0,d1,d2", 0xf2010f02, r(&[]), ext, &[]); // [4,5]
        check_vfp("VMIN.F32 d0,d1,d2", 0xf2210f02, r(&[]), ext, &[]); // [2,3]
        // quad: q1 lanes ext[4..8], q2 lanes ext[8..12]
        let extq = e(&[
            (4, F2), (5, F3), (6, F2), (7, F3),
            (8, 0x4080_0000), (9, 0x4080_0000), (10, 0x4080_0000), (11, 0x4080_0000),
        ]);
        check_vfp("VADD.F32 q0,q1,q2", 0xf2020d44, r(&[]), extq, &[]);
    }

    #[test]
    fn neon_fp_unary_cvt() {
        let neg = e(&[(2, 0xC000_0000), (3, 0xC040_0000)]); // d1 = [-2.0, -3.0]
        check_vfp("VABS.F32 d0,d1", 0xf3b90701, r(&[]), neg, &[]); // [2,3]
        check_vfp("VNEG.F32 d0,d1", 0xf3b90781, r(&[]), neg, &[]); // [2,3]
        let f = e(&[(2, 0x4076_6666), (3, 0xBFC0_0000)]); // d1 = [3.85, -1.5]
        check_vfp("VCVT.S32.F32 d0,d1", 0xf3bb0701, r(&[]), f, &[]); // [3,-1]
        check_vfp("VCVT.U32.F32 d0,d1", 0xf3bb0781, r(&[]), e(&[(2, 0x4076_6666)]), &[]);
        let ints = e(&[(2, 5), (3, 0xFFFF_FFFE)]); // d1 = [5, -2] as s32
        check_vfp("VCVT.F32.S32 d0,d1", 0xf3bb0601, r(&[]), ints, &[]); // [5.0,-2.0]
    }

    #[test]
    fn vfp_vcmp_flags() {
        // s0 vs s1: VCMP sets FPSCR.NZCV; VMRS would copy to CPSR (tested via the
        // fpscr.nzcv comparison in check_vfp).
        check_vfp("VCMP 2<3", 0xEEB40A60, r(&[]), e(&[(0, F2), (1, F3)]), &[]); // N=1
        check_vfp("VCMP 3>2", 0xEEB40A60, r(&[]), e(&[(0, F3), (1, F2)]), &[]); // C=1
        check_vfp("VCMP 2==2", 0xEEB40A60, r(&[]), e(&[(0, F2), (1, F2)]), &[]); // Z=1,C=1
    }
}
