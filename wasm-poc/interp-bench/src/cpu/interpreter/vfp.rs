/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! VFP (floating-point, cp10/cp11) + the small slice of Advanced SIMD (NEON)
//! the game actually uses, for the pure-Rust interpreter (iOS backend).
//!
//! Reached from BOTH decoders: the ARM decoder (cond 11xx after the cond strip)
//! and the Thumb-2 decoder (op1∈{01,11} with insn[26]==1). A VFP/SIMD
//! instruction's low 28 bits are encoded identically in ARM and Thumb-2; the
//! top nibble (ARM condition / Thumb 1110|1111 prefix) is consumed by the
//! caller, so this module decodes only bits[27:0]. All these instructions are
//! 4 bytes in both states, so PC advances by 4.
//!
//! Scan of the game binary's __text: ~91.7k VFP (scalar f32/f64) vs only ~2.9k
//! NEON-quad, almost all of which are `VMOV.I32 qN,#0`. So the bulk here is
//! scalar VFP: VLDR/VSTR/VLDM/VSTM, VMOV (all forms), VADD/VSUB/VMUL/VDIV/MAC,
//! VABS/VNEG/VSQRT, VCMP(E), VCVT, VMRS/VMSR; plus NEON VMOV.I / VORR / VAND /
//! VLD1 / VST1. Anything not yet implemented returns None → `[INTERP-UNIMPL]`.

use super::InterpreterCpu;
use crate::cpu::{CpuError, CpuState};
use crate::mem::Mem;

/// VFPExpandImm (ARM ARM A7.5.1): expand an 8-bit immediate to f32 / f64 bits.
/// f32 = sign : NOT(b6) : b6×5 : imm8<5:0> : Zeros(19).
fn vfp_expand_imm32(imm8: u32) -> u32 {
    let sign = (imm8 >> 7) & 1;
    let b6 = (imm8 >> 6) & 1;
    let mut bits = sign << 31;
    bits |= (1 - b6) << 30;
    let rep = if b6 == 1 { 0b11111 } else { 0 };
    bits |= rep << 25;
    bits |= (imm8 & 0x3f) << 19;
    bits
}
fn vfp_expand_imm64(imm8: u32) -> u64 {
    let sign = ((imm8 >> 7) & 1) as u64;
    let b6 = ((imm8 >> 6) & 1) as u64;
    let mut bits = sign << 63;
    bits |= (1 - b6) << 62;
    let rep = if b6 == 1 { 0xff } else { 0 };
    bits |= rep << 54;
    bits |= ((imm8 & 0x3f) as u64) << 48;
    bits
}

