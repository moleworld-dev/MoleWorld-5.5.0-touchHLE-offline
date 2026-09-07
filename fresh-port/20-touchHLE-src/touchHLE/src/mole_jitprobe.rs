/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [MoleWorld iOS · JIT 探针] 这台真机到底能不能拿到可执行内存?
//!
//! 我们用纯解释器是因为「iOS 上 JIT 已死」这条结论;但那条结论的取证是 2026 年早些时候做的,
//! 而 LiveContainer/dynarmic 在 2026-08-29 加了一条专门针对 **iOS 26+ / TXM** 的双映射路径。
//! 与其继续引用旧结论,不如让设备自己回答。本探针把业界实际在用的四条路各试一遍并记录 errno:
//!   ① csops 读进程代码签名标志(CS_DEBUGGED 决定「调试器挂上了没有」,CS_GET_TASK_ALLOW 决定「能不能被挂」)
//!   ② mmap(RX) → mprotect(RW) 翻转        = oaknut CodeBlock 在 iOS 上走的路
//!   ③ mmap(RX) + vm_remap 出 RW 别名       = oaknut DualCodeBlock,LiveContainer 的 TXM 路径基座
//!   ④ mmap(MAP_JIT)                        = Apple 官方 JIT(要 allow-jit entitlement,iOS 上第三方拿不到)
//! ★默认只探测「能不能拿到内存」,不执行生成的指令(执行失败会被内核直接 SIGKILL,没法在进程内兜住)。
//! 容器里放 `Documents/mole_jit_exec` 才会真的写一条 `ret` 并跳进去——那才是「JIT 真的可用」的终判。

use std::sync::atomic::{AtomicBool, Ordering};

const PROT_NONE: i32 = 0;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const PROT_EXEC: i32 = 4;
const MAP_PRIVATE: i32 = 0x0002;
const MAP_ANON: i32 = 0x1000;
const MAP_JIT: i32 = 0x0800;
const VM_FLAGS_ANYWHERE: i32 = 0x0001;

// csops(2) 的 CS_OPS_STATUS
const CS_OPS_STATUS: u32 = 0;
const CS_VALID: u32 = 0x0000_0001;
const CS_GET_TASK_ALLOW: u32 = 0x0000_0004;
const CS_INSTALLER: u32 = 0x0000_0008;
const CS_HARD: u32 = 0x0000_0100;
const CS_KILL: u32 = 0x0000_0200;
const CS_RESTRICT: u32 = 0x0000_0800;
const CS_ENFORCEMENT: u32 = 0x0000_1000;
const CS_DYLD_PLATFORM: u32 = 0x0200_0000;
const CS_DEBUGGED: u32 = 0x1000_0000;
const CS_SIGNED: u32 = 0x2000_0000;

