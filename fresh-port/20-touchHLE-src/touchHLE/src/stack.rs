/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Utilities related to the stack.

use crate::cpu::Cpu;
use crate::mem::{GuestUSize, Mem, MutPtr, Ptr};

/// Set up the stack for the main thread, ready to execute the entry-point
/// of the application (aka `start`).
///
/// The entry-point of an iPhone OS application expects certain data to have
/// been placed on the stack, which it will then pass as arguments to the C
/// `main` function. On iPhone OS, `main` can have up to four arguments:
///
/// ```c
/// int main(int argc, char *argv[], char *envp[], char *apple[]);
/// ```
pub fn prep_stack_for_start(
    mem: &mut Mem,
    cpu: &mut Cpu,
    argv: &[&str],
    envp: &[&str],
    apple: &[&str],
) {
    let argc: i32 = argv.len().try_into().unwrap();

    // We are arbitrarily putting the main thread's stack at the top of the
    // address space (see also: mem::Mem::MAIN_THREAD_STACK_LOW_END).
    // Since the stack grows downwards, its first byte would be 0xffffffff.
    #[cfg(not(target_arch = "wasm32"))]
    let stack_base: usize = 1 << 32;
    // [wasm M1] mem 手术:guest 地址空间收缩到 1GiB,主栈重定位到 1GiB 窗口顶端。
    // stack_base = 1<<30(exclusive 顶,首字节 0x3FFFFFFF),与 mem::MAIN_THREAD_STACK_LOW_END
    // = 0x3FF00000 一致,栈区 [0x3FF00000, 0x40000000) 全在 [u8; 1<<30] 数组内。
    #[cfg(target_arch = "wasm32")]
    let stack_base: usize = 1 << 30;

    // Rust vectors grow upwards but we need to grow this one downwards, so
    // let's push the strings onto it reversed.
    let mut reversed_data = Vec::<u8>::new();
    let mut string_ptrs = Vec::<u32>::new();

    fn push_string(
        string: &str,
        reversed_data: &mut Vec<u8>,
        string_ptrs: &mut Vec<u32>,
        stack_base: usize,
    ) {
        reversed_data.reserve(string.len() + 1);
        reversed_data.push(b'\0'); // null terminator
        for &c in string.as_bytes().iter().rev() {
            reversed_data.push(c);
        }

        let ptr: u32 = (stack_base - reversed_data.len()).try_into().unwrap();
        string_ptrs.push(ptr);
    }

    string_ptrs.push(argc as u32);
    for arg in argv {
        push_string(arg, &mut reversed_data, &mut string_ptrs, stack_base);
    }

    for arg in envp {
        push_string(arg, &mut reversed_data, &mut string_ptrs, stack_base);
    }
    string_ptrs.push(0); // terminator

    for arg in apple {
        push_string(arg, &mut reversed_data, &mut string_ptrs, stack_base);
    }
    string_ptrs.push(0); // terminator

    // Pad to ensure stack is 4-byte aligned
    let misaligned_by = reversed_data.len() % 4;
    let pad_by = if misaligned_by != 0 {
        4 - misaligned_by
    } else {
        0
    };
    reversed_data.resize(reversed_data.len() + pad_by, 0);

    for ptr in string_ptrs.iter().rev() {
        reversed_data.extend_from_slice(ptr.to_be_bytes().as_slice());
    }

    let stack_ptr: MutPtr<u8> =
        Ptr::from_bits((stack_base - reversed_data.len()).try_into().unwrap());
    let stack_height: GuestUSize = reversed_data.len().try_into().unwrap();

    assert!(stack_height < Mem::MAIN_THREAD_STACK_SIZE);

    let stack_region = mem.bytes_at_mut(stack_ptr, stack_height);

    for i in 0..stack_height {
        stack_region[i as usize] = reversed_data[(stack_height - i - 1) as usize];
    }

    //println!(
    //  "{}",
    //  std::str::from_utf8(
    //      &stack_region
    //      .iter()
    //      .flat_map(|&c| std::ascii::escape_default(c))
    //      .collect::<Vec<u8>>())
    //  .unwrap()
    //);
    //println!(
    //    "{:?}",
    //    mem.cstr_at_utf8(MutPtr::from_bits(mem.read((stack_ptr + 4).cast())))
    //);

    assert!(stack_height.is_multiple_of(4)); // ensure padding worked properly

    cpu.regs_mut()[Cpu::SP] = stack_ptr.to_bits();
}
