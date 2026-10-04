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
    touchHLE::main(std::env::args())
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
