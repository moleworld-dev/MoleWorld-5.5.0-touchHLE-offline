/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! MoleWorld offline port: lightweight on-disk diagnostic logger.
//!
//! GUI verification is blocked — the emulator window lives on its own macOS
//! Space and cannot be screenshotted — so instead of watching the screen we
//! record key runtime signals to a file the developer can read after a normal
//! play session. This turns "I can't see the screen" into a file-based
//! feedback loop.
//!
//! Two signals are recorded by callers in `objc::messages`:
//!  * `log_unique(class, selector)` — every Objective-C selector that silently
//!    no-ops (the "does not respond" compatibility shim). De-duplicated, so the
//!    file stays a compact list of every method that returned nil instead of
//!    running. This is the #1 suspect for both invisible buildings (a sprite
//!    setup call no-ops) and the broken leveling chain (an addXp sub-call
//!    no-ops).
//!  * `log_line(line)` — an unconditional line, used to trace the
//!    experience/leveling chain (addXp:/checkUpgrade/...) with its argument.
//!
//! Output goes to `/tmp/mole_diag.log`, truncated once per emulator run.

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::Mutex;

const DIAG_PATH: &str = "/tmp/mole_diag.log";

/// Truncate the log exactly once per process, the first time anything is logged.
static TRUNCATED: AtomicBool = AtomicBool::new(false);
/// De-dup set for `log_unique`. `Mutex::new(None)` is const; the set is created
/// lazily on first use so no non-const initializer is needed for the static.
static SEEN: Mutex<Option<HashSet<String>>> = Mutex::new(None);

/// 诊断脚手架(截帧 + 注点 + NO-OP 选择器记录)是给无头 macOS 验证流程用的
/// **开发者工具**,默认【关闭】,仅当设置环境变量 `MOLE_DIAG` 时启用。原因:
///   (1) Windows 没有 `/tmp` 目录,截帧的 `File::create("/tmp/...").unwrap()`
///       会在首帧直接 panic(实测 ea5c0f3 在 RTX 5090 上崩于 debug.rs:16);
///   (2) 对正式游戏而言,每 30 帧一次 glReadPixels + 每次 runloop 读 /tmp 文件
///       是纯开销,还会乱写文件。
/// 验证脚本(launch_game.sh)设 `MOLE_DIAG=1` 即可照常截帧。每进程只查一次环境变量。
fn diag_enabled() -> bool {
    static STATE: AtomicU8 = AtomicU8::new(0); // 0=未知, 1=关, 2=开
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os("MOLE_DIAG").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

fn ensure_fresh() {
    if !TRUNCATED.swap(true, Ordering::SeqCst) {
        let _ = std::fs::write(DIAG_PATH, b"=== mole_diag (fresh run) ===\n");
    }
}

fn append(line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(DIAG_PATH) {
        let _ = writeln!(f, "{}", line);
    }
}

/// Append a line unconditionally (used for the exp/leveling trace).
pub fn log_line(line: &str) {
    if !diag_enabled() {
        return;
    }
    ensure_fresh();
    append(line);
}

/// Append a `class::selector` pair the first time it is seen, so the file
/// becomes a compact unique list of every method that silently no-ops.
pub fn log_unique(class: &str, selector: &str) {
    if !diag_enabled() {
        return;
    }
    ensure_fresh();
    let key = format!("{}::{}", class, selector);
    let mut guard = match SEEN.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    let set = guard.get_or_insert_with(HashSet::new);
    if set.insert(key.clone()) {
        drop(guard);
        append(&format!("NO-OP  {}", key));
    }
}

// ===========================================================================
// Autonomous "eyes + hands": let the developer drive and observe the game even
// though the emulator window lives on its own macOS Space and can't be
// screenshotted or clicked by the host.
//   * maybe_dump_frame() snapshots the presented frame to /tmp/mole_frame.ppm.
//   * next_inject() feeds synthetic taps from /tmp/mole_input ("tap <x> <y>").
// ===========================================================================

const FRAME_PATH: &str = "/tmp/mole_frame.ppm";
const INPUT_PATH: &str = "/tmp/mole_input";

static FRAME_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Snapshot the just-presented window framebuffer to disk every ~30 frames, so
/// the developer can `Read` it as an image and see the game. Cheap enough at
/// ~1-2 dumps/sec; glReadPixels is the only real cost.
pub fn maybe_dump_frame(gles: &mut dyn crate::gles::GLES, viewport: (u32, u32, u32, u32)) {
    if !diag_enabled() {
        return;
    }
    let n = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    if n % 30 != 0 {
        return;
    }
    let (x, y, w, h) = viewport;
    if w == 0 || h == 0 {
        return;
    }
    crate::debug::dump_framebuffer(FRAME_PATH, x, y, w, h, gles);
}

/// One synthetic touch step. Down and Up are returned on consecutive calls so a
/// tap spans two runloop iterations, which cocos2d buttons expect.
#[derive(Clone, Copy)]
pub enum Inject {
    Down(f32, f32),
    Up(f32, f32),
    /// Toggle the debug menu (same as pressing T) — lets the harness drive the
    /// menu without synthesising a keyboard event.
    Menu,
}

static PENDING_UP: Mutex<Option<(f32, f32)>> = Mutex::new(None);

/// Returns the next synthetic touch step, or None. Reads a one-line command file
/// `/tmp/mole_input` of the form `tap <x> <y>` (guest screen points) and turns
/// it into a Down (this call) followed by an Up (next call).
pub fn next_inject() -> Option<Inject> {
    if !diag_enabled() {
        return None;
    }
    // Finish a tap already in progress first.
    {
        let mut pend = match PENDING_UP.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some((x, y)) = pend.take() {
            return Some(Inject::Up(x, y));
        }
    }
    let content = std::fs::read_to_string(INPUT_PATH).ok()?;
    let _ = std::fs::remove_file(INPUT_PATH);
    let mut it = content.split_whitespace();
    match it.next() {
        Some("menu") => {
            log_line("INJECT menu toggle");
            Some(Inject::Menu)
        }
        Some("tap") => {
            let x: f32 = it.next()?.parse().ok()?;
            let y: f32 = it.next()?.parse().ok()?;
            let mut pend = match PENDING_UP.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            *pend = Some((x, y));
            log_line(&format!("INJECT tap {} {}", x, y));
            Some(Inject::Down(x, y))
        }
        _ => None,
    }
}
