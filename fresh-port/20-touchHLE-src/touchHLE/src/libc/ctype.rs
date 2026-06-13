/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `ctype.h`

use super::wchar::wchar_t;
use crate::abi::GuestFunction;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::mem::{ConstVoidPtr, MutVoidPtr, Ptr, SafeRead};
use crate::Environment;

/// Called by inlined `tolower()` on Darwin
fn __tolower(_env: &mut Environment, c: i32) -> i32 {
    if (c as u8) as i32 == c {
        (c as u8).to_ascii_lowercase().into()
    } else {
        c
    }
}
/// Called by inlined `toupper()` on Darwin
fn __toupper(_env: &mut Environment, c: i32) -> i32 {
    if (c as u8) as i32 == c {
        (c as u8).to_ascii_uppercase().into()
    } else {
        c
    }
}

fn __maskrune(env: &mut Environment, rune: i32, mask: u32) -> i32 {
    // TODO: do not re-create rune table on each call
    let default_rune_locale_ptr = get_default_rune_locale(env);
    let rune_locale: RuneLocale = env.mem.read(default_rune_locale_ptr.cast());
    env.mem.free(default_rune_locale_ptr.cast_mut());
    (rune_locale.runetype[(rune & 0xFF) as usize] & mask) as i32
}

#[allow(non_camel_case_types)]
type darwin_rune_t = wchar_t;

const LOOKUP_TABLE_SIZE: usize = 1 << 8;

/// Darwin inlines its implementation of the ctype functions and so this struct
/// is part of its ABI. The names have had their leading underscores removed.
#[repr(C, packed)]
struct RuneLocale {
    magic: [u8; 8],
    /// Fixed-width string naming the encoding
    encoding: [u8; 32],

    getrune: GuestFunction, // TODO
    putrune: GuestFunction, // TODO
    invalid_rune: darwin_rune_t,

    /// Bits represent type of character
    runetype: [u32; LOOKUP_TABLE_SIZE],
    map_lower: [darwin_rune_t; LOOKUP_TABLE_SIZE],
    map_upper: [darwin_rune_t; LOOKUP_TABLE_SIZE],

    variable: MutVoidPtr, // extra data, not used
    variable_len: i32,

    ncharclasses: i32,     // extra data, not used
    charclass: MutVoidPtr, // type should be pointer to RuneCharClass
}
unsafe impl SafeRead for RuneLocale {}

fn get_default_rune_locale(env: &mut Environment) -> ConstVoidPtr {
    let mut runetype = [0u32; LOOKUP_TABLE_SIZE];
    let mut map_lower = [0 as darwin_rune_t; LOOKUP_TABLE_SIZE];
    let mut map_upper = [0 as darwin_rune_t; LOOKUP_TABLE_SIZE];

    for idx in 0..LOOKUP_TABLE_SIZE {
        let c: u8 = idx.try_into().unwrap();

        let as_lower = c.to_ascii_lowercase();
        let as_upper = c.to_ascii_lowercase();

        let mut as_runetype = 0u32;
        if c.is_ascii_alphabetic() {
            as_runetype |= 0x100;
        }
        if c.is_ascii_control() {
            as_runetype |= 0x200;
        }
        if c.is_ascii_digit() {
            as_runetype |= 0x400;
        }
        if c.is_ascii_graphic() {
            as_runetype |= 0x800;
        }
        if c.is_ascii_lowercase() {
            as_runetype |= 0x1000;
        }
        if c.is_ascii_punctuation() {
            as_runetype |= 0x2000;
        }
        // Rust's definition excludes vertical tab
        if c.is_ascii_whitespace() || c == b'\x0b' {
            as_runetype |= 0x4000;
        }
        if c.is_ascii_uppercase() {
            as_runetype |= 0x8000;
        }
        if c.is_ascii_hexdigit() {
            as_runetype |= 0x10000;
        }
        // isblank()
        if c == b' ' || c == b'\t' {
            as_runetype |= 0x20000;
        }
        // isprint()
        if c.is_ascii_graphic() || c == b' ' {
            as_runetype |= 0x40000;
        }
        // TODO: There are some other flags: "ideogram", "special", "phonogram",
        // and a character "width" between 0 and 4. These aren't standard C and
        // aren't implemented here.

        runetype[idx] = as_runetype;
        map_lower[idx] = as_lower.into();
        map_upper[idx] = as_upper.into();
    }

    let mut encoding = [0u8; 32];
    encoding[0..4].copy_from_slice(b"NONE"); // this is the real value!

    env.mem
        .alloc_and_write(RuneLocale {
            magic: *b"RuneMagA",
            encoding,

            getrune: GuestFunction::null_ptr(), // TODO
            putrune: GuestFunction::null_ptr(), // TODO
            invalid_rune: -1,                   // probably not correct

            runetype,
            map_lower,
            map_upper,

            variable: Ptr::null(),
            variable_len: 0,

            ncharclasses: 0,
            charclass: Ptr::null(),
        })
        .cast()
        .cast_const()
}

fn stack_chk_guard(env: &mut Environment) -> ConstVoidPtr {
    // The stack-protector canary value. -fstack-protector function prologues
    // load *(&__stack_chk_guard) into a local; epilogues compare it and call
    // __stack_chk_fail on mismatch. touchHLE otherwise leaves this non-lazy
    // symbol unbound (a pointer to address 0), so every guest function with a
    // stack buffer null-derefs. First hit for MoleWorld:
    // -[iRate localizedStringForKey:]. Any stable value works since we never
    // actually smash the guest stack; use a fixed nonzero sentinel.
    env.mem
        .alloc_and_write::<u32>(0xdead_2a55)
        .cast()
        .cast_const()
}

pub const CONSTANTS: ConstantExports = &[
    (
        "__DefaultRuneLocale",
        HostConstant::Custom(get_default_rune_locale),
    ),
    (
        "___stack_chk_guard",
        HostConstant::Custom(stack_chk_guard),
    ),
];

/// `__stack_chk_fail` — the -fstack-protector epilogue calls this when the
/// stored canary doesn't match `__stack_chk_guard`. In an emulator we never
/// actually smash the guest stack, so reaching here means EITHER a CPU-backend
/// bug (a store/compare divergence corrupting the canary slot) OR a genuine
/// guest issue. The function is `noreturn`, so returning leaves the caller in a
/// corrupt state (it falls through past the call). Log loudly + dump the
/// interpreter's recent-instruction trace so the divergent canary check is
/// visible, then continue (best-effort; matches the old no-op behaviour).
fn __stack_chk_fail(env: &mut Environment) {
    echo!(
        "[STACK-CHK-FAIL] canary mismatch! lr={:#010x} regs={:08x?}",
        env.cpu.regs()[14],
        env.cpu.regs()
    );
    env.cpu.dump_interp_trace();
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(__tolower(_)),
    export_c_func!(__toupper(_)),
    export_c_func!(__maskrune(_, _)),
    export_c_func!(__stack_chk_fail()),
];
