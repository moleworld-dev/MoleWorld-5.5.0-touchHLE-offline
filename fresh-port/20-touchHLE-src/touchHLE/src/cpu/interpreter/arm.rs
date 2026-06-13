/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! ARM (A32) common integer instruction group for the pure-Rust interpreter.
//!
//! Implements ARMv7-A (DDI 0406C) A32 data-processing, MOVW/MOVT, multiply,
//! load/store (immediate + register, single + halfword/signed/dual), LDM/STM,
//! branches (B/BL/BX/BLX), sign/zero extends, bitfield ops, REV/CLZ and the
//! barrier no-ops. IT blocks and VFP/coprocessor are handled elsewhere.
//!
//! Contract (see mod.rs): `exec_arm` returns `Option<CpuState>`:
//!   * `None`          — encoding not recognised by this group.
//!   * `Some(Normal)`  — executed; PC has been advanced / branched.
//!   * `Some(Svc(imm))`— SVC trap.
//!   * `Some(Error(MemoryError))` — a data access faulted (PC already advanced).

use crate::cpu::{CpuError, CpuState};
use crate::mem::Mem;

impl super::InterpreterCpu {
    /// ARMExpandImm_C: rotate an 8-bit immediate by 2*rot, with carry-out.
    /// `imm12` is the low 12 bits of the instruction. Returns (value, carry_out).
    fn arm_expand_imm_c(imm12: u32, carry_in: bool) -> (u32, bool) {
        let unrotated = imm12 & 0xff;
        let rot = ((imm12 >> 8) & 0xf) * 2;
        if rot == 0 {
            (unrotated, carry_in)
        } else {
            Self::ror_c(unrotated, rot)
        }
    }

    /// Decode the register-form shifter operand (bits[11:0]) producing
    /// (value, carry_out). `bit4` selects immediate (0) vs register (1) shift
    /// amount. Caller guarantees the data-processing register form.
    fn arm_shifted_reg(&self, insn: u32) -> (u32, bool) {
        let rm = (insn & 0xf) as usize;
        let stype = (insn >> 5) & 3;
        let carry_in = self.flag_c();
        let m = self.get_reg(rm);
        if insn & (1 << 4) == 0 {
            // Immediate shift amount.
            let imm5 = (insn >> 7) & 0x1f;
            if imm5 == 0 {
                match stype {
                    0 => (m, carry_in),                  // LSL #0
                    1 => Self::lsr_c(m, 32),             // LSR #32
                    2 => Self::asr_c(m, 32),             // ASR #32
                    _ => Self::rrx_c(m, carry_in),       // RRX
                }
            } else {
                Self::shift_c(m, stype, imm5, carry_in)
            }
        } else {
            // Register-controlled shift amount (uses Rs[7:0]).
            let rs = ((insn >> 8) & 0xf) as usize;
            let amount = self.get_reg(rs) & 0xff;
            if amount == 0 {
                (m, carry_in)
            } else {
                match stype {
                    0 => Self::lsl_c(m, amount),
                    1 => Self::lsr_c(m, amount),
                    2 => Self::asr_c(m, amount),
                    _ => {
                        // ROR by register: amount==0 handled above; ROR by 32 → no
                        // change but carry from bit31; ror_c masks &31 so feed the
                        // masked amount, but ROR#0-mod-32 keeps value & carry bit31.
                        let m5 = amount & 0x1f;
                        if m5 == 0 {
                            (m, (m >> 31) & 1 != 0)
                        } else {
                            Self::ror_c(m, m5)
                        }
                    }
                }
            }
        }
    }

