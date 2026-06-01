/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! MoleWorld offline port: one-tap player-save reset.
//!
//! Wired to a start-screen ("LogoLayer") button via a short-circuit in
//! `objc::messages`. Pops a native macOS confirmation dialog (via `osascript`,
//! i.e. a real system-level dialog) and, only if the user confirms, deletes the
//! player's save files so the next launch begins a brand-new game. This is an
//! escape hatch for saves whose archived map the unarchiver can't yet fully
//! reconstruct.

use crate::fs::GuestPath;
use crate::Environment;
use std::process::Command;

/// Run an AppleScript snippet via osascript, returning its stdout (or None if
/// osascript could not be launched at all).
fn osascript(script: &str) -> Option<String> {
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Show the native confirmation dialog and, if confirmed, wipe the player's
/// saves and quit. Safe by default: any failure to show the dialog is treated
/// as "cancelled" so we never delete saves without an explicit confirmation.
pub fn confirm_and_reset_saves(env: &mut Environment) {
    let answer = osascript(
        "display dialog \"确定要清空所有存档数据吗?庄园、建筑、土地、等级和摩尔豆都会重置为新游戏。此操作无法撤销!\" buttons {\"取消\", \"确定清空\"} default button \"取消\" with title \"重置玩家存档\" with icon caution",
    );
    let confirmed = answer.as_deref().is_some_and(|s| s.contains("确定清空"));
    if !confirmed {
        log!("[RESET] save reset cancelled by user");
        return;
    }

    let home = env.fs.home_directory().as_str().to_string();
    let targets = [
        format!("{}/Documents/map.dat", home),
        format!("{}/Library/Preferences/com.taomee.MoleWorld.plist", home),
    ];
    for target in &targets {
        match env.fs.remove(GuestPath::new(target.as_str())) {
            Ok(()) => {
                log!("[RESET] removed {}", target);
            }
            Err(e) => {
                log!("[RESET] could not remove {} ({:?})", target, e);
            }
        }
    }

    let _ = osascript(
        "display dialog \"存档已清空。点击\\\"确定\\\"后游戏会退出,重新打开即是全新游戏。\" buttons {\"确定\"} default button \"确定\" with title \"重置完成\"",
    );

    log!("[RESET] player save wiped; exiting so the next launch is fresh");
    // Hard-exit on purpose: we must NOT let the game re-save its in-memory
    // (pre-reset) state on the way out, which a normal termination would do.
    std::process::exit(0);
}
