/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
// Allow the crate to have a non-snake-case name (touchHLE).
// This also allows items in the crate to have non-snake-case names.
#![allow(non_snake_case)]

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
