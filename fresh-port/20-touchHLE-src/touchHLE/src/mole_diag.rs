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
            // NB: do NOT force-enable on iOS — maybe_dump_frame's glReadPixels,
            // on the device's tile-based deferred GPU, resolves+discards the
            // renderbuffer that presentRenderbuffer then presents, blanking the
            // on-screen frame (the dump file still reads the image, which made
            // this maddening to diagnose). Env-var-gated only.
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
/// Rolling counter for the MOLE_FRAMESEQ frame-by-frame dump (debugging the drag flashing).
static SEQ_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Snapshot the just-presented window framebuffer to disk every ~30 frames, so
/// the developer can `Read` it as an image and see the game. Cheap enough at
/// ~1-2 dumps/sec; glReadPixels is the only real cost.
pub fn maybe_dump_frame(gles: &mut dyn crate::gles::GLES, viewport: (u32, u32, u32, u32)) {
    if !diag_enabled() {
        return;
    }
    let (x, y, w, h) = viewport;
    if w == 0 || h == 0 {
        return;
    }
    // [MoleWorld DIAG] MOLE_FRAMESEQ=1: dump EVERY presented frame to a rolling numbered sequence
    // (/tmp/moleframes/NNN.ppm, last ~180 frames). Lets a drag be reviewed frame-by-frame to see what
    // actually alternates ("flashing"). Read newest-first by mtime. Desktop only.
    #[cfg(not(target_os = "ios"))]
    if std::env::var_os("MOLE_FRAMESEQ").is_some() {
        let seq = SEQ_COUNTER.fetch_add(1, Ordering::Relaxed) % 180;
        let _ = std::fs::create_dir_all("/tmp/moleframes");
        crate::debug::dump_framebuffer(&format!("/tmp/moleframes/{:03}.ppm", seq), x, y, w, h, gles);
        return;
    }
    let n = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
    if n % 30 != 0 {
        return;
    }
    // [MoleWorld iOS] /tmp isn't writable in the iOS sandbox; dump into the
    // app's Documents (Files-app visible, pullable via devicectl) so we can see
    // what the DEVICE actually rendered into its CAEAGLLayer framebuffer.
    #[cfg(target_os = "ios")]
    {
        let path = crate::paths::user_data_base_path().join("mole_frame.ppm");
        crate::debug::dump_framebuffer(&path.to_string_lossy(), x, y, w, h, gles);
        return;
    }
    #[cfg(not(target_os = "ios"))]
    crate::debug::dump_framebuffer(FRAME_PATH, x, y, w, h, gles);
}

/// One synthetic touch step. Down and Up are returned on consecutive calls so a
/// tap spans two runloop iterations, which cocos2d buttons expect.
#[derive(Clone, Copy)]
pub enum Inject {
    Down(f32, f32),
    /// A touch-move step (for synthesising a drag/pan gesture).
    Move(f32, f32),
    Up(f32, f32),
    /// Toggle the debug menu (same as pressing T) — lets the harness drive the
    /// menu without synthesising a keyboard event.
    Menu,
}

static PENDING_UP: Mutex<Option<(f32, f32)>> = Mutex::new(None);
/// Queued multi-step gesture (e.g. a drag): one step returned per next_inject() call.
static INJECT_QUEUE: Mutex<std::collections::VecDeque<Inject>> =
    Mutex::new(std::collections::VecDeque::new());

/// Returns the next synthetic touch step, or None. Reads a one-line command file
/// `/tmp/mole_input`:
///   `tap <x> <y>`                      — Down then Up at (x,y)
///   `drag <x1> <y1> <x2> <y2> [steps]` — Down at (x1,y1), `steps` interpolated Moves to (x2,y2), Up
///                                        (synthesises a map pan to reproduce the drag-flashing bug)
///   `menu`                             — toggle the debug menu
/// Coordinates are guest screen points. Multi-step gestures are queued and drained one per call.
pub fn next_inject() -> Option<Inject> {
    if !diag_enabled() {
        return None;
    }
    // Drain a queued multi-step gesture (drag) first.
    {
        let mut q = INJECT_QUEUE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(step) = q.pop_front() {
            return Some(step);
        }
    }
    // Finish a tap already in progress.
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
        Some("drag") => {
            let x1: f32 = it.next()?.parse().ok()?;
            let y1: f32 = it.next()?.parse().ok()?;
            let x2: f32 = it.next()?.parse().ok()?;
            let y2: f32 = it.next()?.parse().ok()?;
            let steps: u32 = it
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(24)
                .clamp(2, 240);
            let mut q = INJECT_QUEUE.lock().unwrap_or_else(|p| p.into_inner());
            for i in 1..=steps {
                let t = i as f32 / steps as f32;
                q.push_back(Inject::Move(x1 + (x2 - x1) * t, y1 + (y2 - y1) * t));
            }
            q.push_back(Inject::Up(x2, y2));
            log_line(&format!(
                "INJECT drag {} {} -> {} {} ({} steps)",
                x1, y1, x2, y2, steps
            ));
            Some(Inject::Down(x1, y1))
        }
        _ => None,
    }
}
