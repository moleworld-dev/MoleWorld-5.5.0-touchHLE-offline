/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! touchHLE is a high-level emulator (HLE) for iPhone OS applications.
//!
//! In various places, the terms "guest" and "host" are used to distinguish
//! between the emulated application (the "guest") and the emulator itself (the
//! "host"), and more generally, their different environments.
//! For example:
//! - The guest is a 32-bit application, so a "guest pointer" is 32 bits.
//! - The host is a 64-bit application, so a "host pointer" is 64 bits.
//! - The guest can only directly access "guest memory".
//! - The host can access both "guest memory" and "host memory".
//! - A "guest function" is emulated Arm code, usually from the app binary.
//! - A "host function" is a Rust function that is part of this emulator.

// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]
// The documentation for this crate is intended to include private items.
// rustdoc complains about some public macros that link to private items, but
// we're forced to make those macros public by the weird macro scoping rules,
// so this warning is unhelpful.
#![allow(rustdoc::private_intra_doc_links)]

#[macro_use]
mod log;
mod abi;
mod audio;
mod bundle;
mod cpu;
mod debug;
mod dyld;
mod environment;
mod font;
mod frameworks;
mod fs;
mod gdb;
mod gles;
mod image;
mod libc;
mod licenses;
mod mach_o;
mod matrix;
mod mem;
mod mole_cheats;
mod mole_diag;
mod mole_menu;
mod mole_sysinfo;
mod objc;
mod save_reset;
mod options;
mod paths;
mod stack;
mod window;

// Environment is used very frequently used and used to be in this module, so
// it is re-exported to avoid having to update lots of imports. The other things
// probably shouldn't be, but they need a new home (TODO).
// Unlike its siblings, this module should be considered private and only used
// via re-exports.
use environment::{Environment, MutexId, MutexType, ThreadId, PTHREAD_MUTEX_DEFAULT};

use std::path::PathBuf;

pub use touchHLE_version::*;

/// This is the true entry point on Android (SDLActivity calls it after
/// initialization). On other platforms the true entry point is in src/bin.rs.
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn SDL_main(
    _argc: std::ffi::c_int,
    _argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    // Rust's default panic handler prints to stderr, but on Android that just
    // gets discarded, so we set a custom hook to make debugging easier.
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s
        } else {
            "(non-string payload)"
        };
        if let Some(location) = info.location() {
            echo!("Panic at {}: {}", location, payload);
        } else {
            echo!("Panic: {}", payload);
        }
    }));

    // [MoleWorld 点击即玩] touchHLE 默认传空参数 → 弹出 app 选择器。这里改为:把内置在
    // APK assets 里的 MoleWorld.ipa 复制到外部存储的 touchHLE_apps/(touchHLE 的
    // BundleData 只能从真实文件路径加载,读不了 APK asset),再用该路径直接启动 → 跳过
    // 选择器 = 双击图标即玩。仅首次复制;若复制失败则退回选择器(至少不崩)。
    let args: Vec<String> = match ensure_bundled_moleworld() {
        Some(ipa_path) => vec![
            String::from("touchHLE"), // argv[0],main() 会跳过
            ipa_path,
            String::from("--landscape-right"),
            String::from("--device-family=ipad"),
        ],
        None => vec![String::new()],
    };
    match main(args.into_iter()) {
        Ok(_) => echo!("touchHLE finished"),
        Err(e) => echo!("touchHLE errored: {e:?}"),
    }
    0
}

/// [MoleWorld 点击即玩] 确保内置游戏已落到外部存储,返回其 .ipa 路径(失败返回 None)。
/// 游戏以单个 MoleWorld.ipa 内置于 APK assets(见 CI 的"内置 MoleWorld 到 assets"步骤),
/// 首次启动时复制到 touchHLE_apps/MoleWorld.ipa,之后复用。
#[cfg(target_os = "android")]
fn ensure_bundled_moleworld() -> Option<String> {
    use std::io::Read;
    let apps_dir = paths::user_data_base_path().join(paths::APPS_DIR);
    let target = apps_dir.join("MoleWorld.ipa");
    if target.is_file() {
        return Some(target.to_string_lossy().into_owned());
    }
    if let Err(e) = std::fs::create_dir_all(&apps_dir) {
        echo!("[MoleWorld] 创建目录 {:?} 失败: {:?}", apps_dir, e);
        return None;
    }
    // 从 APK assets 读取内置的 MoleWorld.ipa(经 SDL2 的 Android assets 封装)。
    let mut data = Vec::new();
    match paths::ResourceFile::open("MoleWorld.ipa") {
        Ok(mut rf) => {
            if let Err(e) = rf.get().read_to_end(&mut data) {
                echo!("[MoleWorld] 读取内置 MoleWorld.ipa 失败: {:?}", e);
                return None;
            }
        }
        Err(e) => {
            echo!("[MoleWorld] 打开内置 MoleWorld.ipa(APK asset)失败: {}", e);
            return None;
        }
    }
    if let Err(e) = std::fs::write(&target, &data) {
        echo!("[MoleWorld] 写入 {:?} 失败: {:?}", target, e);
        return None;
    }
    echo!(
        "[MoleWorld] 已复制内置游戏到 {:?}({} 字节)",
        target,
        data.len()
    );
    Some(target.to_string_lossy().into_owned())
}

