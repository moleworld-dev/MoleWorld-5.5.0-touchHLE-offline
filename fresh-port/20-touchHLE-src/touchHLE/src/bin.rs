/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]
// [2026-10-04 第八轮 R8-D4] Windows 发行版做成图形程序,不再带黑色控制台窗口:点控制台的关闭按钮会把
// 进程直接结束、不存档,改成图形程序就没有这条路。日志本来就写 touchHLE_log.txt,启动失败和崩溃
// 有错误弹窗;没有控制台时写 stderr 会被静默丢弃。调试版保留控制台,方便开发时看输出。
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(not(target_os = "ios"))]
fn main() -> Result<(), String> {
    let result = touchHLE::main(bundled_game_args(std::env::args().collect()).into_iter());
    // [2026-10-07 第十一轮 R11-H-2] 建窗之前就出错时(找不到游戏包、游戏文件夹不可写等)补一个中文错误框:
    // 发行版是图形程序,stderr 玩家看不到。已经弹过框或命令行关了弹框就不弹。
    if let Err(ref e) = result {
        touchHLE::report_startup_error(e);
    }
    result
}

/// [2026-10-07 第十一轮 R11-H-3] Windows/Linux 发行包:游戏路径只由 Run-MoleWorld.bat 作为参数传入。玩家直接双击
/// touchHLE.exe、或把运行中的游戏「固定到任务栏」后从任务栏启动(记下的是 exe 本身,不带参数),以前会进英文
/// 应用选择器并提示找不到 touchHLE_apps,进不了游戏。现在没有游戏路径参数、且 exe 同目录有 MoleWorld.app 时,
/// 先切到 exe 所在目录(存档和日志都按当前目录放,起始目录不同会像丢档),再按 bat 的参数加载游戏。
/// macOS .app 与安卓/iOS 入口不受影响。
#[cfg(not(any(target_os = "ios", target_os = "android", target_os = "macos")))]
fn bundled_game_args(args: Vec<String>) -> Vec<String> {
    let has_bundle = args
        .iter()
        .skip(1)
        .take_while(|a| a.as_str() != "--args")
        .any(|a| !a.starts_with("--"));
    if has_bundle {
        return args;
    }
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_path_buf()))
    else {
        return args;
    };
    if !dir.join("MoleWorld.app").is_dir() || std::env::set_current_dir(&dir).is_err() {
        return args;
    }
    let mut out = vec![
        args.first().cloned().unwrap_or_default(),
        "MoleWorld.app".to_string(),
        "--landscape-right".to_string(),
        "--device-family=ipad".to_string(),
    ];
    out.extend(args.into_iter().skip(1));
    out
}

#[cfg(any(target_os = "android", target_os = "macos"))]
fn bundled_game_args(args: Vec<String>) -> Vec<String> {
    args
}

// On iOS the app's main executable must hand control to SDL's UIKit runner,
// which sets up the UIApplication run loop and then calls our SDL_main (defined
// in the library — see lib.rs). SDL_UIKitRunApp comes from the statically-linked
// SDL2. This avoids needing a separate Objective-C main.m and the static-lib
// symbol-retention issues (the bin references SDL_main so it's kept).
#[cfg(target_os = "ios")]
fn main() {
    use std::ffi::{c_char, c_int};
    // The SDL_main_func SDL calls (on the main thread) after UIApplication setup.
    // Defined here in the bin so it's retained (referenced by main); it just
    // hands off to the library's ios_entry (a normal pub fn, LTO-safe).
    extern "C" fn touchhle_sdl_main(_argc: c_int, _argv: *mut *mut c_char) -> c_int {
        touchHLE::ios_entry();
        0
    }
    extern "C" {
        fn SDL_UIKitRunApp(
            argc: c_int,
            argv: *mut *mut c_char,
            main_function: extern "C" fn(c_int, *mut *mut c_char) -> c_int,
        ) -> c_int;
    }
    unsafe {
        SDL_UIKitRunApp(0, std::ptr::null_mut(), touchhle_sdl_main);
    }
}
