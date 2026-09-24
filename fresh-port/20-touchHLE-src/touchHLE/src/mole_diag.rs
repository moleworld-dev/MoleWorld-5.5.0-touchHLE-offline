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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
// [扫描修 2026-09-15] AtomicU32 只剩桌面帧转储计数器在用,iOS 上不导入,免得出现未使用告警。
#[cfg(not(target_os = "ios"))]
use std::sync::atomic::AtomicU32;
use std::sync::{Mutex, OnceLock};

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

/// [2026-09-16] A1-04 无头注入通道(next_inject 读命令文件)的开关:设置了 `MOLE_DEV` 且值不是 "0"
/// (与 MOLE_HUD 同一口径),或者设置了 `MOLE_DIAG`。
/// 根因:原来 next_inject 只看 diag_enabled(),脚本想发一条命令就得开 MOLE_DIAG,连带打开每 30 帧 glReadPixels
/// 截帧和 NO-OP 选择子记录,测试环境与正式环境不一致。MOLE_DEV 只打开注入通道(含文本开发命令),
/// 不截帧、不写 /tmp/mole_diag.log。MOLE_DIAG 仍按 diag_enabled() 的口径(设了就开),老脚本行为不变。
/// 两个都没设时与原来一样,每轮只读一次原子量,不碰文件;环境变量每进程只查一次。
fn dev_input_enabled() -> bool {
    static STATE: AtomicU8 = AtomicU8::new(0); // 0=未知, 1=关, 2=开
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let dev = std::env::var("MOLE_DEV").map(|v| v != "0").unwrap_or(false);
            let on = dev || diag_enabled();
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
// [2026-09-16] A1-04 注入通道改由 dev_input_enabled()(MOLE_DEV 或 MOLE_DIAG)门控,命令文件首选
// 用户数据目录下的 mole_input、兼容 /tmp/mole_input;除触摸外还能发文本开发命令(见 next_inject)。
// 截帧仍只看 MOLE_DIAG。
// ===========================================================================

#[cfg(not(target_os = "ios"))]
const FRAME_PATH: &str = "/tmp/mole_frame.ppm";

/// [2026-09-16] A1-04 命令文件名。原来硬编码 `/tmp/mole_input`:Windows、Android、iOS 没有可写的 /tmp,
/// 这些平台上没法用脚本驱动做回归。现在首选 paths::user_data_base_path() 下的同名文件(桌面是工作目录,
/// macOS .app 是 SDL pref_path,Android 是外部存储,iOS 是 App 的 Documents),再兼容读旧路径,老脚本不用改。
/// 写入方约定不变:先写临时文件、再 rename 成这个名字;读到就删。
const INPUT_NAME: &str = "mole_input";
const LEGACY_INPUT_PATH: &str = "/tmp/mole_input";

/// [2026-09-16] A1-04 命令文件候选路径,按顺序尝试。只算一次并缓存:user_data_base_path() 在 macOS .app 下
/// 每次都调 SDL pref_path、在 iOS 下每次都 create_dir_all,而注入轮询每轮 run loop 都会走到这里。
fn input_paths() -> &'static [PathBuf; 2] {
    static PATHS: OnceLock<[PathBuf; 2]> = OnceLock::new();
    PATHS.get_or_init(|| {
        [
            crate::paths::user_data_base_path().join(INPUT_NAME),
            PathBuf::from(LEGACY_INPUT_PATH),
        ]
    })
}

/// [2026-09-16] A1-04 取走一个命令文件的内容:按 input_paths() 顺序,读到就删掉该文件并返回。
/// 按字节读再宽松解码 UTF-8:原来 read_to_string 遇到非 UTF-8 内容会报错且不删文件,之后每轮都重读一遍。
/// [2026-09-16] 复审修:删不掉的命令文件不执行。原来 `let _ = remove_file` 忽略失败,文件留在原处,
/// 下一轮 run loop 又读到同一条命令:tap 只是反复点,但 give / quest / time 会每帧重复发物品、改任务、快进,
/// 直接把存档打坏(Android 外部存储、iOS Documents 权限异常或文件被占用时会遇到)。
/// 现在只有删成功(或已被别人删掉,NotFound)才返回内容;删不掉就跳过这个文件并只打一次日志,避免刷屏。
fn take_input_file() -> Option<String> {
    static REMOVE_FAIL_LOGGED: AtomicBool = AtomicBool::new(false);
    for path in input_paths() {
        if let Ok(bytes) = std::fs::read(path) {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    if !REMOVE_FAIL_LOGGED.swap(true, Ordering::Relaxed) {
                        log!(
                            "[DEVCMD] 命令文件 {} 删不掉({}),为防止每帧重复执行已忽略其内容",
                            path.display(),
                            e
                        );
                    }
                    continue;
                }
            }
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
    }
    None
}

