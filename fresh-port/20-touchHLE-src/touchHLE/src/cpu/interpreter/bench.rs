/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CPU-bound throughput micro-bench for the LIVE interpreter (test-only).
//!
//! Boot MIPS is diluted by native texture decompression + ObjC `msgSend` HLE,
//! so it can't measure the interpreter itself. This runs hand-assembled hot
//! loops straight through `InterpreterCpu` with NO HLE/native work, timing pure
//! interpreter dispatch — the right benchmark for any per-instruction or
//! decode-cache optimization. Native MIPS is a faithful proxy for the wasm
//! speedup ratio (Gate 0 measured wasm only ~1.1x slower than native).
//!
//! Run (release, or the numbers are meaningless):
//!   cargo test --release --no-default-features --features cpu_interpreter \
//!       cpu::interpreter::bench -- --nocapture
#![cfg(test)]

use super::InterpreterCpu;
use crate::cpu::CpuState;
use crate::mem::{Mem, MutPtr, Ptr};
use std::time::Instant;

const CODE_BASE: u32 = 0x0001_0000;
const STACK_TOP: u32 = 0x0010_0000;
const DATA_BASE: u32 = 0x0020_0000;
const CPSR_USER_MODE: u32 = 0x0000_0010;
const CPSR_THUMB: u32 = 0x0000_0020;

/// Pure ALU + branch (dispatch upper bound, no memory): 3 insns/iter.
///   loop: ADD r0,r0,r1 ; SUBS r1,r1,#1 ; BNE loop
#[rustfmt::skip]
const ALU_LOOP: &[u8] = &[
    0x08, 0x18, // ADD  r0, r0, r1
    0x01, 0x39, // SUBS r1, r1, #1
    0xFC, 0xD1, // BNE  -8
];

/// Load-heavy (1 of 4 insns is a memory read): closer to real code.
///   loop: LDR r3,[r2] ; ADD r0,r0,r3 ; SUBS r1,r1,#1 ; BNE loop
#[rustfmt::skip]
const LDR_LOOP: &[u8] = &[
    0x13, 0x68, // LDR  r3, [r2, #0]
    0xC0, 0x18, // ADD  r0, r0, r3
    0x01, 0x39, // SUBS r1, r1, #1
    0xFB, 0xD1, // BNE  -10
];

/// Mixed load+store+ALU+branch (5 insns/iter), the most code-like:
///   loop: LDR r3,[r2] ; ADD r3,r3,r1 ; STR r3,[r2] ; SUBS r1,r1,#1 ; BNE loop
#[rustfmt::skip]
const MIXED_LOOP: &[u8] = &[
    0x13, 0x68, // LDR  r3, [r2, #0]
    0x4B, 0x18, // ADD  r3, r1, r3   (0x184b)
    0x13, 0x60, // STR  r3, [r2, #0] (0x6013)
    0x01, 0x39, // SUBS r1, r1, #1
    0xF9, 0xD1, // BNE  -14
];

fn run_blob_mips(name: &str, blob: &[u8], budget: u64) -> f64 {
    let mut mem = Mem::new();
    let code: MutPtr<u8> = Ptr::from_bits(CODE_BASE);
    mem.bytes_at_mut(code, blob.len() as u32).copy_from_slice(blob);
    let data: MutPtr<u8> = Ptr::from_bits(DATA_BASE);
    mem.bytes_at_mut(data, 4).copy_from_slice(&0x1234_5678u32.to_le_bytes());

    let mut cpu = InterpreterCpu::new(0);
    let regs = cpu.regs_mut();
    regs[0] = 0;
    regs[1] = 0xFFFF_FFFF; // big enough that BNE never falls through within budget
    regs[2] = DATA_BASE;
    regs[13] = STACK_TOP;
    regs[15] = CODE_BASE;
    cpu.set_cpsr(CPSR_USER_MODE | CPSR_THUMB);

    let mut ticks = budget;
    let t0 = Instant::now();
    let st = cpu.run_or_step(&mut mem, Some(&mut ticks));
    let dt = t0.elapsed().as_secs_f64();
    // The loop must NOT halt early (no SVC/error) — that would mean we mis-decoded.
    assert!(
        matches!(st, CpuState::Normal),
        "{name}: halted early ({st:?}) — loop mis-decoded?"
    );
    let executed = budget - ticks;
    let mips = executed as f64 / dt / 1.0e6;
    println!("[BENCH] {name:<12} {mips:7.1} MIPS  ({executed} insns / {dt:.3}s)");
    mips
}

#[test]
fn interp_throughput() {
    // 200M-instruction budget each: long enough to dwarf startup, short enough
    // to finish in a couple seconds at ~100 MIPS.
    const N: u64 = 200_000_000;
    println!("\n=== LIVE interpreter CPU-bound throughput (native; wasm ~= /1.1) ===");
    run_blob_mips("alu", ALU_LOOP, N);
    run_blob_mips("ldr", LDR_LOOP, N);
    run_blob_mips("mixed", MIXED_LOOP, N);
    println!("===================================================================\n");
}
