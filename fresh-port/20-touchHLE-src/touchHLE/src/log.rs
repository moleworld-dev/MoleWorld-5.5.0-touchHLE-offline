/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Logging and terminal output macros.

use std::fs::File;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};

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
///
/// [2026-09-16] A1-03 除了编译期常量表 `ENABLED_MODULES`,还能在运行时打开:环境变量
/// `TOUCHHLE_LOG_MODULES` 或选项 `--log-modules=`(见 `init_dbg_modules`)。objc_msgSend 热路径上
/// 每条消息都会走到这里,所以运行时表没开时只多读一次原子量 `DBG_ANY` 就短路,不做字符串比较。
macro_rules! log_dbg {
    ($($arg:tt)+) => {
        if $crate::log::ENABLED_MODULES.contains(&module_path!())
            || ($crate::log::DBG_ANY.load(::std::sync::atomic::Ordering::Relaxed)
                && $crate::log::dbg_enabled(module_path!()))
        {
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

/// [MoleWorld iOS · 性能] 是否逐行 fsync 日志。默认 **否**(每行 fsync 在真机上 0.1~2ms,
/// 日志一多就成为可观的 CPU/IO 开销)。设 `MOLE_LOG_SYNC=1` 可恢复逐行落盘,用于抓硬崩现场。
pub fn log_sync_enabled() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("MOLE_LOG_SYNC").is_some())
}

/// Print a message (with implicit newline). This should be used for all
/// touchHLE output that isn't coming from the app itself.
///
/// Prefer use [log] or [log_dbg] for errors and warnings during emulation.
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
            // [MoleWorld P0-C] 不再每行 fsync(sync_data)。write_all 已落到 OS 页缓存,进程崩溃
            // (panic/段错误)不会丢日志——内核仍会把页缓存写回磁盘;fsync 只防断电/内核崩,对调试
            // 日志没必要。而每行 fsync 在场景切换/进村时是毫秒级主线程 stall =「切场景卡一下」的真凶。
            // [iOS⇄main 合并 2026-09-24] iOS 分支(158df05)独立做了同一件事,另留了可选开关:
            // 需要抓硬崩(断电/内核崩)现场时设 MOLE_LOG_SYNC=1 恢复逐行 sync_data(),默认关。
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

/// [2026-09-16] A1-03 运行时 log_dbg! 总开关:只有 `init_dbg_modules` 装进了非空模块表才置真。
/// 单独留一个原子量,是为了让默认关闭时的判断只读这一个值就短路,不去碰 OnceLock 和字符串表。
pub static DBG_ANY: AtomicBool = AtomicBool::new(false);

/// [2026-09-16] A1-03 运行时打开 log_dbg! 的模块前缀表(如 `touchHLE::mole_cheats`)。
/// 启动时由 `init_dbg_modules` 写入一次,之后只读。
pub static DBG_MODULES: OnceLock<Vec<String>> = OnceLock::new();

/// [2026-09-16] A1-03 模块是否在运行时模块表里。按前缀匹配,这样 `touchHLE::mole_` 能一次打开所有
/// mole_* 模块。log_dbg! 只在 `DBG_ANY` 为真时才调用它。
pub fn dbg_enabled(module: &str) -> bool {
    DBG_MODULES
        .get()
        .is_some_and(|list| list.iter().any(|prefix| module.starts_with(prefix.as_str())))
}

/// [2026-09-16] A1-03 初始化运行时 log_dbg! 模块表,整个进程只调用一次(lib.rs 在全部选项应用完之后)。
/// 两个来源合并生效,都是逗号分隔的模块路径前缀:
/// - 环境变量 `TOUCHHLE_LOG_MODULES`:桌面上临时排查最方便;
/// - 选项 `--log-modules=`:能写进 touchHLE_options.txt,安卓 / iOS 设不了环境变量时靠它。
///
/// ★不要对 `touchHLE::objc::messages` 整模块打开(或 `touchHLE::objc` 这类覆盖它的前缀):
/// 每条 Objective-C 消息都会打一行,日志暴涨、帧率骤降。
pub fn init_dbg_modules(from_options: &[String]) {
    let from_env = std::env::var("TOUCHHLE_LOG_MODULES").unwrap_or_default();
    let mut list: Vec<String> = from_env
        .split(',')
        .chain(from_options.iter().map(String::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    list.sort();
    list.dedup();
    if list.is_empty() {
        return;
    }
    log!("运行时打开 log_dbg! 的模块前缀:{}", list.join(", "));
    if DBG_MODULES.set(list).is_ok() {
        DBG_ANY.store(true, Ordering::Relaxed);
    }
}