    pub(super) fn exec_arm(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> Option<CpuState> {
        let cond = insn >> 28;

        // Unconditional (cond == 0xF) space: BLX(imm), PLD, barriers (the latter
        // also appear here). Handle a few, otherwise fall through to None.
        if cond == 0xF {
            return self.exec_arm_uncond(insn, pc, mem);
        }

        // Conditional-but-failed: skip (advance PC).
        if !self.cond_passed(cond) {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- SVC (host-call) fallback: svc #imm (0xefxxxxxx after cond strip) -----
        if (insn & 0x0f00_0000) == 0x0f00_0000 {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Svc(insn & 0x00ff_ffff));
        }

        // ----- Branch / branch-with-link (immediate): cond 101L imm24 -----
        if (insn & 0x0e00_0000) == 0x0a00_0000 {
            let link = insn & (1 << 24) != 0;
            let imm24 = insn & 0x00ff_ffff;
            let off = ((imm24 << 8) as i32 >> 6) as u32; // sign-extend, <<2
            let pc_read = self.get_reg(15); // PC+8
            if link {
                self.regs[14] = pc.wrapping_add(4);
            }
            self.regs[15] = pc_read.wrapping_add(off);
            return Some(CpuState::Normal);
        }

        // ----- BX / BLX / BXJ (register), CLZ : cond 0001 0010 ... 0001/0011 Rm -----
        // Miscellaneous: 0001 0xx0 ...
        if (insn & 0x0fff_fff0) == 0x012f_ff10 {
            // BX Rm
            let rm = (insn & 0xf) as usize;
            let target = self.get_reg(rm);
            self.bx_write_pc(target);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0fff_fff0) == 0x012f_ff30 {
            // BLX Rm
            let rm = (insn & 0xf) as usize;
            let target = self.get_reg(rm);
            self.regs[14] = pc.wrapping_add(4);
            self.bx_write_pc(target);
            return Some(CpuState::Normal);
        }
        if (insn & 0x0fff_0ff0) == 0x016f_0f10 {
            // CLZ Rd, Rm
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            self.regs[rd] = self.get_reg(rm).leading_zeros();
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- Multiply family: cond 0000 ... 1001 (bits 27..24 == 0, bits7..4==1001)
        if (insn & 0x0fc0_00f0) == 0x0000_0090 {
            // MUL / MLA  (bit21 = A)
            return Some(self.exec_mul_mla(insn, pc));
        }
        if (insn & 0x0f80_00f0) == 0x0080_0090 {
            // UMULL/UMLAL/SMULL/SMLAL (bits23..21 select)
            return Some(self.exec_mull(insn, pc));
        }

        // ----- Extra load/store: halfword, signed byte/halfword, doubleword -----
        // These share bits: 000 ... 1xx1 with bit4==1 and bit7==1.
        if (insn & 0x0e00_0090) == 0x0000_0090 && (insn & 0x0000_0060) != 0 {
            return Some(self.exec_extra_ldst(insn, pc, mem));
        }

        // ----- Media: bitfield / extends / rev (cond 011x xxxx ... bit4==1) -----
        if (insn & 0x0e00_0010) == 0x0600_0010 {
            if let Some(st) = self.exec_media(insn, pc) {
                return Some(st);
            }
        }

        // ----- MOVW / MOVT (16-bit immediate) : cond 0011 0x00 -----
        if (insn & 0x0fb0_0000) == 0x0300_0000 {
            let imm4 = (insn >> 16) & 0xf;
            let imm12 = insn & 0xfff;
            let imm16 = (imm4 << 12) | imm12;
            let rd = ((insn >> 12) & 0xf) as usize;
            if insn & (1 << 22) == 0 {
                // MOVW: Rd = imm16
                self.regs[rd] = imm16;
            } else {
                // MOVT: Rd[31:16] = imm16, keep low half
                self.regs[rd] = (self.get_reg(rd) & 0x0000_ffff) | (imm16 << 16);
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // ----- Data processing (immediate / reg / reg-shifted-reg) -----
        // cond 00x ... opcode at bits24..21, S at bit20.
        if (insn & 0x0c00_0000) == 0x0000_0000 {
            if let Some(st) = self.exec_data_proc(insn, pc) {
                return Some(st);
            }
        }

        // ----- Single load/store (LDR/STR/LDRB/STRB) : cond 01x ... -----
        if (insn & 0x0c00_0000) == 0x0400_0000 {
            // Exclude media (bit25==1 && bit4==1) which is handled above.
            let media = (insn & (1 << 25)) != 0 && (insn & (1 << 4)) != 0;
            if !media {
                return Some(self.exec_single_ldst(insn, pc, mem));
            }
        }

        // ----- LDM / STM : cond 100 ... -----
        if (insn & 0x0e00_0000) == 0x0800_0000 {
            return Some(self.exec_ldm_stm(insn, pc, mem));
        }

        // ----- Coprocessor / VFP (cond 11xx): VLDM/VSTM/VLDR/VSTR/VFP -----
        if (insn & 0x0c00_0000) == 0x0c00_0000 {
            return self.exec_vfp(insn, pc, mem);
        }

        None
    }

    fn exec_arm_uncond(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> Option<CpuState> {
        // BLX (immediate): 1111 101H imm24 — always switches to Thumb.
        if (insn & 0xfe00_0000) == 0xfa00_0000 {
            let h = (insn >> 24) & 1;
            let imm24 = insn & 0x00ff_ffff;
            let off = (((imm24 << 8) as i32 >> 6) as u32) | (h << 1);
            let pc_read = self.get_reg(15); // PC+8
            self.regs[14] = pc.wrapping_add(4);
            // Target is current ARM PC + off, set Thumb bit.
            self.bx_write_pc(pc_read.wrapping_add(off) | 1);
            return Some(CpuState::Normal);
        }
        // PLD / PLI / barriers (DMB/DSB/ISB) / CLREX / NOP-ish: treat as no-op.
        // DMB: 1111 0101 0111 1111 1111 0000 0101 xxxx
        // DSB: ... 0100 xxxx ; ISB: ... 0110 xxxx ; CLREX: ... 0001 1111
        if (insn & 0xfff0_0000) == 0xf550_0000 {
            // PLD (immediate/literal) — hint, no-op.
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        if (insn & 0xffff_fff0) == 0xf57f_f040 // DSB
            || (insn & 0xffff_fff0) == 0xf57f_f050 // DMB
            || (insn & 0xffff_fff0) == 0xf57f_f060 // ISB
            || (insn & 0xffff_ffff) == 0xf57f_f01f
        // CLREX
        {
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // Advanced SIMD (NEON) — the unconditional encodings: data-processing
        // 1111 001x (0xf2/0xf3) and element/structure load/store 1111 0100
        // (0xf4). VFP data-proc/transfers are conditional (cond != 1111) and
        // reach exec_vfp via the coprocessor path in exec_arm instead.
        let top = insn >> 24;
        if top == 0xf2 || top == 0xf3 || top == 0xf4 {
            return self.exec_vfp(insn, pc, mem);
        }
        None
    }

    /// Data-processing: AND/EOR/SUB/RSB/ADD/ADC/SBC/RSC/TST/TEQ/CMP/CMN/ORR/MOV/BIC/MVN.
    fn exec_data_proc(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        let is_imm = insn & (1 << 25) != 0;
        // Register-shifted-register requires bit4==1 && bit7==0 in the reg form;
        // bit7==1 with bit4==1 in non-imm space is the extra-ldst/media we already
        // excluded. Guard here: in reg form, if bit4==1 && bit7==1 it's not DP.
        if !is_imm && (insn & (1 << 4)) != 0 && (insn & (1 << 7)) != 0 {
            return None;
        }

        let opcode = (insn >> 21) & 0xf;
        let s = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let rd = ((insn >> 12) & 0xf) as usize;

        let carry_in = self.flag_c();
        let (operand2, shifter_c) = if is_imm {
            Self::arm_expand_imm_c(insn & 0xfff, carry_in)
        } else {
            self.arm_shifted_reg(insn)
        };

        let n = self.get_reg(rn);

        // Compute result + flags depending on opcode.
        // Logical ops use shifter carry; arithmetic ops use AddWithCarry carry/v.
        let mut result: u32 = 0;
        let mut write_result = true;
        let mut set_c = shifter_c;
        let mut set_v = self.flag_v();
        let mut arith = false;

        match opcode {
            0x0 => result = n & operand2,                  // AND
            0x1 => result = n ^ operand2,                  // EOR
            0x2 => {
                // SUB
                let (r, c, v) = Self::add_with_carry(n, !operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x3 => {
                // RSB
                let (r, c, v) = Self::add_with_carry(!n, operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x4 => {
                // ADD
                let (r, c, v) = Self::add_with_carry(n, operand2, false);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x5 => {
                // ADC
                let (r, c, v) = Self::add_with_carry(n, operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x6 => {
                // SBC
                let (r, c, v) = Self::add_with_carry(n, !operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x7 => {
                // RSC
                let (r, c, v) = Self::add_with_carry(!n, operand2, carry_in);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
            }
            0x8 => {
                // TST (always S)
                result = n & operand2;
                write_result = false;
            }
            0x9 => {
                // TEQ
                result = n ^ operand2;
                write_result = false;
            }
            0xA => {
                // CMP
                let (r, c, v) = Self::add_with_carry(n, !operand2, true);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
                write_result = false;
            }
            0xB => {
                // CMN
                let (r, c, v) = Self::add_with_carry(n, operand2, false);
                result = r;
                set_c = c;
                set_v = v;
                arith = true;
                write_result = false;
            }
            0xC => result = n | operand2,                  // ORR
            0xD => result = operand2,                      // MOV (and MOVS/LSL/etc reg form)
            0xE => result = n & !operand2,                 // BIC
            0xF => result = !operand2,                     // MVN
            _ => unreachable!(),
        }

        // TST/TEQ/CMP/CMN always set flags.
        let always_s = matches!(opcode, 0x8 | 0x9 | 0xA | 0xB);

        if write_result {
            if rd == 15 {
                // Writing PC. In user code: ALUWritePC (branch). If S — exception
                // return (not modelled here); we approximate with branch + flags
                // from result is not standard, but S+PC is rare in app code. Do a
                // plain branch (bx semantics for ARM DP write to PC keeps ARM
                // state); set flags if S as best-effort.
                if s && !always_s {
                    if arith {
                        self.set_nzcv(result, set_c, set_v);
                    } else {
                        self.set_nzcv(result, set_c, self.flag_v());
                    }
                }
                // BXWritePC in ARMv7 for DP write: actually BranchWritePC (stays
                // ARM, aligns to 4). Use bx_write_pc with bit0 cleared.
                self.bx_write_pc(result & !1);
                return Some(CpuState::Normal);
            }
            self.regs[rd] = result;
        }

        if s || always_s {
            if arith {
                self.set_nzcv(result, set_c, set_v);
            } else {
                // Logical: N,Z from result, C from shifter, V unchanged.
                self.set_nzcv(result, set_c, self.flag_v());
            }
        }

        self.regs[15] = pc.wrapping_add(4);
        Some(CpuState::Normal)
    }

    /// MUL (A==0) / MLA (A==1). 32-bit result; S sets N,Z.
    fn exec_mul_mla(&mut self, insn: u32, pc: u32) -> CpuState {
        let a = insn & (1 << 21) != 0;
        let s = insn & (1 << 20) != 0;
        let rd = ((insn >> 16) & 0xf) as usize;
        let ra = ((insn >> 12) & 0xf) as usize; // accumulate (MLA)
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let prod = self.get_reg(rm).wrapping_mul(self.get_reg(rs));
        let result = if a {
            prod.wrapping_add(self.get_reg(ra))
        } else {
            prod
        };
        self.regs[rd] = result;
        if s {
            self.set_nz(result);
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// UMULL/UMLAL/SMULL/SMLAL. bit22=U/S(0=unsigned),bit21=A(accumulate).
    fn exec_mull(&mut self, insn: u32, pc: u32) -> CpuState {
        let signed = insn & (1 << 22) != 0;
        let accumulate = insn & (1 << 21) != 0;
        let s = insn & (1 << 20) != 0;
        let rdhi = ((insn >> 16) & 0xf) as usize;
        let rdlo = ((insn >> 12) & 0xf) as usize;
        let rs = ((insn >> 8) & 0xf) as usize;
        let rm = (insn & 0xf) as usize;
        let m = self.get_reg(rm);
        let n = self.get_reg(rs);

        let mut full: u64 = if signed {
            ((m as i32 as i64).wrapping_mul(n as i32 as i64)) as u64
        } else {
            (m as u64).wrapping_mul(n as u64)
        };
        if accumulate {
            let acc = ((self.get_reg(rdhi) as u64) << 32) | (self.get_reg(rdlo) as u64);
            full = full.wrapping_add(acc);
        }
        self.regs[rdlo] = full as u32;
        self.regs[rdhi] = (full >> 32) as u32;
        if s {
            // N from bit63, Z from whole 64-bit result.
            self.cpsr &= !(0b11 << 30);
            self.cpsr |= (((full >> 63) & 1) as u32) << 31;
            if full == 0 {
                self.cpsr |= 1 << 30;
            }
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Extra load/store: LDRH/STRH/LDRSB/LDRSH/LDRD/STRD.
    fn exec_extra_ldst(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> CpuState {
        let p = insn & (1 << 24) != 0; // pre-index
        let u = insn & (1 << 23) != 0; // add
        let imm_form = insn & (1 << 22) != 0; // 1 = immediate offset
        let w = insn & (1 << 21) != 0; // writeback (or post always writes back)
        let l = insn & (1 << 20) != 0; // load
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;
        let op = (insn >> 5) & 3; // bits[6:5]: SH

        let offset = if imm_form {
            ((insn >> 4) & 0xf0) | (insn & 0xf)
        } else {
            self.get_reg((insn & 0xf) as usize)
        };

        let base = self.get_reg(rn);
        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if p { offset_addr } else { base };
        // Writeback for pre-index requires W; post-index always writes back.
        let wback = (!p) || w;

        let mut fault = false;

        // op encodes the access:
        //   L=0: op=1 STRH, op=2 LDRD, op=3 STRD
        //   L=1: op=1 LDRH, op=2 LDRSB, op=3 LDRSH
        match (l, op) {
            (false, 1) => {
                // STRH
                if !self.data_w_u16(mem, address, self.get_reg(rt) as u16) {
                    fault = true;
                }
            }
            (true, 1) => {
                // LDRH
                match self.data_r_u16(mem, address) {
                    Some(v) => self.regs[rt] = v as u32,
                    None => fault = true,
                }
            }
            (true, 2) => {
                // LDRSB
                match self.data_r_u8(mem, address) {
                    Some(v) => self.regs[rt] = v as i8 as i32 as u32,
                    None => fault = true,
                }
            }
            (true, 3) => {
                // LDRSH
                match self.data_r_u16(mem, address) {
                    Some(v) => self.regs[rt] = v as i16 as i32 as u32,
                    None => fault = true,
                }
            }
            (false, 2) => {
                // LDRD (Rt, Rt+1) — op=2,L=0
                let lo = self.data_r_u32(mem, address);
                let hi = self.data_r_u32(mem, address.wrapping_add(4));
                match (lo, hi) {
                    (Some(a), Some(b)) => {
                        self.regs[rt] = a;
                        self.regs[rt + 1] = b;
                    }
                    _ => fault = true,
                }
            }
            (false, 3) => {
                // STRD (Rt, Rt+1) — op=3,L=0
                let ok1 = self.data_w_u32(mem, address, self.get_reg(rt));
                let ok2 = self.data_w_u32(mem, address.wrapping_add(4), self.get_reg(rt + 1));
                if !ok1 || !ok2 {
                    fault = true;
                }
            }
            _ => {
                // Shouldn't happen given the dispatch guard.
            }
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        if wback && rn != 15 {
            self.regs[rn] = offset_addr;
        }
        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Single data transfer: LDR/STR/LDRB/STRB (immediate-12 or register+shift).
    fn exec_single_ldst(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> CpuState {
        let is_reg = insn & (1 << 25) != 0;
        let p = insn & (1 << 24) != 0;
        let u = insn & (1 << 23) != 0;
        let b = insn & (1 << 22) != 0; // byte
        let w = insn & (1 << 21) != 0;
        let l = insn & (1 << 20) != 0;
        let rn = ((insn >> 16) & 0xf) as usize;
        let rt = ((insn >> 12) & 0xf) as usize;

        let offset = if is_reg {
            // Register offset with optional immediate shift.
            let rm = (insn & 0xf) as usize;
            let stype = (insn >> 5) & 3;
            let imm5 = (insn >> 7) & 0x1f;
            let m = self.get_reg(rm);
            let carry_in = self.flag_c();
            let (val, _) = if imm5 == 0 {
                match stype {
                    0 => (m, carry_in),
                    1 => Self::lsr_c(m, 32),
                    2 => Self::asr_c(m, 32),
                    _ => Self::rrx_c(m, carry_in),
                }
            } else {
                Self::shift_c(m, stype, imm5, carry_in)
            };
            val
        } else {
            insn & 0xfff
        };

        // For PC-relative (Rn==15) use Align(PC,4).
        let base = if rn == 15 {
            self.get_reg_align(15)
        } else {
            self.get_reg(rn)
        };
        let offset_addr = if u {
            base.wrapping_add(offset)
        } else {
            base.wrapping_sub(offset)
        };
        let address = if p { offset_addr } else { base };
        let wback = !p || w;

        let mut fault = false;

        if l {
            if b {
                match self.data_r_u8(mem, address) {
                    Some(v) => self.regs[rt] = v as u32,
                    None => fault = true,
                }
            } else {
                match self.data_r_u32(mem, address) {
                    Some(v) => {
                        if rt == 15 {
                            // LDR to PC: interworking branch.
                            // Defer the write until after wback.
                            self.regs[rt] = v; // temp; corrected below
                        } else {
                            self.regs[rt] = v;
                        }
                    }
                    None => fault = true,
                }
            }
        } else {
            let v = self.get_reg(rt);
            if b {
                if !self.data_w_u8(mem, address, v as u8) {
                    fault = true;
                }
            } else if !self.data_w_u32(mem, address, v) {
                fault = true;
            }
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        if wback && rn != 15 {
            self.regs[rn] = offset_addr;
        }

        if l && !b && rt == 15 {
            // LDR PC: interworking.
            let v = self.regs[15];
            self.bx_write_pc(v);
            return CpuState::Normal;
        }

        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// LDM/STM (IA/IB/DA/DB), writeback, LDM with PC in list.
    fn exec_ldm_stm(&mut self, insn: u32, pc: u32, mem: &mut Mem) -> CpuState {
        let p = insn & (1 << 24) != 0; // before
        let u = insn & (1 << 23) != 0; // increment
        let w = insn & (1 << 21) != 0; // writeback
        let l = insn & (1 << 20) != 0; // load
        let rn = ((insn >> 16) & 0xf) as usize;
        let reg_list = insn & 0xffff;
        let count = reg_list.count_ones();
        if count == 0 {
            // Unpredictable; treat as no-op advancing PC.
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Normal;
        }

        let base = self.get_reg(rn);
        // Lowest address accessed.
        let start = if u {
            if p {
                base.wrapping_add(4) // IB
            } else {
                base // IA
            }
        } else if p {
            base.wrapping_sub(count * 4) // DB
        } else {
            base.wrapping_sub(count * 4).wrapping_add(4) // DA
        };

        let mut addr = start;
        let mut fault = false;
        let mut pc_loaded: Option<u32> = None;

        // Registers are accessed lowest-numbered at lowest address.
        for i in 0..16 {
            if reg_list & (1 << i) == 0 {
                continue;
            }
            if l {
                match self.data_r_u32(mem, addr) {
                    Some(v) => {
                        if i == 15 {
                            pc_loaded = Some(v);
                        } else {
                            self.regs[i] = v;
                        }
                    }
                    None => {
                        fault = true;
                        break;
                    }
                }
            } else {
                // STM stores the original base for Rn even with writeback if Rn is
                // the lowest register; we store current reg value (R15 = PC+8).
                let val = self.get_reg(i);
                if !self.data_w_u32(mem, addr, val) {
                    fault = true;
                    break;
                }
            }
            addr = addr.wrapping_add(4);
        }

        if fault {
            self.regs[15] = pc.wrapping_add(4);
            return CpuState::Error(CpuError::MemoryError);
        }

        // Writeback: new base = base ± count*4.
        if w {
            let new_base = if u {
                base.wrapping_add(count * 4)
            } else {
                base.wrapping_sub(count * 4)
            };
            // If Rn is in the list and it's a load, value loaded wins (already set).
            // For store with Rn in list and writeback, architecturally the stored
            // value should be the original/updated base depending on position; we
            // keep it simple (lowest-in-list stores original base, which our loop
            // already did since we read get_reg before writeback).
            self.regs[rn] = new_base;
        }

        if let Some(v) = pc_loaded {
            self.bx_write_pc(v);
            return CpuState::Normal;
        }

        self.regs[15] = pc.wrapping_add(4);
        CpuState::Normal
    }

    /// Media instructions: UXTB/SXTB/UXTH/SXTH/UBFX/SBFX/BFI/BFC/REV.
    fn exec_media(&mut self, insn: u32, pc: u32) -> Option<CpuState> {
        // Sign/zero extends: cond 0110 1mm0 1111 Rd rot 00 0111 Rm
        //   SXTB:  0110 1010 1111 ...  (op 0x6A)
        //   SXTH:  0110 1011 1111 ...  (op 0x6B)
        //   UXTB:  0110 1110 1111 ...  (op 0x6E)
        //   UXTH:  0110 1111 1111 ...  (op 0x6F)
        // The "1111" at Rn means no add (the with-Rn forms are *XTA*, not here).
        let op_high = (insn >> 20) & 0xff; // bits 27..20
        let rn = (insn >> 16) & 0xf;
        if (insn & 0x0000_03f0) == 0x0000_0070 && rn == 0xf {
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            let rot = ((insn >> 10) & 3) * 8;
            let rotated = self.get_reg(rm).rotate_right(rot);
            match op_high {
                0x6a => {
                    // SXTB
                    self.regs[rd] = (rotated as u8) as i8 as i32 as u32;
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Normal);
                }
                0x6b => {
                    // SXTH
                    self.regs[rd] = (rotated as u16) as i16 as i32 as u32;
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Normal);
                }
                0x6e => {
                    // UXTB
                    self.regs[rd] = rotated & 0xff;
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Normal);
                }
                0x6f => {
                    // UXTH
                    self.regs[rd] = rotated & 0xffff;
                    self.regs[15] = pc.wrapping_add(4);
                    return Some(CpuState::Normal);
                }
                _ => {}
            }
        }

        // REV: cond 0110 1011 1111 Rd 1111 0011 Rm  (op 0x6B, bits 11..4 == 0xF3)
        if (insn & 0x0fff_0ff0) == 0x06bf_0f30 {
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            self.regs[rd] = self.get_reg(rm).swap_bytes();
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }
        // REV16: 0110 1011 1111 Rd 1111 1011 Rm
        if (insn & 0x0fff_0ff0) == 0x06bf_0fb0 {
            let rd = ((insn >> 12) & 0xf) as usize;
            let rm = (insn & 0xf) as usize;
            let v = self.get_reg(rm);
            self.regs[rd] = ((v & 0x00ff_00ff) << 8) | ((v >> 8) & 0x00ff_00ff);
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // SBFX: cond 0111 101 widthm1 Rd lsb 101 Rn
        // UBFX: cond 0111 111 widthm1 Rd lsb 101 Rn
        if (insn & 0x0fe0_0070) == 0x07a0_0050 || (insn & 0x0fe0_0070) == 0x07e0_0050 {
            let unsigned = (insn & (1 << 22)) != 0;
            let widthm1 = (insn >> 16) & 0x1f;
            let rd = ((insn >> 12) & 0xf) as usize;
            let lsb = (insn >> 7) & 0x1f;
            let rn = (insn & 0xf) as usize;
            let width = widthm1 + 1;
            let src = self.get_reg(rn);
            let field = if lsb + width >= 32 {
                src >> lsb
            } else {
                (src >> lsb) & ((1u32 << width) - 1)
            };
            self.regs[rd] = if unsigned {
                field
            } else {
                // Sign-extend from bit (width-1).
                let shift = 32 - width;
                (((field << shift) as i32) >> shift) as u32
            };
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        // BFI / BFC: cond 0111 110 msb Rd lsb 001 Rn
        //   BFC if Rn==1111 (clear); else BFI (insert from Rn).
        if (insn & 0x0fe0_0070) == 0x07c0_0010 {
            let msb = (insn >> 16) & 0x1f;
            let rd = ((insn >> 12) & 0xf) as usize;
            let lsb = (insn >> 7) & 0x1f;
            let rn = (insn & 0xf) as usize;
            if msb >= lsb {
                let width = msb - lsb + 1;
                let mask = if width >= 32 {
                    0xffff_ffffu32
                } else {
                    ((1u32 << width) - 1) << lsb
                };
                let cur = self.get_reg(rd);
                if rn == 0xf {
                    // BFC
                    self.regs[rd] = cur & !mask;
                } else {
                    let src = self.get_reg(rn);
                    let ins = (src << lsb) & mask;
                    self.regs[rd] = (cur & !mask) | ins;
                }
            }
            self.regs[15] = pc.wrapping_add(4);
            return Some(CpuState::Normal);
        }

        None
    }
}

#[cfg(test)]
#[cfg(all(feature = "cpu_dynarmic", feature = "cpu_interpreter"))]
mod tests {
    use super::super::diff::{check, check_mem, STACK_TOP};

    fn r(setup: &[(usize, u32)]) -> [u32; 16] {
        let mut regs = [0u32; 16];
        regs[13] = STACK_TOP;
        for &(i, v) in setup {
            regs[i] = v;
        }
        regs
    }

    // ---------------- Data processing (immediate) ----------------
    #[test]
    fn arm_mov_imm() {
        // MOV r0, #1   : e3a00001
        check("MOV r0,#1", 0xe3a0_0001, 4, false, r(&[]), 0);
        // MOVS r0, #0  : e3b00000  (Z set)
        check("MOVS r0,#0", 0xe3b0_0000, 4, false, r(&[]), 0);
        // MOVS r0, #0xff000000 via rotate (imm=0xff, rot=4 → ror 8)
        // MOV r1, #0xff000000 : imm12 = (4<<8)|0xff = 0x4ff → e3a014ff
        check("MOVS rot carry", 0xe3b0_14ff, 4, false, r(&[]), 0);
    }

    #[test]
    fn arm_add_sub_imm() {
        check("ADD r0,r1,#1", 0xe281_0001, 4, false, r(&[(1, 41)]), 0);
        check("ADDS ovf", 0xe291_0001, 4, false, r(&[(1, 0x7fff_ffff)]), 0);
        check("ADDS carry", 0xe291_0001, 4, false, r(&[(1, 0xffff_ffff)]), 0);
        check("SUBS 5-3", 0xe251_0003, 4, false, r(&[(1, 5)]), 0);
        check("SUBS 3-5", 0xe251_0005, 4, false, r(&[(1, 3)]), 0);
        check("RSBS 0-x", 0xe271_0000, 4, false, r(&[(1, 7)]), 0);
    }

    #[test]
    fn arm_adc_sbc_imm() {
        // ADCS r0, r1, #0 with C=1
        check("ADCS C=1", 0xe2b1_0000, 4, false, r(&[(1, 1)]), 0x2000_0000);
        // SBCS r0, r1, #0 with C=0 (borrow)
        check("SBCS C=0", 0xe2d1_0000, 4, false, r(&[(1, 5)]), 0);
        check("SBCS C=1", 0xe2d1_0001, 4, false, r(&[(1, 5)]), 0x2000_0000);
    }

    #[test]
    fn arm_logical_imm() {
        check("ANDS", 0xe211_000f, 4, false, r(&[(1, 0xff)]), 0);
        check("ORRS", 0xe391_0f00, 4, false, r(&[(1, 0xff)]), 0);
        check("EORS", 0xe231_00ff, 4, false, r(&[(1, 0xf0)]), 0);
        check("BICS", 0xe3d1_000f, 4, false, r(&[(1, 0xff)]), 0);
        check("MVNS", 0xe3f0_0000, 4, false, r(&[]), 0);
        // imm rotate sets shifter carry into logical: MOVS r0,#0xff000000
        check("ANDS rot carry", 0xe21014ff, 4, false, r(&[(1, 0xffff_ffff)]), 0);
    }

    #[test]
    fn arm_test_compare_imm() {
        check("TST", 0xe311_0001, 4, false, r(&[(1, 1)]), 0);
        check("TEQ", 0xe331_00ff, 4, false, r(&[(1, 0xff)]), 0);
        check("CMP eq", 0xe351_0005, 4, false, r(&[(1, 5)]), 0);
        check("CMP lt", 0xe351_0009, 4, false, r(&[(1, 3)]), 0);
        check("CMN", 0xe371_0001, 4, false, r(&[(1, 0xffff_ffff)]), 0);
    }

    // ---------------- Data processing (register + shift) ----------------
    #[test]
    fn arm_dp_reg() {
        // ADDS r0, r1, r2 : e0910002
        check("ADDS reg", 0xe091_0002, 4, false, r(&[(1, 10), (2, 20)]), 0);
        // ADD r0, r1, r2, LSL #4 : e0810202
        check("ADD lsl#4", 0xe081_0202, 4, false, r(&[(1, 1), (2, 3)]), 0);
        // MOVS r0, r1, LSR #1 (carry from shifted-out bit) : e1b000a1
        check("MOVS lsr#1 c", 0xe1b0_00a1, 4, false, r(&[(1, 3)]), 0);
        // MOVS r0, r1, ASR #32 (imm5==0) : e1b00041
        check("MOVS asr#32", 0xe1b0_0041, 4, false, r(&[(1, 0x8000_0000)]), 0);
        // MOVS r0, r1, RRX : e1b00061 with C set
        check("MOVS rrx", 0xe1b0_0061, 4, false, r(&[(1, 2)]), 0x2000_0000);
    }

    #[test]
    fn arm_dp_reg_shifted_reg() {
        // MOVS r0, r1, LSL r2 : e1b00211
        check("MOVS lsl rs", 0xe1b0_0211, 4, false, r(&[(1, 1), (2, 4)]), 0);
        check("MOVS lsl rs32", 0xe1b0_0211, 4, false, r(&[(1, 1), (2, 32)]), 0);
        // ADDS r0, r1, r2, LSL r3 : e0910312
        check("ADDS lsl rs", 0xe091_0312, 4, false, r(&[(1, 1), (2, 1), (3, 8)]), 0);
    }

    // ---------------- MOVW / MOVT ----------------
    #[test]
    fn arm_movw_movt() {
        // MOVW r0, #0x1234 : e3001234
        check("MOVW", 0xe300_1234, 4, false, r(&[]), 0);
        // MOVT r0, #0xabcd : e34a0bcd  (imm16=0xabcd → imm4=a, imm12=bcd)
        check("MOVT", 0xe34a_0bcd, 4, false, r(&[(0, 0x0000_5678)]), 0);
    }

    // ---------------- Multiply ----------------
    #[test]
    fn arm_mul_family() {
        // MUL r0, r1, r2 : e0000291  (Rd=0 at 19..16, Rm=1, Rs=2)
        check("MUL", 0xe000_0291, 4, false, r(&[(1, 6), (2, 7)]), 0);
        // MULS r0, r1, r2 : e0100291
        check("MULS neg", 0xe010_0291, 4, false, r(&[(1, 0xffff_ffff), (2, 1)]), 0);
        // MLA r0, r1, r2, r3 : e0203291  (Ra=r3 at 15..12)
        check("MLA", 0xe020_3291, 4, false, r(&[(1, 6), (2, 7), (3, 100)]), 0);
        // UMULL r0(lo), r1(hi), r2, r3 : e0810392
        check("UMULL", 0xe081_0392, 4, false, r(&[(2, 0xffff_ffff), (3, 2)]), 0);
        // SMULL r0, r1, r2, r3 : e0c10392
        check("SMULL", 0xe0c1_0392, 4, false, r(&[(2, 0xffff_ffff), (3, 2)]), 0);
        // UMLAL r0, r1, r2, r3 : e0a10392
        check("UMLAL", 0xe0a1_0392, 4, false, r(&[(0, 5), (1, 0), (2, 4), (3, 4)]), 0);
        // SMLAL : e0e10392
        check("SMLAL", 0xe0e1_0392, 4, false, r(&[(0, 0), (1, 0), (2, 0xffff_fffe), (3, 1)]), 0);
    }

    #[test]
    fn arm_clz() {
        // CLZ r0, r1 : e16f0f11
        check("CLZ 1", 0xe16f_0f11, 4, false, r(&[(1, 1)]), 0);
        check("CLZ 0", 0xe16f_0f11, 4, false, r(&[(1, 0)]), 0);
        check("CLZ top", 0xe16f_0f11, 4, false, r(&[(1, 0x8000_0000)]), 0);
    }

    // ---------------- Branches ----------------
    #[test]
    fn arm_branch() {
        // B +8 : imm24 = 0 → target = PC+8. ea000000
        check("B 0", 0xea00_0000, 4, false, r(&[]), 0);
        // B back: imm24 = 0xfffffe (-2) → target = PC+8 -8 = PC. eafffffe
        check("B -8", 0xeaff_fffe, 4, false, r(&[]), 0);
        // BL +8 : eb000000 (lr = pc+4)
        check("BL", 0xeb00_0000, 4, false, r(&[]), 0);
    }

    #[test]
    fn arm_bx_blx_reg() {
        // BX r1 (to ARM addr) : e12fff11
        check("BX arm", 0xe12f_ff11, 4, false, r(&[(1, 0x0002_0000)]), 0);
        // BX r1 (to Thumb, bit0=1) : switches T bit
        check("BX thumb", 0xe12f_ff11, 4, false, r(&[(1, 0x0002_0001)]), 0);
        // BLX r1 : e12fff31
        check("BLX reg", 0xe12f_ff31, 4, false, r(&[(1, 0x0002_0001)]), 0);
    }

    // ---------------- Conditional execution ----------------
    #[test]
    fn arm_cond_codes() {
        // ADDEQ r0, r1, #1 with Z=1 (executes) : 0281 0001
        check("ADDEQ taken", 0x0281_0001, 4, false, r(&[(1, 5)]), 0x4000_0000);
        // ADDEQ with Z=0 (skipped)
        check("ADDEQ skip", 0x0281_0001, 4, false, r(&[(1, 5)]), 0);
        // ADDNE taken (Z=0)
        check("ADDNE taken", 0x1281_0001, 4, false, r(&[(1, 5)]), 0);
        // MOVMI r0,#1 with N=1
        check("MOVMI taken", 0x43a0_0001, 4, false, r(&[]), 0x8000_0000);
        // BGT skip vs taken
        check("BGT skip", 0xca00_0000, 4, false, r(&[]), 0x4000_0000);
    }

    // ---------------- Loads / stores ----------------
    #[test]
    fn arm_ldr_str_imm() {
        // STR r0, [r1] : e5810000
        check_mem(
            "STR",
            0xe581_0000,
            4,
            false,
            r(&[(0, 0xdead_beef), (1, STACK_TOP - 32)]),
            0,
            &[],
        );
        // LDR r0, [r1] : e5910000
        check_mem(
            "LDR",
            0xe591_0000,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x1234_5678)],
        );
        // LDR r0, [r1, #4]! (pre, wback) : e5b10004
        check_mem(
            "LDR pre wb",
            0xe5b1_0004,
            4,
            false,
            r(&[(1, STACK_TOP - 36)]),
            0,
            &[(STACK_TOP - 32, 0xaabb_ccdd)],
        );
        // LDR r0, [r1], #4 (post, wback) : e4910004
        check_mem(
            "LDR post",
            0xe491_0004,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x0f0f_0f0f)],
        );
        // LDR r0, [r1, -#4] (down) : e5110004
        check_mem(
            "LDR down",
            0xe511_0004,
            4,
            false,
            r(&[(1, STACK_TOP - 28)]),
            0,
            &[(STACK_TOP - 32, 0x5555_5555)],
        );
    }

    #[test]
    fn arm_ldrb_strb() {
        // STRB r0, [r1] : e5c10000
        check_mem(
            "STRB",
            0xe5c1_0000,
            4,
            false,
            r(&[(0, 0x0000_00ab), (1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0xffff_ffff)],
        );
        // LDRB r0, [r1] : e5d10000
        check_mem(
            "LDRB",
            0xe5d1_0000,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x1234_56ab)],
        );
    }

    #[test]
    fn arm_ldr_str_reg() {
        // LDR r0, [r1, r2] : e7910002
        check_mem(
            "LDR reg",
            0xe791_0002,
            4,
            false,
            r(&[(1, STACK_TOP - 32), (2, 4)]),
            0,
            &[(STACK_TOP - 28, 0xcafe_babe)],
        );
        // LDR r0, [r1, r2, LSL #2] : e7910102
        check_mem(
            "LDR reg lsl",
            0xe791_0102,
            4,
            false,
            r(&[(1, STACK_TOP - 32), (2, 1)]),
            0,
            &[(STACK_TOP - 28, 0x1357_9bdf)],
        );
    }

    #[test]
    fn arm_extra_ldst() {
        // STRH r0, [r1] : e1c100b0
        check_mem(
            "STRH",
            0xe1c1_00b0,
            4,
            false,
            r(&[(0, 0x0000_abcd), (1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0xffff_ffff)],
        );
        // LDRH r0, [r1] : e1d100b0
        check_mem(
            "LDRH",
            0xe1d1_00b0,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x0000_89ab)],
        );
        // LDRSB r0, [r1] : e1d100d0
        check_mem(
            "LDRSB",
            0xe1d1_00d0,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x0000_00ff)],
        );
        // LDRSH r0, [r1] : e1d100f0
        check_mem(
            "LDRSH",
            0xe1d1_00f0,
            4,
            false,
            r(&[(1, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x0000_8001)],
        );
        // LDRH r0, [r1, #4]! : e1f100b4
        check_mem(
            "LDRH pre wb",
            0xe1f1_00b4,
            4,
            false,
            r(&[(1, STACK_TOP - 36)]),
            0,
            &[(STACK_TOP - 32, 0x0000_1122)],
        );
    }

    #[test]
    fn arm_ldrd_strd() {
        // LDRD r0, r1, [r2] : e1c200d0  (op=2,L=0 with bits... encoding: D0)
        check_mem(
            "LDRD",
            0xe1c2_00d0,
            4,
            false,
            r(&[(2, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x1111_1111), (STACK_TOP - 28, 0x2222_2222)],
        );
        // STRD r0, r1, [r2] : e1c200f0
        check_mem(
            "STRD",
            0xe1c2_00f0,
            4,
            false,
            r(&[(0, 0xaaaa_aaaa), (1, 0xbbbb_bbbb), (2, STACK_TOP - 32)]),
            0,
            &[],
        );
    }

    // ---------------- LDM / STM ----------------
    #[test]
    fn arm_ldm_stm() {
        // STMIA r13!, {r0,r1,r2} : e8ad0007  (sp decremented? no - IA increments)
        // Use a fixed base below stack top.
        // STMIA r4!, {r0,r1} : e8a40003
        check_mem(
            "STMIA wb",
            0xe8a4_0003,
            4,
            false,
            r(&[(0, 0xa1a1_a1a1), (1, 0xb2b2_b2b2), (4, STACK_TOP - 32)]),
            0,
            &[],
        );
        // LDMIA r4!, {r0,r1} : e8b40003
        check_mem(
            "LDMIA wb",
            0xe8b4_0003,
            4,
            false,
            r(&[(4, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x1111_2222), (STACK_TOP - 28, 0x3333_4444)],
        );
        // STMDB r13!, {r0,r1,r2} : e92d0007 (push)
        check_mem(
            "STMDB push",
            0xe92d_0007,
            4,
            false,
            r(&[(0, 1), (1, 2), (2, 3)]),
            0,
            &[],
        );
        // LDMDB r4!, {r0,r1} : e9340003
        check_mem(
            "LDMDB wb",
            0xe934_0003,
            4,
            false,
            r(&[(4, STACK_TOP - 24)]),
            0,
            &[(STACK_TOP - 32, 0x5555_5555), (STACK_TOP - 28, 0x6666_6666)],
        );
        // LDMIB r4!, {r0,r1} : e9b40003
        check_mem(
            "LDMIB wb",
            0xe9b4_0003,
            4,
            false,
            r(&[(4, STACK_TOP - 36)]),
            0,
            &[(STACK_TOP - 32, 0x7777_7777), (STACK_TOP - 28, 0x8888_8888)],
        );
        // LDMDA r4!, {r0,r1} : e8340003
        check_mem(
            "LDMDA wb",
            0xe834_0003,
            4,
            false,
            r(&[(4, STACK_TOP - 28)]),
            0,
            &[(STACK_TOP - 32, 0x9999_9999), (STACK_TOP - 28, 0xaaaa_aaaa)],
        );
        // LDMIA with PC : LDMIA r4, {r0, pc} : e8948001
        check_mem(
            "LDM pc",
            0xe894_8001,
            4,
            false,
            r(&[(4, STACK_TOP - 32)]),
            0,
            &[(STACK_TOP - 32, 0x1212_1212), (STACK_TOP - 28, 0x0002_0000)],
        );
    }

    // ---------------- Extends / bitfield / rev ----------------
    #[test]
    fn arm_extends() {
        // UXTB r0, r1 : e6ef0071
        check("UXTB", 0xe6ef_0071, 4, false, r(&[(1, 0x1234_56ff)]), 0);
        // SXTB r0, r1 : e6af0071
        check("SXTB", 0xe6af_0071, 4, false, r(&[(1, 0x0000_00ff)]), 0);
        // UXTH r0, r1 : e6ff0071
        check("UXTH", 0xe6ff_0071, 4, false, r(&[(1, 0x1234_89ab)]), 0);
        // SXTH r0, r1 : e6bf0071
        check("SXTH", 0xe6bf_0071, 4, false, r(&[(1, 0x0000_8001)]), 0);
        // UXTB r0, r1, ROR #8 : e6ef0471
        check("UXTB ror8", 0xe6ef_0471, 4, false, r(&[(1, 0x1234_56ff)]), 0);
    }

    #[test]
    fn arm_bitfield() {
        // UBFX r0, r1, #4, #8 : lsb=4,width=8 → widthm1=7 → e7e74251
        check("UBFX", 0xe7e7_4251, 4, false, r(&[(1, 0x1234_5678)]), 0);
        // SBFX r0, r1, #4, #8 : e7a74251
        check("SBFX", 0xe7a7_4251, 4, false, r(&[(1, 0x1234_5f78)]), 0);
        // BFI r0, r1, #4, #8 : msb=lsb+width-1=11 → e7cb4291
        check("BFI", 0xe7cb_4291, 4, false, r(&[(0, 0xffff_ffff), (1, 0x0000_00ab)]), 0);
        // BFC r0, #4, #8 : Rn=1111 → e7cb421f
        check("BFC", 0xe7cb_421f, 4, false, r(&[(0, 0xffff_ffff)]), 0);
    }

    #[test]
    fn arm_rev() {
        // REV r0, r1 : e6bf0f31
        check("REV", 0xe6bf_0f31, 4, false, r(&[(1, 0x1234_5678)]), 0);
        // REV16 r0, r1 : e6bf0fb1
        check("REV16", 0xe6bf_0fb1, 4, false, r(&[(1, 0x1234_5678)]), 0);
    }
}
