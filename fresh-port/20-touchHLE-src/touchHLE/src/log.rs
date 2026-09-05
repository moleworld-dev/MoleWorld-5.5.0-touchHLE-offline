/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Logging and terminal output macros.

use std::fs::File;
use std::sync::LazyLock;

/// Get a handle to the log file. This is only for use by logging macros!
///
/// All the logging macros print to stderr or (on Android) logcat, but this
/// is not convenient for users who aren't accustomed to command-line tools or
/// who don't have access to ADB, so we also write to a log file.
pub fn get_log_file() -> &'static File {
    static LOG_FILE: LazyLock<File> = LazyLock::new(|| {
        File::create(crate::paths::user_data_base_path().join("touchHLE_log.txt")).unwrap()
    });

    &LOG_FILE
}

/// Prints a log message unconditionally. Use this for errors or warnings.
///
/// The message is prefixed with the module path, so it is clear where it comes
/// from.
macro_rules! log {
    ($($arg:tt)+) => {
        echo!("{}: {}", module_path!(), format_args!($($arg)+));
    }
}

/// Same as [log], but silently fails on panic instead of
/// panicking.
macro_rules! log_no_panic {
    ($($arg:tt)+) => {
        echo_no_panic!("{}: {}", module_path!(), format_args!($($arg)+));
    }
}

/// Like [log], but prints the message only if debugging is enabled for the
/// module where it is used. This can be used for verbose things only needed
/// when debugging.
macro_rules! log_dbg {
    ($($arg:tt)+) => {
        if $crate::log::ENABLED_MODULES.contains(&module_path!()) {
            log!($($arg)*);
        }
    }
}

/// Like [log], but messages only log once and cannot have formatting.
/// To be used for log messages that are known to spam the log file (like those
/// logged every frame).
macro_rules! log_once {
    ($msg:literal) => {{
        static LOG_ONCE: std::sync::Once = std::sync::Once::new();
        LOG_ONCE.call_once(|| {
            log!("{} [this log will only be shown once]", $msg);
        });
    }};
}

/// Print a message (with implicit newline). This should be used for all
/// touchHLE output that isn't coming from the app itself.
///
/// Prefer use [log] or [log_dbg] for errors and warnings during emulation.
/// [MoleWorld iOS · 性能] 是否逐行 fsync 日志。默认 **否**(每行 fsync 在真机上 0.1~2ms,
/// 日志一多就成为可观的 CPU/IO 开销)。设 `MOLE_LOG_SYNC=1` 可恢复逐行落盘,用于抓硬崩现场。
pub fn log_sync_enabled() -> bool {
    use std::sync::OnceLock;
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("MOLE_LOG_SYNC").is_some())
}

macro_rules! echo {
    ($($arg:tt)+) => {
        {
            let formatted_str = format!($($arg)+);

            // [MoleWorld iOS] iOS 也走 SDL_Log → NSLog → 统一日志(Console.app 可见,
            // 真机调试用);eprintln! 到 stderr 仍保留(无害冗余)。
            #[cfg(any(target_os = "android", target_os = "ios"))]
            {
                sdl2::log::log(&formatted_str);
            }
            #[cfg(not(target_os = "android"))]
            eprintln!("{}", formatted_str);

            use std::io::Write;
            let mut log_file = $crate::log::get_log_file();
            let _ = log_file.write_all(formatted_str.as_bytes());
            let _ = log_file.write_all(b"\n");
            // [MoleWorld iOS · 性能] 原本【每行都 sync_data()】(真机 NVMe 一次 fsync 0.1~2ms)。
            // 日志量一大就直接吃掉可观 CPU/IO。默认改为不逐行落盘(write_all 已进内核页缓存,
            // 进程崩溃也不会丢——只有内核崩溃才会),需要抓硬崩现场时用 MOLE_LOG_SYNC=1 恢复逐行同步。
            if $crate::log::log_sync_enabled() {
                let _ = log_file.sync_data();
            }
        }
    };
    () => {
        {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            {
                sdl2::log::log("");
            }
            #[cfg(not(target_os = "android"))]
            eprintln!("");

            use std::io::Write;
            let mut log_file = $crate::log::get_log_file();
            let _ = log_file.write_all(b"\n");
            if $crate::log::log_sync_enabled() {
                let _ = log_file.sync_data();
            }
        }
    }
}

/// Same as [echo], but silently fails on panic instead of
/// panicking.
macro_rules! echo_no_panic {
    ($($arg:tt)*) => {
        {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                echo!($($arg)*);
            }));
        }
    }
}

/// Put modules to enable [log_dbg] for here, e.g. "touchHLE::mem" to see when
/// memory is allocated and freed.
pub const ENABLED_MODULES: &[&str] = &[];