/// iOS entry, called from the app executable's `SDL_UIKitRunApp` callback in
/// bin.rs (that callback is the `SDL_main_func` SDL invokes after UIApplication
/// setup). Defined as a normal `pub fn` (NOT `#[no_mangle]`) so it survives
/// cross-crate fat-LTO when the bin references it — a bare `#[no_mangle]` symbol
/// in the lib gets internalized by the lib's LTO and isn't visible to the bin.
/// The game (MoleWorld.ipa) is bundled inside the .app; we load it directly from
/// the read-only bundle (BundleData reads the zip in place, no copy) and skip the
/// app picker → tap the icon to play. Saves go to the writable pref_path (see
/// paths.rs get_macos_bundled_resources_path iOS arm).
#[cfg(target_os = "ios")]
pub fn ios_entry() {
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s
        } else {
            "(non-string payload)"
        };
        if let Some(location) = info.location() {
            echo!("Panic at {}: {}", location, payload);
        } else {
            echo!("Panic: {}", payload);
        }
    }));

    let base = sdl2::filesystem::base_path().unwrap_or_else(|_| String::from("./"));
    let game = std::path::Path::new(&base).join("MoleWorld.ipa");
    let args = vec![
        String::from("touchHLE"), // argv[0], skipped by main()
        game.to_string_lossy().into_owned(),
        String::from("--landscape-right"),
        String::from("--device-family=ipad"),
    ];
    match main(args.into_iter()) {
        Ok(_) => echo!("touchHLE finished"),
        Err(e) => echo!("touchHLE errored: {e:?}"),
    }
}

const USAGE: &str = "\
Usage:
    touchHLE [PATH] [OPTIONS]

PATH should be a path to a .app bundle or .ipa file.

If no app path or special option is specified, a GUI app picker is displayed.

Special options:
    --help
        Display this help text.

    --copyright
        Display copyright, authorship and license information.

    --info
        Print basic information about the app bundle without running the app.
";