impl InterpreterCpu {
    /// VFP / Advanced-SIMD instruction space. Decodes bits[27:0]. Returns None
    /// for non-FP coprocessors and sub-encodings not yet implemented.
    pub(super) fn exec_vfp(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> Option<CpuState> {
        let pfx = insn >> 24; // top byte: ARM cond<<4|… or Thumb 0xE_/0xF_ prefix

        // ---- Advanced SIMD (NEON) ----
        // Data-processing: Thumb 0xef (U=0) / 0xff (U=1); ARM 0xf2 / 0xf3.
        if matches!(pfx, 0xef | 0xff | 0xf2 | 0xf3) {
            return self.neon_data_proc(insn, pc);
        }
        // Element/structure load/store: Thumb 0xf9; ARM 0xf4.
        if matches!(pfx, 0xf9 | 0xf4) {
            return self.neon_ldst(insn, pc, mem);
        }

        // ---- VFP (cp10 single / cp11 double) ----
        let coproc = (insn >> 8) & 0xf;
        if coproc != 0b1010 && coproc != 0b1011 {
            return None;
        }
        // bits[27:25]==110 → extension-register load/store (+ 64-bit transfers).
        if (insn >> 25) & 0b111 == 0b110 {
            return self.vfp_ext_ldst(insn, pc, mem);
        }
        // bits[27:24]==1110 → data-processing (bit4==0) / register transfer (1).
        if (insn >> 24) & 0xf == 0b1110 {
            if (insn >> 4) & 1 == 0 {
                return self.vfp_data_proc(insn, pc);
            }
            return self.vfp_xfer(insn, pc);
        }
        None
    }

    // ============================ ext load/store ============================

    /// VLDM, VSTM, VLDR, VSTR, VPUSH, VPOP (+ the P=U=W=0 64-bit VMOV transfer).
    fn vfp_ext_ldst(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let p = (insn >> 24) & 1;
        let u = (insn >> 23) & 1;
        let d_bit = (insn >> 22) & 1;
        let w = (insn >> 21) & 1;
        let l = (insn >> 20) & 1;
        let rn = ((insn >> 16) & 0xf) as usize;
        let vd = (insn >> 12) & 0xf;
        let single = (insn >> 8) & 1 == 0; // cp10 single, cp11 double
        let imm8 = insn & 0xff;

        // P=0,U=0,W=0 → 64-bit core↔doubleword transfer (VMOV), not load/store.
        if p == 0 && u == 0 && w == 0 {
            return self.vfp_64_xfer(insn, pc);
        }

        let dreg = if single {
            ((vd << 1) | d_bit) as usize
        } else {
            ((d_bit << 4) | vd) as usize
        };
        let base = if rn == 15 {
            self.get_reg(15) & !3
        } else {
            self.get_reg(rn)
        };

        // VLDR / VSTR (single register, P==1, W==0).
        if p == 1 && w == 0 {
            let imm32 = imm8 << 2;
            let addr = if u == 1 {
                base.wrapping_add(imm32)
            } else {
                base.wrapping_sub(imm32)
            };
            if single {
                if l == 1 {
                    let Some(v) = self.data_r_u32(mem, addr) else {
                        self.regs[15] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    self.set_sreg(dreg, v);
                } else if !self.data_w_u32(mem, addr, self.get_sreg(dreg)) {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
            } else if l == 1 {
                let (Some(lo), Some(hi)) =
                    (self.data_r_u32(mem, addr), self.data_r_u32(mem, addr.wrapping_add(4)))
                else {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                };
                self.set_dreg(dreg, (lo as u64) | ((hi as u64) << 32));
            } else {
                let v = self.get_dreg(dreg);
                if !self.data_w_u32(mem, addr, v as u32)
                    || !self.data_w_u32(mem, addr.wrapping_add(4), (v >> 32) as u32)
                {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
            }
            self.regs[15] = next;
            return Some(CpuState::Normal);
        }

        // VLDM / VSTM (multiple). imm8 counts words; a double is two words.
        let count = if single { imm8 } else { imm8 / 2 };
        let bytes = imm8 << 2;
        let start = if u == 1 { base } else { base.wrapping_sub(bytes) };
        let mut addr = start;
        for i in 0..count {
            let r = dreg + i as usize;
            if single {
                if l == 1 {
                    let Some(v) = self.data_r_u32(mem, addr) else {
                        self.regs[15] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    self.set_sreg(r, v);
                } else if !self.data_w_u32(mem, addr, self.get_sreg(r)) {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
                addr = addr.wrapping_add(4);
            } else {
                if l == 1 {
                    let (Some(lo), Some(hi)) =
                        (self.data_r_u32(mem, addr), self.data_r_u32(mem, addr.wrapping_add(4)))
                    else {
                        self.regs[15] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    };
                    self.set_dreg(r, (lo as u64) | ((hi as u64) << 32));
                } else {
                    let v = self.get_dreg(r);
                    if !self.data_w_u32(mem, addr, v as u32)
                        || !self.data_w_u32(mem, addr.wrapping_add(4), (v >> 32) as u32)
                    {
                        self.regs[15] = next;
                        return Some(CpuState::Error(CpuError::MemoryError));
                    }
                }
                addr = addr.wrapping_add(8);
            }
        }
        if w == 1 {
            self.regs[rn] = if u == 1 {
                base.wrapping_add(bytes)
            } else {
                base.wrapping_sub(bytes)
            };
        }
        self.regs[15] = next;
        Some(CpuState::Normal)
    }

    // ============================ data-processing ============================

    /// VFP data-processing: VADD/VSUB/VMUL/VNMUL/VDIV, VMLA/VMLS/VNMLA/VNMLS,
    /// VMOV(imm), and the 2-register "other" group (VMOV-reg/VABS/VNEG/VSQRT/
    /// VCMP(E)/VCVT). bits[27:24]==1110, bit4==0.
    fn vfp_data_proc(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let dp = (insn >> 8) & 1 == 1; // sz: 0=f32, 1=f64
        // opc1 = bits[23:20] but bit22 is the Vd-high (D) register bit, NOT part
        // of the opcode — mask it out so high registers (d16-d31, D=1) decode to
        // the same opcode as low ones (e.g. VADD.F64 d16 has bits[23:20]=0111,
        // same op as d0's 0011).
        let opc1 = (insn >> 20) & 0b1011;
        let opc3 = (insn >> 6) & 0b11;
        let (vd, vn, vm) = ((insn >> 12) & 0xf, (insn >> 16) & 0xf, insn & 0xf);
        let (dbit, nbit, mbit) = ((insn >> 22) & 1, (insn >> 7) & 1, (insn >> 5) & 1);
        let (rd, rn, rm) = if dp {
            (
                ((dbit << 4) | vd) as usize,
                ((nbit << 4) | vn) as usize,
                ((mbit << 4) | vm) as usize,
            )
        } else {
            (
                ((vd << 1) | dbit) as usize,
                ((vn << 1) | nbit) as usize,
                ((vm << 1) | mbit) as usize,
            )
        };

        // opc1==1011 with opc3 odd → the 2-register "other" group.
        if opc1 == 0b1011 && (opc3 & 1) == 1 {
            return self.vfp_other(insn, pc, dp, rd, rm);
        }

        let op = opc3 & 1;
        match opc1 {
            0b0011 => {
                // VADD (op=0) / VSUB (op=1)
                if dp {
                    let (a, b) = (self.get_d_f64(rn), self.get_d_f64(rm));
                    self.set_d_f64(rd, if op == 0 { a + b } else { a - b });
                } else {
                    let (a, b) = (self.get_s_f32(rn), self.get_s_f32(rm));
                    self.set_s_f32(rd, if op == 0 { a + b } else { a - b });
                }
            }
            0b0010 => {
                // VMUL (op=0) / VNMUL (op=1)
                if dp {
                    let p = self.get_d_f64(rn) * self.get_d_f64(rm);
                    self.set_d_f64(rd, if op == 0 { p } else { -p });
                } else {
                    let p = self.get_s_f32(rn) * self.get_s_f32(rm);
                    self.set_s_f32(rd, if op == 0 { p } else { -p });
                }
            }
            0b1000 if op == 0 => {
                // VDIV
                if dp {
                    self.set_d_f64(rd, self.get_d_f64(rn) / self.get_d_f64(rm));
                } else {
                    self.set_s_f32(rd, self.get_s_f32(rn) / self.get_s_f32(rm));
                }
            }
            0b0000 => {
                // VMLA (op=0): Vd += Vn*Vm ; VMLS (op=1): Vd -= Vn*Vm
                if dp {
                    let p = self.get_d_f64(rn) * self.get_d_f64(rm);
                    let d = self.get_d_f64(rd);
                    self.set_d_f64(rd, if op == 0 { d + p } else { d - p });
                } else {
                    let p = self.get_s_f32(rn) * self.get_s_f32(rm);
                    let d = self.get_s_f32(rd);
                    self.set_s_f32(rd, if op == 0 { d + p } else { d - p });
                }
            }
            0b0001 => {
                // VNMLS (op=0): Vd = -Vd + Vn*Vm ; VNMLA (op=1): Vd = -Vd - Vn*Vm
                if dp {
                    let p = self.get_d_f64(rn) * self.get_d_f64(rm);
                    let d = self.get_d_f64(rd);
                    self.set_d_f64(rd, if op == 0 { -d + p } else { -d - p });
                } else {
                    let p = self.get_s_f32(rn) * self.get_s_f32(rm);
                    let d = self.get_s_f32(rd);
                    self.set_s_f32(rd, if op == 0 { -d + p } else { -d - p });
                }
            }
            0b1011 => {
                // opc1==1011, opc3 even → VMOV (immediate).
                let imm8 = ((insn >> 16) & 0xf) << 4 | (insn & 0xf);
                if dp {
                    self.set_dreg(rd, vfp_expand_imm64(imm8));
                } else {
                    self.set_sreg(rd, vfp_expand_imm32(imm8));
                }
            }
            _ => return None,
        }
        self.regs[15] = next;
        Some(CpuState::Normal)
    }

    /// 2-register "other" VFP group (opc1==1011, opc3 odd): VMOV-reg, VABS,
    /// VNEG, VSQRT, VCMP(E), VCVT (f32↔f64, int↔fp, fp→int).
    fn vfp_other(&mut self, insn: u32, pc: u32, dp: bool, rd: usize, rm: usize) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let opc2 = (insn >> 16) & 0xf;
        let opc3 = (insn >> 6) & 0b11; // bit7:bit6
        let e = (insn >> 7) & 1; // opc3 high bit
        // VCVT mixes operand sizes (the integer side, and one side of f32↔f64,
        // is always a single register), so the passed-in rd/rm — numbered by the
        // sz bit — are wrong for it. Recompute both single- and double-precision
        // register numbers from the raw Vd/Vm + D/M bits.
        let (vd, vm) = ((insn >> 12) & 0xf, insn & 0xf);
        let (dbit, mbit) = ((insn >> 22) & 1, (insn >> 5) & 1);
        let rd_s = ((vd << 1) | dbit) as usize;
        let rd_d = ((dbit << 4) | vd) as usize;
        let rm_s = ((vm << 1) | mbit) as usize;
        let rm_d = ((mbit << 4) | vm) as usize;

        match opc2 {
            0b0000 => {
                if opc3 == 0b01 {
                    // VMOV (register): Vd = Vm
                    if dp {
                        let v = self.get_dreg(rm);
                        self.set_dreg(rd, v);
                    } else {
                        let v = self.get_sreg(rm);
                        self.set_sreg(rd, v);
                    }
                } else {
                    // VABS
                    if dp {
                        self.set_d_f64(rd, self.get_d_f64(rm).abs());
                    } else {
                        self.set_s_f32(rd, self.get_s_f32(rm).abs());
                    }
                }
            }
            0b0001 => {
                if opc3 == 0b01 {
                    // VNEG
                    if dp {
                        self.set_d_f64(rd, -self.get_d_f64(rm));
                    } else {
                        self.set_s_f32(rd, -self.get_s_f32(rm));
                    }
                } else {
                    // VSQRT
                    if dp {
                        self.set_d_f64(rd, self.get_d_f64(rm).sqrt());
                    } else {
                        self.set_s_f32(rd, self.get_s_f32(rm).sqrt());
                    }
                }
            }
            0b0100 | 0b0101 => {
                // VCMP / VCMPE: Vd vs (opc2==0101 ? 0.0 : Vm). e selects E variant
                // (signalling) — we don't model FP exceptions so treat alike.
                let _ = e;
                let cmp_zero = opc2 == 0b0101;
                let (n, z, c, v) = if dp {
                    let a = self.get_d_f64(rd);
                    let b = if cmp_zero { 0.0 } else { self.get_d_f64(rm) };
                    fp_cmp_flags(a.partial_cmp(&b))
                } else {
                    let a = self.get_s_f32(rd);
                    let b = if cmp_zero { 0.0 } else { self.get_s_f32(rm) };
                    fp_cmp_flags(a.partial_cmp(&b).map(|o| o))
                };
                self.fpscr = (self.fpscr & 0x0fff_ffff)
                    | (n << 31)
                    | (z << 30)
                    | (c << 29)
                    | (v << 28);
            }
            0b0111 if opc3 == 0b11 => {
                // VCVT between single and double precision. dp = SOURCE size
                // (sz bit): the destination is the other size.
                if dp {
                    // f64 (Dm) -> f32 (Sd)
                    let val = self.get_d_f64(rm_d) as f32;
                    self.set_s_f32(rd_s, val);
                } else {
                    // f32 (Sm) -> f64 (Dd)
                    let val = self.get_s_f32(rm_s) as f64;
                    self.set_d_f64(rd_d, val);
                }
            }
            0b1000 => {
                // VCVT integer -> floating point. Source is always single reg Sm;
                // dest is Sd (sz=0) or Dd (sz=1). bit7 (e): 1 = signed.
                let src = self.get_sreg(rm_s);
                let signed = e == 1;
                if dp {
                    let v = if signed { src as i32 as f64 } else { src as f64 };
                    self.set_d_f64(rd_d, v);
                } else {
                    let v = if signed { src as i32 as f32 } else { src as f32 };
                    self.set_s_f32(rd_s, v);
                }
            }
            0b1100 | 0b1101 => {
                // VCVT floating point -> integer. Source is Sm (sz=0) or Dm
                // (sz=1); destination is always single reg Sd. opc2==1101 →
                // signed, 1100 → unsigned. bit7 (e): 1 = round toward zero
                // (VCVT), 0 = round per FPSCR (VCVTR) — we always trunc.
                let signed = opc2 == 0b1101;
                let f = if dp {
                    self.get_d_f64(rm_d)
                } else {
                    self.get_s_f32(rm_s) as f64
                };
                let out = if signed {
                    fp_to_i32(f) as u32
                } else {
                    fp_to_u32(f)
                };
                self.set_sreg(rd_s, out);
            }
            _ => return None,
        }
        self.regs[15] = next;
        Some(CpuState::Normal)
    }

    // ============================ register transfers ============================

    /// VFP 8/16/32-bit transfer between core and FP registers (bits[27:24]==1110,
    /// bit4==1): VMOV core↔single, VMSR/VMRS (system regs), VMOV.32 core↔scalar.
    fn vfp_xfer(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let l = (insn >> 20) & 1; // 1 = to core (read), 0 = from core (write)
        let rt = ((insn >> 12) & 0xf) as usize;
        let a = (insn >> 21) & 0b111; // opc1

        // VMSR/VMRS: bits[27:21]==1110_111, L selects R/W, special reg in bits[19:16].
        if (insn >> 21) & 0b111 == 0b111 {
            let sysreg = (insn >> 16) & 0xf;
            if sysreg == 0b0001 {
                // FPSCR
                if l == 1 {
                    // VMRS
                    if rt == 15 {
                        // VMRS APSR_nzcv, FPSCR — copy FPSCR[31:28] into CPSR.
                        self.cpsr = (self.cpsr & 0x0fff_ffff) | (self.fpscr & 0xf000_0000);
                    } else {
                        self.regs[rt] = self.fpscr;
                    }
                } else {
                    // VMSR FPSCR, Rt
                    self.fpscr = self.get_reg(rt);
                }
                self.regs[15] = next;
                return Some(CpuState::Normal);
            }
            // FPSID/FPEXC etc.: read returns a benign value, write ignored.
            if l == 1 && rt != 15 {
                self.regs[rt] = 0;
            }
            self.regs[15] = next;
            return Some(CpuState::Normal);
        }

        // VMOV (core ↔ single-precision register): bits[27:21]==1110_000, bit20=op.
        if a == 0b000 && (insn >> 8) & 0xf == 0b1010 && (insn >> 5) & 0b111 == 0 {
            let n = (((insn >> 16) & 0xf) << 1 | ((insn >> 7) & 1)) as usize; // Sn
            if l == 1 {
                self.regs[rt] = self.get_sreg(n); // VMOV Rt, Sn
            } else {
                let v = self.get_reg(rt);
                self.set_sreg(n, v); // VMOV Sn, Rt
            }
            self.regs[15] = next;
            return Some(CpuState::Normal);
        }

        // VMOV.32 core ↔ scalar lane: VMOV Dn[x], Rt / VMOV Rt, Dn[x]. The 32-bit
        // form has bits[22,5]==0 and selects lane = bit21.
        // Encoding: 1110 0 opc1 L Vn Rt 1011 N opc2 1 0000, opc1=0xx with bit23..
        {
            let dn = (((insn >> 7) & 1) << 4 | ((insn >> 16) & 0xf)) as usize; // (N:Vn)
            let lane = ((insn >> 21) & 1) as usize; // 32-bit form lane index
            // 32-bit variant: bits[23]==0, bits[6:5]==00, bits[22]==0.
            if (insn >> 23) & 1 == 0 && (insn >> 5) & 0b11 == 0 && (insn >> 22) & 1 == 0 {
                if l == 1 {
                    // VMOV Rt, Dn[lane]
                    let d = self.get_dreg(dn);
                    self.regs[rt] = if lane == 0 { d as u32 } else { (d >> 32) as u32 };
                } else {
                    // VMOV Dn[lane], Rt
                    let d = self.get_dreg(dn);
                    let v = self.get_reg(rt) as u64;
                    let nd = if lane == 0 {
                        (d & 0xffff_ffff_0000_0000) | v
                    } else {
                        (d & 0x0000_0000_ffff_ffff) | (v << 32)
                    };
                    self.set_dreg(dn, nd);
                }
                self.regs[15] = next;
                return Some(CpuState::Normal);
            }
        }
        None
    }

    /// 64-bit transfer: VMOV between two core registers and a doubleword FP
    /// register (or a pair of singles). bits[27:21]==1100_010.
    fn vfp_64_xfer(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        let op = (insn >> 20) & 1; // 1 = to core
        let rt = ((insn >> 12) & 0xf) as usize;
        let rt2 = ((insn >> 16) & 0xf) as usize;
        let single = (insn >> 8) & 1 == 0; // cp10 → pair of singles; cp11 → one d
        let m = ((insn >> 5) & 1) as usize;
        let vm = (insn & 0xf) as usize;

        if single {
            // VMOV Sm:Sm+1 ↔ Rt:Rt2.  Sm = (Vm<<1)|M.
            let sm = (vm << 1) | m;
            if op == 1 {
                self.regs[rt] = self.get_sreg(sm);
                self.regs[rt2] = self.get_sreg(sm + 1);
            } else {
                let (a, b) = (self.get_reg(rt), self.get_reg(rt2));
                self.set_sreg(sm, a);
                self.set_sreg(sm + 1, b);
            }
        } else {
            // VMOV Dm ↔ Rt:Rt2.  Dm = (M<<4)|Vm. Rt = low word, Rt2 = high word.
            let dm = (m << 4) | vm;
            if op == 1 {
                let d = self.get_dreg(dm);
                self.regs[rt] = d as u32;
                self.regs[rt2] = (d >> 32) as u32;
            } else {
                let (lo, hi) = (self.get_reg(rt), self.get_reg(rt2));
                self.set_dreg(dm, (lo as u64) | ((hi as u64) << 32));
            }
        }
        self.regs[15] = next;
        Some(CpuState::Normal)
    }

    // ============================ NEON (the small slice used) ============================

    /// NEON data-processing — the slice the game emits: vector FP arithmetic
    /// (VADD/VSUB/VMUL/VMAX/VMIN .F32), vector FP convert/abs/neg (VCVT/VABS/
    /// VNEG), VMOV/VMVN immediate (esp. `VMOV.I32 qN,#0`), and VORR/VAND/VMOV
    /// register (d/q bitwise). Lanes are 32-bit; Q=0 → 2 lanes (d), Q=1 → 4 (q).
    fn neon_data_proc(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        // The U bit is at bit24 in ARM (prefix 0xf2/0xf3) but bit28 in Thumb
        // (0xef/0xff); everything below bit24 is identical in both states.
        let pfx0 = insn >> 24;
        let u = if pfx0 == 0xef || pfx0 == 0xff {
            (insn >> 28) & 1
        } else {
            (insn >> 24) & 1
        };
        let q = (insn >> 6) & 1;
        let lanes = if q == 1 { 4 } else { 2 };
        let rd = (((insn >> 22) & 1) << 4 | (insn >> 12) & 0xf) as usize; // (D:Vd)
        let rn = (((insn >> 7) & 1) << 4 | (insn >> 16) & 0xf) as usize; // (N:Vn)
        let rm = (((insn >> 5) & 1) << 4 | insn & 0xf) as usize; // (M:Vm)

        // ---- 3-register same length, F32 (bit23==0, B=1101/1111) ----
        if (insn >> 23) & 1 == 0 {
            let b = (insn >> 8) & 0xf;
            let bit21 = (insn >> 21) & 1;
            let bit4 = (insn >> 4) & 1;
            let fp_op: Option<fn(f32, f32) -> f32> = match (b, bit4, u, bit21) {
                (0b1101, 0, 0, 0) => Some(|a, b| a + b), // VADD.F32
                (0b1101, 0, 0, 1) => Some(|a, b| a - b), // VSUB.F32
                (0b1101, 1, 1, 0) => Some(|a, b| a * b), // VMUL.F32
                (0b1111, 0, 0, 0) => Some(f32::max),     // VMAX.F32
                (0b1111, 0, 0, 1) => Some(f32::min),     // VMIN.F32
                _ => None,
            };
            if let Some(op) = fp_op {
                for k in 0..lanes {
                    let a = f32::from_bits(self.get_sreg(2 * rn + k));
                    let bb = f32::from_bits(self.get_sreg(2 * rm + k));
                    self.set_sreg(2 * rd + k, op(a, bb).to_bits());
                }
                self.regs[15] = next;
                return Some(CpuState::Normal);
            }
        }

        // ---- 2-register miscellaneous, F32: VABS/VNEG/VCVT (bit23==1,
        // bits[21:20]==11, size bits[19:18]==10) ----
        if (insn >> 23) & 1 == 1 && (insn >> 20) & 0b11 == 0b11 && (insn >> 18) & 0b11 == 0b10 {
            let f2 = (insn >> 16) & 0b11; // bits[17:16]
            let b = (insn >> 7) & 0b11111; // bits[11:7]
            // VABS.F32 (bits[17:16]==01, bits[11:7]==01110); VNEG (…01111).
            if f2 == 0b01 && (b >> 1) == 0b0111 {
                let neg = b & 1 == 1;
                for k in 0..lanes {
                    let v = f32::from_bits(self.get_sreg(2 * rm + k));
                    let r = if neg { -v } else { v.abs() };
                    self.set_sreg(2 * rd + k, r.to_bits());
                }
                self.regs[15] = next;
                return Some(CpuState::Normal);
            }
            // VCVT vector (bits[17:16]==11): bits[11:8] 011x, bit8=to-int,
            // bit7=unsigned. Round toward zero for to-int.
            if f2 == 0b11 && (insn >> 9) & 0b111 == 0b011 {
                let to_int = (insn >> 8) & 1 == 1;
                let unsigned = (insn >> 7) & 1 == 1;
                for k in 0..lanes {
                    let raw = self.get_sreg(2 * rm + k);
                    let out = if to_int {
                        let f = f32::from_bits(raw) as f64;
                        if unsigned {
                            fp_to_u32(f)
                        } else {
                            fp_to_i32(f) as u32
                        }
                    } else if unsigned {
                        (raw as f32).to_bits()
                    } else {
                        (raw as i32 as f32).to_bits()
                    };
                    self.set_sreg(2 * rd + k, out);
                }
                self.regs[15] = next;
                return Some(CpuState::Normal);
            }
        }

        // One-register-and-modified-immediate (A7.4.6):
        //   ARM:   1111 001 i 1 D 000 imm3 Vd cmode 0 Q op 1 imm4
        //   Thumb: 111 i 1111 1 D 000 imm3 Vd cmode 0 Q op 1 imm4
        // Fixed within bits[27:0]: bit23==1, bits[21:19]==000, bit7==0, bit4==1.
        // The `i` immediate bit lives at bit24 in ARM (prefix 0xf2/0xf3) but at
        // bit28 in Thumb (prefix 0xef/0xff) — grab it from the right place.
        if (insn >> 23) & 1 == 1
            && (insn >> 19) & 0b111 == 0
            && (insn >> 7) & 1 == 0
            && (insn >> 4) & 1 == 1
        {
            let pfx = insn >> 24;
            let d = (insn >> 22) & 1;
            let vd = (insn >> 12) & 0xf;
            let q = (insn >> 6) & 1;
            let op = (insn >> 5) & 1;
            let cmode = (insn >> 8) & 0xf;
            let i = if pfx == 0xef || pfx == 0xff {
                (insn >> 28) & 1
            } else {
                (insn >> 24) & 1
            };
            let imm3 = (insn >> 16) & 0b111;
            let imm4 = insn & 0xf;
            let imm8 = (i << 7) | (imm3 << 4) | imm4;
            let (imm64, ok) = neon_modimm(cmode, op, imm8);
            if !ok {
                return None;
            }
            let reg = ((d << 4) | vd) as usize; // d register number
            if q == 1 {
                // Quad: write both halves (dreg index = reg, but Q form uses Vd<3:1>).
                let qbase = (reg & !1) as usize;
                self.set_dreg(qbase, imm64);
                self.set_dreg(qbase + 1, imm64);
            } else {
                self.set_dreg(reg, imm64);
            }
            self.regs[15] = next;
            return Some(CpuState::Normal);
        }

        // Three-registers-same-length, the bitwise ops VAND/VORR/VEOR (used as
        // moves / masks). 111 0 1111 0 D oo Vn Vd 0001 N Q M 1 Vm, oo selects op.
        if (insn >> 23) & 0b11111 == 0b11110 && (insn >> 8) & 0xf == 0b0001 && (insn >> 4) & 1 == 1 {
            let d = (insn >> 22) & 1;
            let vd = (insn >> 12) & 0xf;
            let n = (insn >> 7) & 1;
            let vn = (insn >> 16) & 0xf;
            let m = (insn >> 5) & 1;
            let vm = insn & 0xf;
            let q = (insn >> 6) & 1;
            let oo = (insn >> 20) & 0b11; // 00=VAND,01=VBIC,10=VORR,11=VORN
            let rd = ((d << 4) | vd) as usize;
            let rn = ((n << 4) | vn) as usize;
            let rm = ((m << 4) | vm) as usize;
            let lanes = if q == 1 { 2 } else { 1 };
            for k in 0..lanes {
                let a = self.get_dreg(rn + k);
                let b = self.get_dreg(rm + k);
                let r = match oo {
                    0b00 => a & b,
                    0b01 => a & !b,
                    0b10 => a | b,
                    _ => a | !b,
                };
                self.set_dreg(rd + k, r);
            }
            self.regs[15] = next;
            return Some(CpuState::Normal);
        }
        None
    }

    /// NEON element/structure load/store — VLD1 / VST1 "multiple single elements"
    /// (the {dN} / {dN-dM} register-list forms the game uses for f32 vectors).
    /// Encoding (A7.7): 1111 0100 0D L0 Rn Vd type sz align Rm, type∈{0111,1010,
    /// 0110,0010} = 1/2/3/4 registers.
    fn neon_ldst(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> Option<CpuState> {
        let next = pc.wrapping_add(4);
        // Only handle "multiple single elements" (bit23==0, the A1 form).
        let l = (insn >> 21) & 1; // 1 = load
        let rn = ((insn >> 16) & 0xf) as usize;
        let d = (insn >> 22) & 1;
        let vd = (insn >> 12) & 0xf;
        let typ = (insn >> 8) & 0xf;
        let rm = (insn & 0xf) as usize;
        let dreg = ((d << 4) | vd) as usize;

        // Number of d-registers from `type`.
        let regs = match typ {
            0b0111 => 1,
            0b1010 => 2,
            0b0110 => 3,
            0b0010 => 4,
            _ => return None,
        };
        let mut addr = self.get_reg(rn);
        for i in 0..regs {
            let r = dreg + i;
            if l == 1 {
                let (Some(lo), Some(hi)) =
                    (self.data_r_u32(mem, addr), self.data_r_u32(mem, addr.wrapping_add(4)))
                else {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                };
                self.set_dreg(r, (lo as u64) | ((hi as u64) << 32));
            } else {
                let v = self.get_dreg(r);
                if !self.data_w_u32(mem, addr, v as u32)
                    || !self.data_w_u32(mem, addr.wrapping_add(4), (v >> 32) as u32)
                {
                    self.regs[15] = next;
                    return Some(CpuState::Error(CpuError::MemoryError));
                }
            }
            addr = addr.wrapping_add(8);
        }
        // Write-back: Rm==15 → no wb; Rm==13 → wb by transfer size; else wb by Rm.
        let bytes = (regs as u32) * 8;
        if rm == 15 {
            // no write-back
        } else if rm == 13 {
            self.regs[rn] = self.get_reg(rn).wrapping_add(bytes);
        } else {
            self.regs[rn] = self.get_reg(rn).wrapping_add(self.get_reg(rm));
        }
        self.regs[15] = next;
        Some(CpuState::Normal)
    }
}

/// FP comparison → (N,Z,C,V) per ARM VCMP semantics. `None` ordering = unordered
/// (a NaN operand).
fn fp_cmp_flags(ord: Option<std::cmp::Ordering>) -> (u32, u32, u32, u32) {
    use std::cmp::Ordering::*;
    match ord {
        Some(Equal) => (0, 1, 1, 0),
        Some(Less) => (1, 0, 0, 0),
        Some(Greater) => (0, 0, 1, 0),
        None => (0, 0, 1, 1), // unordered
    }
}

/// FP → signed i32 with ARM saturating, round-toward-zero semantics.
fn fp_to_i32(f: f64) -> i32 {
    if f.is_nan() {
        0
    } else if f >= i32::MAX as f64 {
        i32::MAX
    } else if f <= i32::MIN as f64 {
        i32::MIN
    } else {
        f.trunc() as i32
    }
}
/// FP → unsigned u32 with ARM saturating, round-toward-zero semantics.
fn fp_to_u32(f: f64) -> u32 {
    if f.is_nan() {
        0
    } else if f >= u32::MAX as f64 {
        u32::MAX
    } else if f <= 0.0 {
        0
    } else {
        f.trunc() as u32
    }
}

/// NEON modified-immediate expansion (AdvSIMDExpandImm, A7.4.6) for the cmode/op
/// combinations the game uses (chiefly cmode=0000 → I32, and the all-zero case).
/// Returns (imm64 replicated into the doubleword, recognised?).
fn neon_modimm(cmode: u32, op: u32, imm8: u32) -> (u64, bool) {
    let b = imm8 as u64;
    let rep32 = |x: u64| x | (x << 32);
    match (cmode >> 1, cmode & 1, op) {
        // cmode 000x: I32, imm8 in byte 0 of each 32-bit lane.
        (0b000, _, 0) => (rep32(b), true),
        // cmode 001x: I32, imm8 in byte 1.
        (0b001, _, 0) => (rep32(b << 8), true),
        // cmode 010x: I32, imm8 in byte 2.
        (0b010, _, 0) => (rep32(b << 16), true),
        // cmode 011x: I32, imm8 in byte 3.
        (0b011, _, 0) => (rep32(b << 24), true),
        // cmode 100x: I16, imm8 in byte 0 of each 16-bit lane.
        (0b100, _, 0) => {
            let h = b | (b << 16);
            (h | (h << 32), true)
        }
        // cmode 101x: I16, imm8 in byte 1.
        (0b101, _, 0) => {
            let h = (b << 8) | (b << 24);
            (h | (h << 32), true)
        }
        // cmode 1110, op 0: I8, imm8 in every byte.
        (0b111, 0, 0) => {
            let mut v = 0u64;
            for k in 0..8 {
                v |= b << (k * 8);
            }
            (v, true)
        }
        // cmode 1111, op 0: F32 — VFP-expand imm8 to a float, one per 32-bit lane.
        (0b111, 1, 0) => (rep32(vfp_expand_imm32(imm8) as u64), true),
        // cmode 1110, op 1: I64 — each imm8 bit selects a full byte of 0x00/0xff.
        (0b111, 0, 1) => {
            let mut v = 0u64;
            for k in 0..8 {
                if (imm8 >> k) & 1 == 1 {
                    v |= 0xffu64 << (k * 8);
                }
            }
            (v, true)
        }
        _ => (0, false),
    }
}
