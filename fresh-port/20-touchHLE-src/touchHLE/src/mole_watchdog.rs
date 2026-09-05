/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [MoleWorld iOS · 冻结转储器 · 宿主线程层]
//!
//! 背景:黄金岛"彻底卡死"时,仓库原有的 guest 看门狗(`mole_cheats::watchdog_check`,在 guest 执行批次
//! 的 yield 点运行)**一个字都没打** —— 说明主线程压根没回到 guest 执行:要么调度器在"所有线程阻塞"
//! 的分支里睡(协作式死锁,见 environment.rs 的 `dump_stall_state`),要么主线程卡在某个**阻塞的 host
//! 调用 / host 死循环**里——那种情况下调度器也不转,只有一个独立的 OS 线程能发现并取证。
//!
//! 做法:调度器每次迭代 `note_host_progress()`(一次原子存);本模块起一个真正的 OS 线程每 2 秒看一眼,
//! 前台状态下超过 8 秒没进展 → 向主线程发 `SIGUSR1`,信号处理函数用 `backtrace()` 把**宿主调用栈**
//! 写进日志(附 dyld slide,用 `atos -o touchHLE -arch arm64 -s <slide> <addr>` 符号化)。
//! 稳态成本:每次调度器迭代一个原子存 + 一个每 2 秒醒一次的线程。

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering::Relaxed};

pub static HOST_PROGRESS_MS: AtomicU64 = AtomicU64::new(0);
/// 由 window.set_backgrounded 维护;后台时调度器本就暂停,不算卡死。
pub static BACKGROUNDED: AtomicBool = AtomicBool::new(false);
static LOG_FD: AtomicI32 = AtomicI32::new(-1);
static MAIN_PTHREAD: AtomicUsize = AtomicUsize::new(0);
/// 采样剖析进行中:信号处理函数写紧凑的 [PROF] 样本而不是 [HOSTSTALL] 块。
static PROFILING: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[inline]
pub fn note_host_progress() {
    HOST_PROGRESS_MS.store(now_ms(), Relaxed);
}

#[cfg(unix)]
extern "C" {
    fn backtrace(buf: *mut *mut libc::c_void, size: libc::c_int) -> libc::c_int;
    fn backtrace_symbols_fd(buf: *const *mut libc::c_void, size: libc::c_int, fd: libc::c_int);
    fn _dyld_get_image_vmaddr_slide(image_index: u32) -> isize;
}

#[cfg(unix)]
extern "C" fn on_sigusr1(_sig: libc::c_int) {
    // 信号处理函数:只用 async-signal-safe 的原语(write/backtrace_symbols_fd)。
    let fd = LOG_FD.load(Relaxed);
    if fd < 0 {
        return;
    }
    let mut buf = [std::ptr::null_mut::<libc::c_void>(); 96];
    let n = unsafe { backtrace(buf.as_mut_ptr(), buf.len() as libc::c_int) };
    if PROFILING.load(Relaxed) {
        // 采样模式:一个样本 = "[PROF]" 行 + 最多 32 帧(backtrace_symbols_fd 逐帧一行)。
        let hdr = b"[PROF]\n";
        unsafe {
            libc::write(fd, hdr.as_ptr().cast(), hdr.len());
            backtrace_symbols_fd(buf.as_ptr(), n.min(32), fd);
        }
        return;
    }
    let hdr = b"[HOSTSTALL] ---- main-thread host backtrace (SIGUSR1) ----\n";
    unsafe {
        libc::write(fd, hdr.as_ptr().cast(), hdr.len());
        backtrace_symbols_fd(buf.as_ptr(), n, fd);
        let tail = b"[HOSTSTALL] ---- end backtrace ----\n";
        libc::write(fd, tail.as_ptr().cast(), tail.len());
    }
}

/// 在主线程调用一次(Environment::run 入口)。`log_fd` = 日志文件的原始 fd(dup 一份专供信号处理函数)。
#[cfg(unix)]
pub fn start(log_fd: i32) {
    let fd = unsafe { libc::dup(log_fd) };
    LOG_FD.store(fd, Relaxed);
    MAIN_PTHREAD.store(unsafe { libc::pthread_self() } as usize, Relaxed);
    unsafe {
        libc::signal(libc::SIGUSR1, on_sigusr1 as usize as libc::sighandler_t);
    }
    note_host_progress();
    let slide = unsafe { _dyld_get_image_vmaddr_slide(0) };
    log!(
        "[HOSTSTALL] 宿主卡死看门狗已启动(前台 >8s 调度器无进展 → SIGUSR1 取主线程回溯);dyld slide={:#x}",
        slide
    );
    std::thread::Builder::new()
        .name("mole-host-watchdog".into())
        .spawn(move || {
            let mut last_dump: u64 = 0;
            // [按需采样剖析器] 往应用容器 Documents/ 放一个名为 mole_profile_now 的文件
            // (`xcrun devicectl device copy to ...`)→ 这里 2s 内发现,删掉它,然后对主线程以 100Hz 发
            // SIGUSR1 采样 5 秒,信号处理函数把每个样本的宿主回溯写进日志;用 dev-scripts/mw-symbolicate.py
            // 聚合出热点。稳态零成本(只是每 2s 一次 stat)。
            let arm_path = std::env::var("HOME")
                .map(|h| std::path::PathBuf::from(h).join("Documents").join("mole_profile_now"))
                .ok();
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                if let Some(p) = arm_path.as_ref() {
                    if p.exists() {
                        let _ = std::fs::remove_file(p);
                        log!("[PROFILE] 开始采样主线程:5s @ 100Hz(SIGUSR1)");
                        let main = MAIN_PTHREAD.load(Relaxed) as libc::pthread_t;
                        PROFILING.store(true, Relaxed);
                        for _ in 0..500 {
                            unsafe {
                                libc::pthread_kill(main, libc::SIGUSR1);
                            }
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        PROFILING.store(false, Relaxed);
                        log!("[PROFILE] 采样结束(500 样本)");
                        continue;
                    }
                }
                if BACKGROUNDED.load(Relaxed) {
                    continue;
                }
                let last = HOST_PROGRESS_MS.load(Relaxed);
                let now = now_ms();
                if last == 0 || now.saturating_sub(last) < 8_000 {
                    continue;
                }
                if now.saturating_sub(last_dump) < 10_000 {
                    continue;
                }
                last_dump = now;
                log!(
                    "[HOSTSTALL] 宿主调度器已 {:.1}s 无进展(前台)→ 主线程可能卡在阻塞的 host 调用/host 死循环;发 SIGUSR1 取回溯",
                    (now - last) as f64 / 1000.0
                );
                let main = MAIN_PTHREAD.load(Relaxed) as libc::pthread_t;
                unsafe {
                    libc::pthread_kill(main, libc::SIGUSR1);
                }
            }
        })
        .ok();
}
#[cfg(not(unix))]
pub fn start(_log_fd: i32) {}