#[cfg(not(target_os = "ios"))]
static FRAME_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Snapshot the just-presented window framebuffer to disk every ~30 frames, so
/// the developer can `Read` it as an image and see the game. Cheap enough at
/// ~1-2 dumps/sec; glReadPixels is the only real cost.
///
/// [扫描修 2026-09-15] 删了两段临时代码,勿复活:
///  - iOS 分支:每帧无节流全屏 glReadPixels + 约 3MB 分配,写 Documents/mole_frame.ppm。在真机
///    TBDR GPU 上会 resolve+discard 掉要呈现的 renderbuffer,是真机黑屏元凶之一;而且
///    debug::write_ppm 遇到不可写路径直接 unwrap panic。iOS 上本函数现在什么都不做。
///  - MOLE_FRAMESEQ 逐帧序列转储(/tmp/moleframes/NNN.ppm + SEQ_COUNTER):拖动闪烁排查用的,
///    已经闭环,仓库里没有脚本再用它。
/// 桌面行为与改动前完全一致:MOLE_DIAG=1 时每 30 帧转储一次到 /tmp/mole_frame.ppm
/// (主控无头测试依赖这一点),判断顺序也没变。
pub fn maybe_dump_frame(gles: &mut dyn crate::gles::GLES, viewport: (u32, u32, u32, u32)) {
    #[cfg(target_os = "ios")]
    {
        let _ = (gles, viewport);
    }
    #[cfg(not(target_os = "ios"))]
    {
        if !diag_enabled() {
            return;
        }
        let (x, y, w, h) = viewport;
        if w == 0 || h == 0 {
            return;
        }
        let n = FRAME_COUNTER.fetch_add(1, Ordering::Relaxed);
        if n % 30 != 0 {
            return;
        }
        crate::debug::dump_framebuffer(FRAME_PATH, x, y, w, h, gles);
    }
}

/// One synthetic touch step. Down and Up are returned on consecutive calls so a
/// tap spans two runloop iterations, which cocos2d buttons expect.
/// [2026-09-16] A1-04 加了带 String 的 Dev 变体,去掉 Copy(只有本文件和 frameworks/uikit.rs 按值使用)。
#[derive(Clone)]
pub enum Inject {
    Down(f32, f32),
    /// A touch-move step (for synthesising a drag/pan gesture).
    Move(f32, f32),
    Up(f32, f32),
    /// Toggle the debug menu (same as pressing T) — lets the harness drive the
    /// menu without synthesising a keyboard event.
    Menu,
    /// [补完 2026-09-15] `suspend <秒数>`:走与安卓切后台完全相同的 失活→挂起→激活 流程
    /// (frameworks/uikit.rs → ui_application::suspend_app),只是挂起的结束条件换成计时到点,
    /// 用来在桌面上无头验证 guest 侧的切后台流程。
    Suspend(f32),
    /// [2026-09-16] A1-04 文本开发命令(dev / quest / time / give / 带页名的 menu,以及认不出的命令):
    /// 整行原样交给 frameworks/uikit.rs,在菜单点击同一上下文里调 mole_dev::run_text_command,
    /// 结果写一行 `[DEVCMD] ok|err`,脚本 grep 这一行判断成败,不再按菜单格子坐标点。
    Dev(String),
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
///   `suspend [秒数]`                   — [补完 2026-09-15] 模拟切后台:失活→挂起 N 秒(缺省 3,钳到 0–3600)→激活,
///                                        与 Android 切后台走同一条代码路径;日志关键字「[生命周期]」
/// [2026-09-16] A1-04 文本开发命令,整行交给 mole_dev::run_text_command,日志写 `[DEVCMD] ok|err <文案>`:
///   `dev fps` / `dev grid` / `dev center` / `dev speed <倍率>` — FPS 显示 / 地图格线 / 相机回中 / 动画倍速
///   `dev trace` / `dev unlock` / `dev store` / `dev weather <类型>` — 选择子跟踪 / 解锁交互 / 建筑商店 / 天气
///   `quest main|time|vip|island <任务号>`                      — 任务跳转
///   `story <段号>`                                             — 剧情播放
///   `time <分钟>`                                              — 对象计时快进
///   `give <物品ID>`                                            — 物品放到当前地图
///   `menu <页名>`                                              — 暂不支持,回 err(不带参数的 menu 照旧开关)
/// 命令文件见 input_paths():用户数据目录下的 mole_input 优先,兼容 /tmp/mole_input;只认第一条非空行。
/// 开关见 dev_input_enabled()。
/// Coordinates are guest screen points. Multi-step gestures are queued and drained one per call.
pub fn next_inject() -> Option<Inject> {
    if !dev_input_enabled() {
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
    // [2026-09-16] A1-04 只取第一条非空行:文本命令要把整行交出去。原来对全文 split_whitespace,
    // 实际也只用到开头几个词,单行命令的行为不变。
    let content = take_input_file()?;
    let line = content.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut it = line.split_whitespace();
    match it.next() {
        // [2026-09-16] A1-04 只有不带参数的 menu 开关菜单;`menu <页名>` 落到最后的文本命令分支。
        Some("menu") if it.clone().next().is_none() => {
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
        Some("suspend") => {
            // [补完 2026-09-15] 缺省 3 秒;解析失败或非有限值(如 NaN/inf)按缺省处理;
            // 钳到 0–3600 秒,避免 Duration::from_secs_f32 / Instant 加法溢出 panic。
            let secs: f32 = it
                .next()
                .and_then(|s| s.parse::<f32>().ok())
                .filter(|s| s.is_finite())
                .unwrap_or(3.0)
                .clamp(0.0, 3600.0);
            log_line(&format!("INJECT suspend {}", secs));
            Some(Inject::Suspend(secs))
        }
        // [2026-09-16] A1-04 其余整行交给文本命令台(frameworks/uikit.rs → mole_dev::run_text_command)。
        // 认不出的命令也交过去,由它回一行 `[DEVCMD] err`,脚本不用干等到超时;原来这里静默返回 None。
        _ => {
            log_line(&format!("INJECT dev {}", line));
            Some(Inject::Dev(line.to_string()))
        }
    }
}