extern "C" {
    fn mmap(addr: *mut u8, len: usize, prot: i32, flags: i32, fd: i32, off: i64) -> *mut u8;
    fn munmap(addr: *mut u8, len: usize) -> i32;
    fn mprotect(addr: *mut u8, len: usize, prot: i32) -> i32;
    fn csops(pid: i32, ops: u32, useraddr: *mut u8, usersize: usize) -> i32;
    fn getpid() -> i32;
    fn mach_task_self() -> u32;
    fn mach_vm_remap(
        target_task: u32,
        target_address: *mut u64,
        size: u64,
        mask: u64,
        flags: i32,
        src_task: u32,
        src_address: u64,
        copy: i32,
        cur_protection: *mut i32,
        max_protection: *mut i32,
        inheritance: u32,
    ) -> i32;
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn decode_csflags(f: u32) -> String {
    let mut v = Vec::new();
    for (bit, name) in [
        (CS_VALID, "VALID"),
        (CS_GET_TASK_ALLOW, "GET_TASK_ALLOW"),
        (CS_INSTALLER, "INSTALLER"),
        (CS_HARD, "HARD"),
        (CS_KILL, "KILL"),
        (CS_RESTRICT, "RESTRICT"),
        (CS_ENFORCEMENT, "ENFORCEMENT"),
        (CS_DYLD_PLATFORM, "DYLD_PLATFORM"),
        (CS_DEBUGGED, "DEBUGGED"),
        (CS_SIGNED, "SIGNED"),
    ] {
        if f & bit != 0 {
            v.push(name);
        }
    }
    v.join("|")
}

/// 跑一次探测并把结果写进日志。多次调用只跑第一次。
pub fn probe() {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::Relaxed) {
        return;
    }
    const SZ: usize = 64 * 1024;

    // ① 代码签名标志
    let mut flags: u32 = 0;
    let rc = unsafe {
        csops(
            getpid(),
            CS_OPS_STATUS,
            (&mut flags as *mut u32).cast::<u8>(),
            4,
        )
    };
    log!(
        "[JITPROBE] csops rc={} flags={:#010x} [{}]  ← DEBUGGED=调试器已挂(JIT 前提);GET_TASK_ALLOW=允许被挂",
        rc,
        flags,
        decode_csflags(flags)
    );

    // ② mmap(RX) → mprotect(RW):oaknut CodeBlock 在 iOS 上的做法
    unsafe {
        let p = mmap(
            std::ptr::null_mut(),
            SZ,
            PROT_READ | PROT_EXEC,
            MAP_ANON | MAP_PRIVATE,
            -1,
            0,
        );
        if p as isize == -1 {
            log!("[JITPROBE] ② mmap(RX) 失败 errno={}", errno());
        } else {
            let r = mprotect(p, SZ, PROT_READ | PROT_WRITE);
            log!(
                "[JITPROBE] ② mmap(RX)@{:p} 成功;mprotect(RW) {} {}",
                p,
                if r == 0 { "成功" } else { "失败" },
                if r == 0 { String::new() } else { format!("errno={}", errno()) }
            );
            munmap(p, SZ);
        }
    }

    // ③ mmap(RX) + vm_remap 出可写别名:DualCodeBlock / LiveContainer TXM 路径的基座
    unsafe {
        let rx = mmap(
            std::ptr::null_mut(),
            SZ,
            PROT_READ | PROT_EXEC,
            MAP_ANON | MAP_PRIVATE,
            -1,
            0,
        );
        if rx as isize == -1 {
            log!("[JITPROBE] ③ mmap(RX) 失败 errno={}", errno());
        } else {
            let mut target: u64 = 0;
            let mut cur: i32 = 0;
            let mut max: i32 = 0;
            let kr = mach_vm_remap(
                mach_task_self(),
                &mut target,
                SZ as u64,
                0,
                VM_FLAGS_ANYWHERE,
                mach_task_self(),
                rx as u64,
                0, // copy = false → 共享同一物理页
                &mut cur,
                &mut max,
                1, // VM_INHERIT_COPY
            );
            if kr != 0 {
                log!("[JITPROBE] ③ vm_remap 失败 kr={} (RX@{:p})", kr, rx);
            } else {
                let r = mprotect(target as *mut u8, SZ, PROT_READ | PROT_WRITE);
                log!(
                    "[JITPROBE] ③ vm_remap 成功 别名@{:#x} cur_prot={:#x} max_prot={:#x};别名 mprotect(RW) {} {}",
                    target, cur, max,
                    if r == 0 { "成功" } else { "失败" },
                    if r == 0 { String::new() } else { format!("errno={}", errno()) }
                );
                if r == 0 {
                    // 别名可写 + 原映射可执行 = 双映射 JIT 成立的充分条件
                    std::ptr::write_volatile(target as *mut u32, 0xd65f_03c0); // ret
                    let back = std::ptr::read_volatile(rx as *const u32);
                    log!(
                        "[JITPROBE] ③ 经别名写入 ret,从 RX 侧读回 {:#010x} {}",
                        back,
                        if back == 0xd65f_03c0 { "← 双映射生效(物理页共享)" } else { "← ★没生效(不是同一物理页)" }
                    );
                }
                munmap(target as *mut u8, SZ);
            }
            munmap(rx, SZ);
        }
    }

    // ④ Apple 官方 MAP_JIT(iOS 上需要 allow-jit entitlement)
    unsafe {
        let p = mmap(
            std::ptr::null_mut(),
            SZ,
            PROT_READ | PROT_WRITE | PROT_EXEC,
            MAP_ANON | MAP_PRIVATE | MAP_JIT,
            -1,
            0,
        );
        if p as isize == -1 {
            log!("[JITPROBE] ④ mmap(MAP_JIT, RWX) 失败 errno={} (iOS 上第三方拿不到 allow-jit 就是这个结果)", errno());
        } else {
            log!("[JITPROBE] ④ mmap(MAP_JIT, RWX)@{:p} 成功 ★意外", p);
            munmap(p, SZ);
        }
    }

    // ⑤ 终判:真的跳进去执行。默认不做——失败是内核直接 SIGKILL,进程内兜不住。
    let trig = crate::paths::user_data_base_path().join("mole_jit_exec");
    if !trig.exists() {
        log!("[JITPROBE] ⑤ 执行终判已跳过(容器放 Documents/mole_jit_exec 才做;失败会被内核直接杀进程)");
        return;
    }
    let _ = std::fs::remove_file(&trig);
    log!("[JITPROBE] ⑤ 开始执行终判:写一条 ret 然后跳进去……(若日志到此为止 = 被内核杀了 = JIT 不可用)");
    { use std::io::Write; let mut f = crate::log::get_log_file(); let _ = f.flush(); let _ = f.sync_data(); }
    unsafe {
        let rx = mmap(
            std::ptr::null_mut(),
            SZ,
            PROT_READ | PROT_EXEC,
            MAP_ANON | MAP_PRIVATE,
            -1,
            0,
        );
        if rx as isize == -1 {
            log!("[JITPROBE] ⑤ mmap 失败,放弃");
            return;
        }
        let mut target: u64 = 0;
        let (mut cur, mut max) = (0i32, 0i32);
        let kr = mach_vm_remap(
            mach_task_self(), &mut target, SZ as u64, 0, VM_FLAGS_ANYWHERE,
            mach_task_self(), rx as u64, 0, &mut cur, &mut max, 1,
        );
        if kr != 0 || mprotect(target as *mut u8, SZ, PROT_READ | PROT_WRITE) != 0 {
            log!("[JITPROBE] ⑤ 双映射拿不到,放弃执行");
            munmap(rx, SZ);
            return;
        }
        std::ptr::write_volatile(target as *mut u32, 0xd65f_03c0); // ret
        let f: extern "C" fn() = std::mem::transmute(rx);
        f();
        log!("[JITPROBE] ⑤ ★★ 执行成功并返回 —— 这台机器上 JIT 是可用的");
        munmap(target as *mut u8, SZ);
        munmap(rx, SZ);
    }
}

#[allow(dead_code)]
fn unused(_: i32) {
    let _ = PROT_NONE;
}