/// [crash logging] Windows 顶层异常过滤器:GL 调用等触发的 native 访问违例是 SEH
/// 异常、不是 Rust panic,现有 panic 钩子收不到 → 没有它日志会在崩溃处干净截断。
/// 这里在进程被系统终结前往 touchHLE_log.txt 追加一行错误,然后返回
/// EXCEPTION_CONTINUE_SEARCH(放行默认处理,进程照常崩溃退出,行为不变)。
/// 全程裸指针判空、不上 logging 锁、不 unwrap;writeln! 直写文件不经 String。
#[cfg(windows)]
unsafe extern "system" fn native_exception_filter(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    use std::io::Write;
    use windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_CONTINUE_SEARCH;
    let (code, addr): (u32, usize) = if !info.is_null() && !(*info).ExceptionRecord.is_null() {
        let rec = &*(*info).ExceptionRecord;
        (rec.ExceptionCode as u32, rec.ExceptionAddress as usize)
    } else {
        (0, 0)
    };
    let kind = match code {
        0xC0000005 => "access violation (segfault)",
        0xC000001D => "illegal instruction",
        0xC00000FD => "stack overflow",
        0xC0000094 => "integer divide by zero",
        _ => "native exception",
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(crate::paths::user_data_base_path().join("touchHLE_log.txt"))
    {
        let _ = writeln!(
            f,
            "FATAL native exception 0x{:08X} ({}) at 0x{:016X} — 崩溃点见上方最后一条 [marker]/[splash]/[appframe] 日志",
            code, kind, addr
        );
        let _ = f.flush();
    }
    EXCEPTION_CONTINUE_SEARCH
}

/// [crash logging] 注册上面的顶层异常过滤器(仅 Windows)。
#[cfg(windows)]
fn install_native_crash_handler() {
    use windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;
    // SAFETY: 仅传入一个有效的 extern "system" fn 指针。
    unsafe {
        SetUnhandledExceptionFilter(Some(native_exception_filter));
    }
}

pub fn main<T: Iterator<Item = String>>(mut args: T) -> Result<(), String> {
    // [crash logging] 强制开启 backtrace(若未设),让 panic 钩子能拿到符号栈。
    if std::env::var_os("RUST_BACKTRACE").is_none() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }
    // [crash logging] Windows 原生崩溃(访问违例/segfault,如 GL 调用)是 SEH 异常、
    // 不是 Rust panic,panic 钩子抓不到。装一个顶层异常过滤器,在进程死前往日志
    // 写一行 "FATAL native exception ...",避免日志干净截断。仅 Windows。
    #[cfg(windows)]
    install_native_crash_handler();
    // [crash logging] 全平台 panic 钩子:把 Rust panic 的消息 + 位置(file:line)+
    // 栈回溯写进 touchHLE_log.txt(echo! 已每行落盘),这样硬崩溃也能留下完整错误。
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<&str>() {
            *s
        } else if let Some(s) = info.payload().downcast_ref::<String>() {
            s.as_str()
        } else {
            "(non-string payload)"
        };
        if let Some(location) = info.location() {
            echo!("Rust panic 于 {}: {}", location, payload);
        } else {
            echo!("Rust panic: {}", payload);
        }
        echo!("栈回溯:\n{}", std::backtrace::Backtrace::force_capture());
    }));

    echo!(
        "touchHLE {}{}{} — https://touchhle.org/",
        branding(),
        if branding().is_empty() { "" } else { " " },
        VERSION,
    );
    if GITHUB_RUN_ID.is_some() && !branding().is_empty() {
        echo!(
            "Built from branch {:?} of {:?} by GitHub Actions workflow run {}/{}/actions/runs/{}.",
            GITHUB_REF_NAME.unwrap(),
            GITHUB_REPOSITORY.unwrap(),
            GITHUB_SERVER_URL.unwrap(),
            GITHUB_REPOSITORY.unwrap(),
            GITHUB_RUN_ID.unwrap()
        );
    }
    echo!();

    {
        let base_path = paths::user_data_base_path();
        log!("Base path for touchHLE files: {}", base_path.display());
        paths::prepopulate_user_data_dir();
    }

    let _ = args.next().unwrap(); // skip argv[0]

    let mut bundle_path: Option<PathBuf> = None;
    let mut just_info = false;
    let mut option_args = Vec::new();
    let mut options = options::Options::default();
    let mut app_args = None::<Vec<String>>;

    for arg in args {
        if let Some(ref mut app_args) = app_args {
            app_args.push(arg);
        } else if arg == "--args" {
            app_args = Some(Vec::new());
        } else if arg == "--help" {
            echo!("{}", USAGE);
            echo!("{}", options::OPTIONS_HELP);
            return Ok(());
        } else if arg == "--copyright" {
            echo!("{}", licenses::get_text());
            return Ok(());
        } else if arg == "--info" {
            just_info = true;
        // Parse an option and store a backup in option_args so that we can
        // reapply them after file options are loaded. This ensures that
        // command line options take precedence over file options.
        } else if options.parse_argument(&arg)? {
            option_args.push(arg);
        } else if bundle_path.is_none() {
            bundle_path = Some(PathBuf::from(arg));
        } else {
            echo!("{}", USAGE);
            echo!("{}", options::OPTIONS_HELP);
            return Err(format!("Unexpected argument: {arg:?}"));
        }
    }

    if options.dumping_options.symbols {
        let mut file = std::fs::File::create(&options.dumping_file).map_err(|e| e.to_string())?;
        dyld::Dyld::dump_host_symbols(&mut file).unwrap();
        return Ok(());
    }

    let bundle_path = if let Some(bundle_path) = bundle_path {
        bundle_path
    } else {
        let mut options = options::Options::default();
        // Apply command-line options only (no app-specific options apply)
        for option_arg in &option_args {
            let parse_result = options.parse_argument(option_arg);
            assert!(parse_result == Ok(true));
        }
        if options.headless {
            return Err(
                "No app specified. Use the --help flag to see command-line usage.".to_string(),
            );
        }
        echo!(
            "No app specified, opening app picker. Use the --help flag to see command-line usage."
        );
        let (bundle_path, mut extra_options) = environment::app_picker::app_picker(options)?;
        option_args.append(&mut extra_options);
        bundle_path
    };

    // When PowerShell does tab-completion on a directory, for some reason it
    // expands it to `'..\My Bundle.app\'` and that trailing \ seems to
    // get interpreted as escaping a double quotation mark?
    #[cfg(windows)]
    if let Some(fixed) = bundle_path.to_str().and_then(|s| s.strip_suffix('"')) {
        log!("Warning: The bundle path has a trailing quotation mark! This often happens accidentally on Windows when tab-completing, because '\\\"' gets interpreted by Rust in the wrong way. Did you meant to write {:?}?", fixed);
    }

    let bundle_data = fs::BundleData::open_any(&bundle_path)
        .map_err(|e| format!("Could not open app bundle: {e}"))?;
    let (bundle, fs) = match bundle::Bundle::new_bundle_and_fs_from_host_path(
        bundle_data,
        /* read_only_mode: */ false,
    ) {
        Ok(bundle) => bundle,
        Err(err) => {
            return Err(format!("Application bundle error: {err}. Check that the path is to an .app directory or an .ipa file."));
        }
    };

    let app_id = bundle.bundle_identifier();
    let minimum_os_version = bundle.minimum_os_version();
    let required_device_capabilities = bundle.required_device_capabilities();
    let device_family = bundle.device_family_array();

    echo!("App bundle info:");
    echo!("- Display name: {}", bundle.display_name());
    echo!("- Version: {}", bundle.bundle_version());
    echo!("- Identifier: {}", app_id);
    if let Some(canonical_name) = bundle.canonical_bundle_name() {
        echo!("- Internal name (canonical): {}.app", canonical_name);
    } else {
        echo!("- Internal name (from FS): {}.app", bundle.bundle_name());
    }
    echo!(
        "- Minimum OS version: {}",
        minimum_os_version.unwrap_or("(not specified)")
    );
    echo!(
        "- Required device capabilities: {}",
        if !required_device_capabilities.is_empty() {
            required_device_capabilities.join(", ")
        } else {
            "(not specified)".to_string()
        }
    );
    echo!(
        "- Device family: {}",
        if !device_family.is_empty() {
            device_family
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        } else {
            "(not specified)".to_string()
        }
    );
    echo!();

    if let Some(version) = minimum_os_version {
        let (major, minor_etc) = version.split_once('.').unwrap();
        let minor = minor_etc
            .split_once('.')
            .map_or(minor_etc, |(minor, _etc)| minor);
        let major: u32 = major.parse().unwrap();
        let minor: u32 = minor.parse().unwrap();
        if major > 4 || (major == 4 && minor > 0) {
            echo!("Warning: app requires OS version {}. Only apps for iOS 4.0 and earlier are currently supported.", version);
        }
    }

    if required_device_capabilities.contains(&"opengles-2")
        || required_device_capabilities.contains(&"opengles-3")
    {
        echo!("Warning: app requires OpenGL ES 2.0+ support. Only OpenGL ES 1.1 is currently supported.");
    }

    if just_info {
        return Ok(());
    }

    // Apply options from files
    fn apply_options<F: std::io::Read, P: std::fmt::Display>(
        file: F,
        path: P,
        options: &mut options::Options,
        app_id: &str,
    ) -> Result<(), String> {
        match options::get_options_from_file(file, app_id) {
            Ok(Some(options_string)) => {
                echo!(
                    "Using options from {} for this app: {}",
                    path,
                    options_string
                );
                for option_arg in options_string.split_ascii_whitespace() {
                    match options.parse_argument(option_arg) {
                        Ok(true) => (),
                        Ok(false) => return Err(format!("Unknown option {option_arg:?}")),
                        Err(err) => return Err(format!("Invalid option {option_arg:?}: {err}")),
                    }
                }
            }
            Ok(None) => {
                echo!("No options found for this app in {}", path);
            }
            Err(e) => {
                echo!("Warning: {}", e);
            }
        }
        Ok(())
    }
    let default_options_path = paths::DEFAULT_OPTIONS_FILE;
    match paths::ResourceFile::open(default_options_path) {
        Ok(mut file) => apply_options(file.get(), default_options_path, &mut options, app_id)?,
        Err(err) => echo!("Warning: Could not open {}: {}", default_options_path, err),
    }
    let user_options_path = paths::user_data_base_path().join(paths::USER_OPTIONS_FILE);
    match std::fs::File::open(&user_options_path) {
        Ok(file) => apply_options(file, user_options_path.display(), &mut options, app_id)?,
        Err(err) => echo!(
            "Warning: Could not open {}: {}",
            user_options_path.display(),
            err
        ),
    }
    echo!();

    // Apply command-line options
    for option_arg in option_args {
        let parse_result = options.parse_argument(&option_arg);
        assert!(parse_result == Ok(true));
    }

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Environment::new(bundle, fs, options.clone(), app_args.unwrap_or_default())
    }));
    let env = match res {
        Ok(ret) => match ret {
            Ok(env) => env,
            Err(e) => {
                if options.popup_errors {
                    window::show_error_messagebox(None, e.as_str());
                }
                return Err(e);
            }
        },
        Err(e) => {
            if options.popup_errors {
                let error_string = if let Some(s) = e.downcast_ref::<&str>() {
                    s
                } else if let Some(s) = e.downcast_ref::<String>() {
                    s
                } else {
                    "(non-string payload)"
                };
                window::show_error_messagebox(None, error_string);
            }
            std::panic::resume_unwind(e)
        }
    };
    env.run();
    Ok(())
}
