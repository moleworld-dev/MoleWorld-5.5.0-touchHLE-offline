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
    // [深扫修 2026-09-11] 原来只删 map.dat 和偏好 plist,【不删 userinfo.dat】。
    // 根因:重置后偏好没了 → GameSettings loadSettings 读到 isEncrypt=NO,而 userinfo.dat 还在 →
    // GameData loadUserInfoData 0x757c6 跳 0x75936 弹 HACK_USERINFO_DATA_ERROR(被自动关闭)→
    // alertView 回调 0x754b4 exit(0),此后每次启动都秒退(永久循环)。等级/摩尔豆也根本没重置
    // (它们就在 userinfo.dat 里)。修法:把 userinfo.dat 一并删掉,新游戏时 saveUserInfoData 会
    // 重新写 isEncrypt=YES;黄金岛 4 份自建存档也一起删,重置才名副其实。
    // [复核修 2026-09-15] R5-1/R6-3:再加两份本轮新增、且不绑定用户 ID 的旁路档——
    // vip.dat(mole_items:VIP 三值/登录日与连续天数/累计在线毫秒)与 mole_activity.dat(mole_activity:
    // 签到/脚印兑换/海底寻宝等,经 -[GameData pathForDataFile:]@0x75374 拼在 Documents 下)。
    // 不删的话新游戏会继承旧号的 VIP 等级、连续登录天数和当天已签到状态。本路径删完直接 exit(0),
    // 不会再有 side_save 把内存旧值写回,所以不需要清 mole_items 的内存状态。
    let targets = [
        format!("{}/Documents/map.dat", home),
        format!("{}/Documents/userinfo.dat", home),
        format!("{}/Documents/island_map.dat", home),
        format!("{}/Documents/island_userinfo.dat", home),
        format!("{}/Documents/island_ships.dat", home),
        format!("{}/Documents/island_fragments.dat", home),
        format!("{}/Documents/vip.dat", home),
        format!("{}/Documents/mole_activity.dat", home),
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
