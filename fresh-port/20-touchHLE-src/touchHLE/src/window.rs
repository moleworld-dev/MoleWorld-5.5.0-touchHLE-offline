/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Abstraction of window setup, OpenGL context creation and event handling.
//!
//! Implemented using the sdl2 crate (a Rust wrapper for SDL2). All usage of
//! SDL should be confined to this module.
//!
//! There is currently no separation of concerns between a single window and
//! window system interaction in general, because it is assumed only one window
//! will be needed for the runtime of the app.

use crate::gles::present::present_frame;
use crate::gles::{create_gles1_ctx_no_parent_stack, GLESContext, GLES};
use crate::image::Image;
use crate::matrix::Matrix;
use crate::options::Options;
use crate::Environment;
use sdl2::mouse::MouseButton;
use sdl2::pixels::PixelFormatEnum;
use sdl2::surface::Surface;
use sdl2_sys::SDL_PowerState;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::f32::consts::{FRAC_PI_2, PI};
use std::num::NonZeroU32;
use std::ptr::null_mut;
use std::time::{Duration, Instant};

#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceFamily {
    iPhone,
    iPad,
}
impl std::fmt::Display for DeviceFamily {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}
impl DeviceFamily {
    pub fn portrait_size(&self) -> (u32, u32) {
        // [MoleWorld 智能分辨率·测试分支] MOLE_GUEST_PORTRAIT=WxH 覆盖 guest 逻辑屏(点,portrait 维度)。
        // 用途="物理满屏不黑边"折中:喂一个更宽的 winSize → 游戏世界场景(村庄/岛,checkBounding 读
        // winSize)自然扩视野铺满 + winSize 相对 UI(底部菜单/弹窗)自动重锚;顶部 HUD 等写死坐标保持
        // 老位(后续 targeted 重锚 + present 模糊填缝补)。★portrait 维度:landscape 时 size_for_orientation
        // 交换宽高,故"加宽 landscape"=加大这里的 height(如 768x1366 → landscape 1366x768=16:9)。
        // ui_screen bounds(guest winSize)与 window size 都走本函数 → 窗口自动匹配 guest 宽高比=无 letterbox。
        // 仅 env 显式设置时生效;默认(不设)逐字节不变,零回归。
        if let Some(sz) = guest_portrait_override() {
            return sz;
        }
        // [MoleWorld 智能分辨率] 第二层:MOLE_FILL=1 时由 Window::new 按目标屏宽高比自动算的 guest 逻辑屏。
        if let Some(&sz) = AUTO_PORTRAIT.get() {
            return sz;
        }
        match self {
            DeviceFamily::iPhone => (320, 480),
            DeviceFamily::iPad => (768, 1024),
        }
    }
}

/// [MoleWorld 智能分辨率] 自动适配(--fill-screen / MOLE_FILL)时,Window::new 按目标屏宽高比
/// 算好的 guest portrait 逻辑屏。
static AUTO_PORTRAIT: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();

/// [MoleWorld 智能分辨率] CLI `--logical-size=WxH` 显式指定的 guest portrait 逻辑屏(点,已归一
/// 成 portrait=(短,长))。优先级高于 env `MOLE_GUEST_PORTRAIT`。由 [apply_cli_resolution] 写入。
static CLI_PORTRAIT: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
/// [MoleWorld 智能分辨率] CLI `--fill-screen` 开关(等价 MOLE_FILL=1,一等公民)。
static CLI_FILL_SCREEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// [MoleWorld 智能分辨率] CLI `--max-aspect=F` 覆盖(自动适配时 guest landscape 宽高比上限)。
static CLI_MAX_ASPECT: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
/// [MoleWorld 智能分辨率]「4:3 完美模式」环境补边开关(--ambient-fill)。present 据此:letterbox
/// 空白处用【画面横向拉伸+压暗】填充代替黑边。默认关。
static AMBIENT_FILL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// [MoleWorld 智能分辨率] 设置环境补边开关(Window::new 从 Options 应用一次)。
pub fn set_ambient_fill(on: bool) {
    AMBIENT_FILL.store(on, std::sync::atomic::Ordering::Relaxed);
}
/// [MoleWorld 智能分辨率] present 查:是否启用环境补边。
pub fn ambient_fill_active() -> bool {
    AMBIENT_FILL.load(std::sync::atomic::Ordering::Relaxed)
}

/// [MoleWorld 智能分辨率] 建窗前把 CLI 分辨率选项写进上面的模块静态量。之所以走静态量:
/// [DeviceFamily::portrait_size] 挂在 DeviceFamily 上,且 ui_screen bounds / 窗口尺寸 / fs 宽图
/// 重定向 / 触摸映射等多路消费者都拿不到 [Options],只能读全局。仅此一处写、建窗前调一次。
pub fn apply_cli_resolution(
    logical_size: Option<(u32, u32)>,
    fill_screen: bool,
    max_aspect: Option<f32>,
) {
    if let Some((w, h)) = logical_size {
        if w != 0 && h != 0 {
            // 归一成 portrait=(短边,长边),用户传 1366x768 或 768x1366 皆可。
            let _ = CLI_PORTRAIT.set((w.min(h), w.max(h)));
        }
    }
    if fill_screen {
        CLI_FILL_SCREEN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(a) = max_aspect {
        let _ = CLI_MAX_ASPECT.set(a);
    }
}

/// [MoleWorld 智能分辨率] 显式 guest portrait 逻辑屏覆盖:CLI `--logical-size` 优先,其次
/// env `MOLE_GUEST_PORTRAIT=WxH`(portrait 点尺寸,仅解析一次)。
fn guest_portrait_override() -> Option<(u32, u32)> {
    if let Some(&sz) = CLI_PORTRAIT.get() {
        return Some(sz);
    }
    static OVERRIDE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *OVERRIDE.get_or_init(|| {
        let s = std::env::var("MOLE_GUEST_PORTRAIT").ok()?;
        let (w, h) = s.split_once('x')?;
        let w: u32 = w.trim().parse().ok()?;
        let h: u32 = h.trim().parse().ok()?;
        if w == 0 || h == 0 {
            return None;
        }
        log!(
            "[MOLE-RES] guest 逻辑屏覆盖 MOLE_GUEST_PORTRAIT={}x{}(landscape={}x{})",
            w,
            h,
            h,
            w
        );
        Some((w, h))
    })
}

/// [MoleWorld 智能分辨率] 是否请求自动铺屏适配(--fill-screen 或 MOLE_FILL=1)。
fn fill_screen_requested() -> bool {
    CLI_FILL_SCREEN.load(std::sync::atomic::Ordering::Relaxed)
        || std::env::var("MOLE_FILL").map(|v| v != "0").unwrap_or(false)
}

/// [MoleWorld 智能分辨率] 自动适配时 guest landscape 宽高比上限。默认 2.4(≈21.6:9,覆盖
/// 16:9 / 16:10 / 21:9 等主流桌面比例 → 零黑边);仅超宽屏(如 32:9)会被钳到此值、留极小
/// pillarbox 以避免横向拉伸变形。可用 `--max-aspect=` 或 env MOLE_MAX_ASPECT 调,夹在 [4:3, 4.0]。
fn fill_max_aspect() -> f32 {
    CLI_MAX_ASPECT
        .get()
        .copied()
        .or_else(|| {
            std::env::var("MOLE_MAX_ASPECT")
                .ok()
                .and_then(|s| s.trim().parse().ok())
        })
        .unwrap_or(2.4)
        .clamp(4.0 / 3.0, 4.0)
}

/// [MoleWorld 智能分辨率] 由目标屏长短边算 guest portrait 逻辑屏(FixedHeight Hor+):锁短边
/// = base_short(iPad 768 / iPhone 320),长边按【钳制后的】屏宽高比缩放。比例夹在
/// [4:3(游戏原生下限,更窄会裁掉为 1024 宽设计的内容), max_aspect]。返回 portrait 维度 (短,长)。
fn compute_fill_portrait(base_short: u32, long: u32, short: u32) -> (u32, u32) {
    let raw = if short > 0 {
        long as f32 / short as f32
    } else {
        4.0 / 3.0
    };
    let aspect = raw.clamp(4.0 / 3.0, fill_max_aspect());
    let landscape_long = ((base_short as f32) * aspect).round() as u32;
    (base_short, landscape_long)
}

/// [MoleWorld 智能分辨率] 是否有【定制】guest 逻辑屏(显式 --logical-size/env,或自动 fill 已算出)。
/// [Window::viewport] 据此:定制时走【等比缩放】(不变形,且 guest 比例≈屏比例故无黑边);默认
/// (无定制)保持窗口模式自由拉伸铺满(零回归)。
fn custom_guest_size_active() -> bool {
    guest_portrait_override().is_some() || AUTO_PORTRAIT.get().is_some()
}

/// [MoleWorld 宽屏] 当前 guest 逻辑屏是否比 4:3 更宽(landscape 宽 > 1024)。
/// portrait 覆盖 (W,H) → landscape (H,W),故 landscape 宽 = portrait 高。用于 fs 层在宽屏时
/// 把整屏底图 `X.png` 透明重定向到宽版 `X_wide.png`(见 src/fs.rs lookup_node)。默认(无覆盖)
/// = 4:3 → false → 不重定向,零回归。
pub fn is_widescreen() -> bool {
    let (_w, h) = guest_portrait_override()
        .or_else(|| AUTO_PORTRAIT.get().copied())
        .unwrap_or((0, 0));
    h > 1024
}
impl TryFrom<u64> for DeviceFamily {
    type Error = ();
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(DeviceFamily::iPhone),
            2 => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}
impl TryFrom<&str> for DeviceFamily {
    type Error = ();
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "iphone" => Ok(DeviceFamily::iPhone),
            "ipad" => Ok(DeviceFamily::iPad),
            _ => Err(()),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DeviceOrientation {
    Portrait,
    PortraitUpsideDown,
    LandscapeLeft,
    LandscapeRight,
}
fn size_for_orientation(
    family: DeviceFamily,
    orientation: DeviceOrientation,
    scale_hack: NonZeroU32,
) -> (u32, u32) {
    let (width, height) = family.portrait_size();
    let scale_hack = scale_hack.get();
    match orientation {
        DeviceOrientation::Portrait => (width * scale_hack, height * scale_hack),
        DeviceOrientation::PortraitUpsideDown => (width * scale_hack, height * scale_hack),
        DeviceOrientation::LandscapeLeft => (height * scale_hack, width * scale_hack),
        DeviceOrientation::LandscapeRight => (height * scale_hack, width * scale_hack),
    }
}
fn rotate_fullscreen_size(orientation: DeviceOrientation, screen_size: (u32, u32)) -> (u32, u32) {
    let (short_side, long_side) = if screen_size.0 < screen_size.1 {
        (screen_size.0, screen_size.1)
    } else {
        (screen_size.1, screen_size.0)
    };
    match orientation {
        DeviceOrientation::Portrait | DeviceOrientation::PortraitUpsideDown => {
            (short_side, long_side)
        }
        DeviceOrientation::LandscapeLeft | DeviceOrientation::LandscapeRight => {
            (long_side, short_side)
        }
    }
}
/// Tell SDL2 what orientation we want. Only useful on Android.
fn set_sdl2_orientation(orientation: DeviceOrientation) {
    // Despite the name, this hint works on Android too.
    sdl2::hint::set(
        "SDL_IOS_ORIENTATIONS",
        match orientation {
            DeviceOrientation::Portrait => "Portrait",
            // The inversion is deliberate. These probably correspond to
            // iPhone OS content orientations?
            DeviceOrientation::PortraitUpsideDown => "PortraitUpsideDown",
            DeviceOrientation::LandscapeLeft => "LandscapeRight",
            DeviceOrientation::LandscapeRight => "LandscapeLeft",
        },
    );
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum FingerId {
    Mouse,
    Touch(i64),
    VirtualCursor,
    ButtonToTouch(crate::options::Button),
    StickToTouch,
    DpadToTouch,
}
pub type Coords = (f32, f32);

struct DpadState {
    left: bool,
    right: bool,
    up: bool,
    down: bool,
    active: bool,
}

#[derive(Debug)]
pub enum TextInputEvent {
    Text(String),
    Backspace,
    Return,
}

#[derive(Debug)]
pub enum Event {
    /// User requested quit.
    Quit,
    /// OS has informed touchHLE it will soon become inactive.
    /// (iOS `applicationWillResignActive:`, Android `onPause()`)
    AppWillResignActive,
    /// [MoleWorld iOS] OS told touchHLE the app entered the TRUE background.
    /// (iOS `applicationDidEnterBackground:`) GL is illegal until foreground.
    AppDidEnterBackground,
    /// [MoleWorld iOS] OS told touchHLE the app is about to return to foreground.
    /// (iOS `applicationWillEnterForeground:`) Only fires after a true background.
    AppWillEnterForeground,
    /// [MoleWorld iOS] OS told touchHLE the app became active again.
    /// (iOS `applicationDidBecomeActive:`) Fires for every resume, including
    /// foreground overlays (Control Center) that never entered the background.
    AppDidBecomeActive,
    /// OS has informed touchHLE it will soon terminate.
    /// (iOS `applicationWillTerminate:`, Android `onDestroy()`)
    AppWillTerminate,
    TouchesDown(HashMap<FingerId, Coords>),
    TouchesMove(HashMap<FingerId, Coords>),
    TouchesUp(HashMap<FingerId, Coords>),
    /// User pressed F12, requesting that execution be paused and the debugger
    /// take over.
    EnterDebugger,
    /// [MoleWorld] User pressed T, requesting the debug/cheat menu be toggled.
    ToggleMoleMenu,
    TextInput(TextInputEvent),
}

pub enum BatteryState {
    Unknown,
    OnBattery,
    NoBattery,
    Charging,
    Full,
}

pub enum GLVersion {
    /// OpenGL ES 1.1
    GLES11,
    /// OpenGL 2.1 compatibility profile
    GL21Compat,
}

pub struct GLContext(sdl2::video::GLContext);

impl GLContext {
    pub fn is_current(&self) -> bool {
        self.0.is_current()
    }
}

fn surface_from_image(image: &Image) -> Surface<'_> {
    let src_pixels = image.pixels();
    let (width, height) = image.dimensions();

    let mut surface = Surface::new(width, height, PixelFormatEnum::RGBA32).unwrap();
    let (width, height) = (width as usize, height as usize);
    let pitch = surface.pitch() as usize;
    surface.with_lock_mut(|dst_pixels| {
        for y in 0..height {
            for x in 0..width {
                for channel in 0..4 {
                    let src_idx = y * width * 4 + x * 4 + channel;
                    let dst_idx = y * pitch + x * 4 + channel;
                    dst_pixels[dst_idx] = src_pixels[src_idx];
                }
            }
        }
    });
    surface
}

/// [MoleWorld] 文本输入是否激活(某个 UITextField 成为第一响应者)。仅此状态下物理
/// `T` 键当普通字符输入,不再误触发修改器菜单(见 `poll_for_events` 的 T 分支);
/// `start/stop_text_input` 写、事件翻译处读。用全局原子量(那两个方法是 `&self`)。
static MOLE_TEXT_INPUT_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// [MoleWorld] 文本输入是否激活。供 `find_fullscreen_eagl_layer` 查:编辑文本时强制走
/// composition 合成路径(而非 fullscreen-EAGL 快路径),否则 UITextField/UILabel 逐字符
/// 改了文字永远不上屏(快路径只 present 游戏 GL renderbuffer、不画 UIKit overlay,
/// recomposite 又在 fullscreen-EAGL 处早退)→ 表现为"打字途中不显示、回车后才显示"。
pub fn mole_text_input_active() -> bool {
    MOLE_TEXT_INPUT_ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
}

pub struct Window {
    _sdl_ctx: sdl2::Sdl,
    video_ctx: sdl2::VideoSubsystem,
    window: sdl2::video::Window,
    event_pump: sdl2::EventPump,
    event_queue: VecDeque<Event>,
    last_polled: Instant,
    /// Separate queue for extremely high-priority events (e.g. app about to
    /// terminate).
    high_priority_event: Option<Event>,
    enable_event_polling: bool,
    /// [MoleWorld iOS] True only between `applicationDidEnterBackground:` and the
    /// next foreground event. While set, NO OpenGL ES may be issued (iOS kills
    /// any app that touches the GPU in the true background). Foreground overlays
    /// (Control Center / home-indicator) do NOT set this — they stay foreground.
    backgrounded: bool,
    #[cfg(target_os = "macos")]
    max_height: u32,
    #[cfg(target_os = "macos")]
    viewport_y_offset: u32,
    /// Copy of `fullscreen` on [Options]. Note that this is meaningless when
    /// [Self::rotatable_fullscreen] returns [true].
    fullscreen: bool,
    scale_hack: NonZeroU32,
    /// [MoleWorld] 窗口模式锁定宽高比(等比 letterbox);false=自由拉伸铺满。见 viewport()。
    lock_aspect: bool,
    internal_gl_ins: Option<Box<dyn GLESContext>>,
    splash_image: Option<Image>,
    /// [MoleWorld iOS] SDL 窗口的默认 framebuffer。桌面/安卓=0;iOS=SDL 绑到 CAEAGLLayer 的
    /// 非 0 viewFramebuffer。present_frame 绘制前绑定它。
    default_framebuffer: crate::gles::gles11_raw::types::GLuint,
    /// [MoleWorld iOS] internal_gl_ins context 的 viewRenderbuffer。iOS 的 SDL swap 走
    /// [presentRenderbuffer:GL_RENDERBUFFER],呈现【当前绑定的 renderbuffer】;故 splash /
    /// composition 在 swap 前必须把它绑回 GL_RENDERBUFFER,否则呈现到错误缓冲=黑屏。桌面/安卓=0。
    default_renderbuffer: crate::gles::gles11_raw::types::GLuint,
    device_family: DeviceFamily,
    device_orientation: DeviceOrientation,
    controller_ctx: sdl2::GameControllerSubsystem,
    controllers: Vec<sdl2::controller::GameController>,
    dpad_state: DpadState,
    stick_active: bool,
    _sensor_ctx: sdl2::SensorSubsystem,
    accelerometer: Option<sdl2::sensor::Sensor>,
    virtual_cursor_last: Option<(f32, f32, bool, bool)>,
    virtual_cursor_last_unsticky: Option<(f32, f32, Instant)>,
    virtual_accelerometer_last: Option<(f32, f32, bool)>,
    /// Whether or not we are on the "main" environment stack (rather than
    /// a coroutine stack). Checked in various functions to make sure that
    /// certain SDL functions (that call JNI functions) are on the main
    /// stack on Android.
    pub(super) on_main_stack: bool,
}

impl Window {
    /// Returns [true] if touchHLE is running on a device where we should always
    /// display fullscreen, but SDL2 will let us control the orientation, i.e.
    /// Android devices.
    pub fn rotatable_fullscreen() -> bool {
        env::consts::OS == "android"
    }
    pub fn new(
        title: &str,
        icon: Option<Image>,
        launch_image: Option<Image>,
        options: &Options,
    ) -> Window {
        let sdl_ctx = sdl2::init().unwrap();
        let video_ctx = sdl_ctx.video().unwrap();

        // The "hidapi" feature of rust-sdl2 is enabled so that sdl2::sensor
        // is available, but we don't want to enable SDL's HIDAPI controller
        // drivers because they cause duplicated controllers on macOS
        // (https://github.com/libsdl-org/SDL/issues/7479). Once that's fixed,
        // remove this (https://github.com/touchHLE/touchHLE/issues/85).
        sdl2::hint::set("SDL_JOYSTICK_HIDAPI", "0");

        if env::consts::OS == "android" {
            // It's important to set context version BEFORE window creation
            // ref. https://wiki.libsdl.org/SDL2/SDL_GLattr
            let attr = video_ctx.gl_attr();
            attr.set_context_version(1, 1);
            attr.set_context_profile(sdl2::video::GLProfile::GLES);

            // Disable blocking of event loop when app is paused.
            sdl2::hint::set("SDL_ANDROID_BLOCK_ON_PAUSE", "0");
        }

        // Separate mouse and touch events
        sdl2::hint::set("SDL_TOUCH_MOUSE_EVENTS", "0");

        // SDL2 disables the screen saver by default, but iPhone OS enables
        // the idle timer that triggers sleep by default, so we turn it back on
        // here, and then the app can disable it if it wants to.
        video_ctx.enable_screen_saver();

        let scale_hack = options.scale_hack;
        // TODO: some apps specify their orientation in Info.plist, we could use
        // that here.
        let device_family = options.device_family.unwrap_or(DeviceFamily::iPhone);
        let device_orientation = options.initial_orientation;
        let fullscreen = options.fullscreen;
        let lock_aspect = options.lock_aspect;

        // [MoleWorld 智能分辨率] 建窗前把 CLI 分辨率选项(--logical-size / --fill-screen / --max-aspect)
        // 写进模块静态量,供 portrait_size / fs 宽图重定向 / viewport 等多路消费者读取。
        apply_cli_resolution(options.logical_size, options.fill_screen, options.max_aspect);
        set_ambient_fill(options.ambient_fill);

        // [MoleWorld 智能分辨率] --fill-screen / MOLE_FILL:按目标屏(主显示器/真机设备屏)宽高比
        // 【自动】算 guest 逻辑屏,实现"物理满屏不黑边、不拉伸"——guest winSize 与屏幕同比例(钳制后)
        // → 世界场景(村庄/岛)扩视野铺满 + winSize 相对 UI 自动重锚,无 letterbox。短边按 device-family
        // 固定(iPad=768/iPhone=320),长边按屏宽高比缩放并夹在 [4:3, max_aspect](见 compute_fill_portrait)。
        // 仅当未显式指定 guest 逻辑屏(--logical-size / MOLE_GUEST_PORTRAIT)时生效(显式优先)。结果存
        // AUTO_PORTRAIT,供 portrait_size(ui_screen bounds + 窗口尺寸都走它)读取。默认(不请求)零回归。
        if fill_screen_requested() && guest_portrait_override().is_none() {
            if let Ok(db) = video_ctx.display_bounds(0) {
                let (dw, dh) = db.size();
                let (long, short) = if dw >= dh { (dw, dh) } else { (dh, dw) };
                if short > 0 {
                    let base_short = match device_family {
                        DeviceFamily::iPad => 768u32,
                        DeviceFamily::iPhone => 320u32,
                    };
                    let (pw, ph) = compute_fill_portrait(base_short, long, short);
                    let _ = AUTO_PORTRAIT.set((pw, ph));
                    log!(
                        "[MOLE-RES] 自动铺屏适配:屏 {}x{} → guest 逻辑屏 portrait={}x{}(landscape={}x{}, 比例上限={:.3})",
                        dw, dh, pw, ph, ph, pw, fill_max_aspect()
                    );
                }
            }
        }

        let mut window = if Self::rotatable_fullscreen() {
            // Without this, SDL will force fullscreen mode to be portrait.
            set_sdl2_orientation(device_orientation);
            let screen_size = video_ctx.display_bounds(0).unwrap().size();
            let (width, height) = rotate_fullscreen_size(device_orientation, screen_size);
            // [MoleWorld 智能分辨率·第三层] MOLE_HIDPI=1:iOS 开 allow_highdpi → SDL drawable_size 变
            // 【设备原生像素】(否则真机上 drawable=点尺寸,游戏只画点分辨率再被 iOS 整屏上采样=糊)。
            // viewport()/触摸映射全基于 drawable_size 自动跟随。仅 iOS/Android 全屏路径,Mac 走 else 窗口
            // 路径不受影响(铁律:iOS 渲染改动不污染 Mac)。env 门控,默认不开,真机 opt-in 实测。
            let mut wb = video_ctx.window(title, width, height);
            wb.fullscreen().opengl();
            // [MoleWorld iOS 对齐] 真机默认开高 DPI(原生像素呈现,画面清晰;见 5e2c481),MOLE_HIDPI=0 可关;
            // 其它平台保持 env opt-in。iOS 没有环境变量,若沿用 env 门控会退回点分辨率=糊(cherry-pick 回归)。
            let hidpi = std::env::var("MOLE_HIDPI")
                .map(|v| v != "0")
                .unwrap_or(cfg!(target_os = "ios"));
            if hidpi {
                wb.allow_highdpi();
                log!("[MOLE-RES] HiDPI 开启(allow_highdpi):drawable=设备原生像素");
            }
            wb.build().unwrap()
        } else if fullscreen {
            let (width, height) = video_ctx.display_bounds(0).unwrap().size();
            let window = video_ctx
                .window(title, width, height)
                .fullscreen_desktop()
                .opengl()
                .build()
                .unwrap();
            window
        } else {
            let (width, height) =
                size_for_orientation(device_family, device_orientation, scale_hack);
            // [MoleWorld] 窗口可自由改变大小、拉伸适配屏幕(用户要求)。.resizable()
            // 开放拖拽缩放;set_minimum_size 防止缩到 0。画面缩放在 viewport() 里按窗口
            // 实际 drawable_size 算(自由拉伸铺满),触摸映射沿用 viewport() 自动跟随。
            let mut builder = video_ctx.window(title, width, height);
            builder.position_centered().resizable().opengl();
            // [MoleWorld iOS] 开 high-DPI。否则 SDL 的 iOS GL view backing 只有【点】尺寸
            // (如 956×440),游戏帧 present 上去后由 CoreAnimation 放大到原生像素(2868×1320,
            // 3×)= 糊。开了之后 view backing = 原生像素,drawable_size() 返回像素,present 在原生
            // 分辨率出帧 = 清晰。触摸走 finger 路径(归一化坐标 × drawable_size),自动跟随像素
            // 尺寸,viewport()/present/touch 都基于 drawable_size 一致,无需额外改。桌面不开。
            #[cfg(target_os = "ios")]
            builder.allow_highdpi();
            let mut window = builder.build().unwrap();
            window.set_minimum_size(256, 192).ok();
            window
        };

        if env::consts::OS == "android" {
            // Sanity check
            let gl_attr = video_ctx.gl_attr();
            debug_assert_eq!(gl_attr.context_profile(), sdl2::video::GLProfile::GLES);
            debug_assert_eq!(gl_attr.context_version(), (1, 1));
        }

        if let Some(icon) = icon {
            window.set_icon(surface_from_image(&icon));
        }

        let event_pump = sdl_ctx.event_pump().unwrap();

        let controller_ctx = sdl_ctx.game_controller().unwrap();

        let sensor_ctx = sdl_ctx.sensor().unwrap();
        let mut accelerometer: Option<sdl2::sensor::Sensor> = None;
        if let Ok(num_sensors) = sensor_ctx.num_sensors() {
            for sensor_idx in 0..num_sensors {
                if let Ok(sensor) = sensor_ctx.open(sensor_idx) {
                    if sensor.sensor_type() == sdl2::sensor::SensorType::Accelerometer {
                        log!("Accelerometer detected: {}.", sensor.name());
                        accelerometer = Some(sensor);
                        break;
                    }
                }
            }
        }

        #[cfg(target_os = "macos")]
        let max_height = window.size().1;

        let mut window = Window {
            _sdl_ctx: sdl_ctx,
            video_ctx,
            window,
            event_pump,
            event_queue: VecDeque::new(),
            last_polled: Instant::now() - Duration::from_secs(1),
            high_priority_event: None,
            enable_event_polling: true,
            backgrounded: false,
            #[cfg(target_os = "macos")]
            max_height,
            #[cfg(target_os = "macos")]
            viewport_y_offset: 0,
            fullscreen,
            scale_hack,
            lock_aspect,
            internal_gl_ins: None,
            splash_image: launch_image,
            default_framebuffer: 0,
            default_renderbuffer: 0,
            device_family,
            device_orientation,
            controller_ctx,
            controllers: Vec::new(),
            dpad_state: DpadState {
                left: false,
                right: false,
                up: false,
                down: false,
                active: false,
            },
            stick_active: false,
            _sensor_ctx: sensor_ctx,
            accelerometer,
            virtual_cursor_last: None,
            virtual_cursor_last_unsticky: None,
            virtual_accelerometer_last: None,
            on_main_stack: true,
        };

        // Set up OpenGL ES context used for splash screen and app UI rendering
        // (see src/frameworks/core_animation/composition.rs). OpenGL ES is used
        // because SDL2 won't let us use more than one graphics API in the same
        // window, and we also need OpenGL ES for the app's own rendering.
        let mut gl_ins = create_gles1_ctx_no_parent_stack(&mut window, options);
        let mut window_default_fbo: crate::gles::gles11_raw::types::GLuint = 0;
        let mut window_default_rbo: crate::gles::gles11_raw::types::GLuint = 0;
        {
            let mut gl_ctx = gl_ins.make_current(&mut window);
            let desc = unsafe { gl_ctx.driver_description() };
            log!("Driver info: {}", desc);
            // [MoleWorld] 缓存给「关于」页用(此刻上下文 current,glGetString 安全)。
            crate::mole_sysinfo::set_gpu_desc(desc);
            // [crash log] GPU 已缓存、游戏版本已在 main 缓存 → 输出一次完整运行诊断块,
            // 方便用户贴日志时一眼看清「什么机器 / 什么系统 / 什么版本」。
            echo!("{}", crate::mole_sysinfo::diag_block());
            // [MoleWorld iOS] 此刻 SDL 刚 make_current、把窗口的 viewFramebuffer 留作当前绑定,
            // 抓它作为窗口默认 framebuffer。桌面/安卓=0,iOS=CAEAGLLayer 的非 0 FBO。
            let mut fbo: crate::gles::gles11_raw::types::GLint = 0;
            let mut rbo: crate::gles::gles11_raw::types::GLint = 0;
            unsafe {
                gl_ctx.GetIntegerv(crate::gles::gles11_raw::FRAMEBUFFER_BINDING_OES, &mut fbo);
                gl_ctx.GetIntegerv(crate::gles::gles11_raw::RENDERBUFFER_BINDING_OES, &mut rbo);
            }
            window_default_fbo = fbo as _;
            window_default_rbo = rbo as _;
        }
        window.default_framebuffer = window_default_fbo;
        window.default_renderbuffer = window_default_rbo;
        log!("[ios-present] SDL 窗口默认 framebuffer={} renderbuffer={}", window_default_fbo, window_default_rbo);
        window.internal_gl_ins = Some(gl_ins);

        if window.splash_image.is_some() {
            window.display_splash();
        }

        window
    }

    /// Poll for events from the OS. This needs to be done reasonably often
    /// (60Hz is probably fine) so that the host OS doesn't consider touchHLE
    /// to be unresponsive. Note that events are not returned by this function,
    /// since we often need to defer actually handling them.
    ///
    /// Since polling can be quite expensive, this function will skip it if it
    /// was called too recently.
    pub fn poll_for_events(&mut self, options: &Options) {
        assert!(self.on_main_stack);
        let now = Instant::now();
        // poll roughly twice per frame to try to avoid missing frames sometimes
        if now.duration_since(self.last_polled) < Duration::from_secs_f64(1.0 / 120.0) {
            return;
        }
        self.last_polled = now;

        fn transform_input_coords(
            window: &Window,
            (in_x, in_y): (f32, f32),
            independent_of_viewport: bool,
        ) -> (f32, f32) {
            let (vx, vy, vw, vh) = if independent_of_viewport {
                let (width, height) = size_for_orientation(
                    window.device_family,
                    window.device_orientation,
                    NonZeroU32::new(1).unwrap(),
                );
                (0, 0, width, height)
            } else {
                window.viewport()
            };
            // normalize to unit square centred on origin
            let x = (in_x - vx as f32) / vw as f32 - 0.5;
            let y = (in_y - vy as f32) / vh as f32 - 0.5;
            // rotate
            let matrix = window.rotation_matrix().inverse().unwrap();
            let [x, y] = matrix.transform([x, y]);
            // back to pixels
            let (out_w, out_h) = window.size_unrotated_unscaled();
            let out_x = (x + 0.5) * out_w as f32;
            let out_y = (y + 0.5) * out_h as f32;
            // Round to match touch precision of official devices.
            let out = (out_x.round(), out_y.round());
            out
        }
        fn transform_virt_accel_coords(window: &Window, (in_x, in_y): (i32, i32)) -> (f32, f32) {
            let (_, _, vw, vh) = window.viewport();
            let out_x = ((in_x as f32 / vw as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            let out_y = ((in_y as f32 / vh as f32) * 2.0 - 1.0).clamp(-1.0, 1.0);
            (out_x, out_y)
        }
        fn translate_button(button: sdl2::controller::Button) -> Option<crate::options::Button> {
            match button {
                sdl2::controller::Button::DPadLeft => Some(crate::options::Button::DPadLeft),
                sdl2::controller::Button::DPadUp => Some(crate::options::Button::DPadUp),
                sdl2::controller::Button::DPadRight => Some(crate::options::Button::DPadRight),
                sdl2::controller::Button::DPadDown => Some(crate::options::Button::DPadDown),
                sdl2::controller::Button::Start => Some(crate::options::Button::Start),
                sdl2::controller::Button::A => Some(crate::options::Button::A),
                sdl2::controller::Button::B => Some(crate::options::Button::B),
                sdl2::controller::Button::X => Some(crate::options::Button::X),
                sdl2::controller::Button::Y => Some(crate::options::Button::Y),
                sdl2::controller::Button::LeftShoulder => {
                    Some(crate::options::Button::LeftShoulder)
                }
                _ => None,
            }
        }
        fn finger_absolute_coords(window: &Window, (x, y): (f32, f32)) -> (f32, f32) {
            let (screen_width, screen_height) = window.window.drawable_size();
            (screen_width as f32 * x, screen_height as f32 * y)
        }

        let mut controller_updated = false;
        // event_pump doesn't have a method to peek on events
        // so, we keep track of an unconsumed one from a previous loop iteration
        // FIXME: use peek_event() from even_subsystem
        let mut previous_event: Option<sdl2::event::Event> = None;
        while self.enable_event_polling {
            use sdl2::event::Event as E;
            let event = if let Some(e) = previous_event.take() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming previous event: {:?}", e),
                }
                e
            } else if let Some(e) = self.event_pump.poll_event() {
                match e {
                    E::Unknown { .. } => (),
                    _ => log_dbg!("Consuming new event: {:?}", e),
                }
                e
            } else {
                break;
            };

            // Virtual accelerometer
            match event {
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.right() => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, true));
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Right,
                    ..
                } => {
                    let (x, y) = transform_virt_accel_coords(self, (x, y));
                    self.virtual_accelerometer_last = Some((x, y, false));
                }
                // [MoleWorld] 窗口缩放事件:
                // ① 锁比例(仅显式 --lock-aspect):把【窗口本身】约束回 guest 宽高比,拖拽时窗口
                //    始终保持游戏比例,自由铺满即等比不变形无黑边。★注意:这条走 set_size,而 macOS
                //    上 set_size 会触发 framebuffer=max(新,旧) 怪癖 → 缩小窗口后 drawable 错乱、UI 错位;
                //    故【不再】给 --fill-screen/--logical-size 等定制尺寸自动开这条(那会让"resize 后 UI
                //    错位")。定制尺寸想要"无黑边完美填满"请用 --fullscreen(全屏无 resize/无 set_size/
                //    无怪癖,guest 比例=屏比例 → 铺满不变形);windowed 定制尺寸走 viewport 自由铺满
                //    (填满无黑边,仅当把窗口拖成很不同的比例时才轻微拉伸,不会 UI 错位)。
                // ② macOS framebuffer y-offset 补偿(仅 --lock-aspect 的 set_size 路径需要)。
                // push_back 那个 match 对 Window 事件走 `_ => continue` 不入队,故此处只做副作用。
                E::Window {
                    win_event:
                        sdl2::event::WindowEvent::SizeChanged(w, h)
                        | sdl2::event::WindowEvent::Resized(w, h),
                    ..
                } => {
                    // 仅显式 --lock-aspect(且非全屏)才 set_size 锁窗口比例。
                    if self.lock_aspect
                        && !self.fullscreen
                        && !Self::rotatable_fullscreen()
                        && w > 0
                        && h > 0
                    {
                        let (app_w, app_h) = size_for_orientation(
                            self.device_family,
                            self.device_orientation,
                            self.scale_hack,
                        );
                        // 取宽/高两方向里更大的缩放比 → 窗口跟随主拖拽方向、保持 app 比例。
                        let scale = (w as f32 / app_w as f32)
                            .max(h as f32 / app_h as f32)
                            .max(0.15);
                        let tw = ((app_w as f32 * scale).round() as u32).max(1);
                        let th = ((app_h as f32 * scale).round() as u32).max(1);
                        // set_size 会再触发一次 resize;约束已满足时不再 set,避免抖动/死循环。
                        if (tw, th) != self.window.size() {
                            let _ = self.window.set_size(tw, th);
                        }
                    }
                    #[cfg(target_os = "macos")]
                    {
                        let (_, fh) = self.window.size();
                        self.max_height = self.max_height.max(fh);
                        self.viewport_y_offset = self.max_height - fh;
                    }
                }
                _ => {}
            }

            self.event_queue.push_back(match event {
                E::Quit { .. } => Event::Quit,
                E::MouseButtonDown {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("INPUT MouseButtonDown x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesDown(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseMotion {
                    x, y, mousestate, ..
                } if mousestate.left() => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("INPUT MouseMotion x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesMove(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::MouseButtonUp {
                    x,
                    y,
                    mouse_btn: MouseButton::Left,
                    ..
                } => {
                    let coords = transform_input_coords(self, (x as f32, y as f32), false);
                    log_dbg!("INPUT MouseButtonUp x {}, y {}, coords {:?}", x, y, coords);
                    Event::TouchesUp(HashMap::from([(FingerId::Mouse, coords)]))
                }
                E::ControllerDeviceAdded { which, .. } => {
                    self.controller_added(which);
                    continue;
                }
                E::ControllerDeviceRemoved { which, .. } => {
                    self.controller_removed(which);
                    continue;
                }
                // Note that accelerometer simulation with analog sticks is
                // handled with polling, rather than being event-based.
                E::ControllerButtonUp { button, .. } | E::ControllerButtonDown { button, .. } => {
                    controller_updated = true;
                    let Some(button) = translate_button(button) else {
                        continue;
                    };
                    // Called whenever a DPad direction is pressed or released
                    if (button == crate::options::Button::DPadLeft
                        || button == crate::options::Button::DPadUp
                        || button == crate::options::Button::DPadRight
                        || button == crate::options::Button::DPadDown)
                        && options.dpad_to_touch.is_some()
                    {
                        let Some((x, y, w, h)) = options.dpad_to_touch else {
                            unreachable!();
                        };

                        // Update held state
                        let pressed = matches!(event, E::ControllerButtonDown { .. });
                        match button {
                            crate::options::Button::DPadLeft => self.dpad_state.left = pressed,
                            crate::options::Button::DPadRight => self.dpad_state.right = pressed,
                            crate::options::Button::DPadUp => self.dpad_state.up = pressed,
                            crate::options::Button::DPadDown => self.dpad_state.down = pressed,
                            _ => unreachable!(),
                        }

                        // Compute center
                        let cx = x + w * 0.5;
                        let cy = y + h * 0.5;

                        // Compute combined delta
                        let mut dx = 0.0;
                        let mut dy = 0.0;

                        if self.dpad_state.left {
                            dx -= 0.5 * w;
                        }
                        if self.dpad_state.right {
                            dx += 0.5 * w;
                        }
                        if self.dpad_state.up {
                            dy -= 0.5 * h;
                        }
                        if self.dpad_state.down {
                            dy += 0.5 * h;
                        }

                        // Final coords: center + movement
                        let coords = transform_input_coords(self, (cx + dx, cy + dy), true);

                        // Send TouchDown if any dpad is held, TouchUp if none
                        let any_held = self.dpad_state.left
                            || self.dpad_state.right
                            || self.dpad_state.up
                            || self.dpad_state.down;

                        if !self.dpad_state.active && any_held {
                            // New touch
                            self.dpad_state.active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && any_held {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else if self.dpad_state.active && !any_held {
                            // Release touch
                            self.dpad_state.active = false;
                            Event::TouchesUp(HashMap::from([(FingerId::DpadToTouch, coords)]))
                        } else {
                            continue;
                        }
                    } else {
                        let Some(&(x, y)) = options.button_to_touch.get(&button) else {
                            continue;
                        };
                        match event {
                            E::ControllerButtonUp { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesUp(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            E::ControllerButtonDown { .. } => {
                                let coords = transform_input_coords(self, (x, y), true);
                                Event::TouchesDown(HashMap::from([(
                                    FingerId::ButtonToTouch(button),
                                    coords,
                                )]))
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                E::ControllerAxisMotion { axis, .. } => {
                    controller_updated = true;
                    let Some((x, y, w, h)) = options.stick_to_touch else {
                        continue;
                    };
                    if axis == sdl2::controller::Axis::LeftX
                        || axis == sdl2::controller::Axis::LeftY
                    {
                        let (stick_x, stick_y, _) = self.get_controller_stick(options, true);
                        let coords = transform_input_coords(
                            self,
                            (
                                x + ((stick_x + 1.0) / 2.0) * w,
                                y + ((stick_y + 1.0) / 2.0) * h,
                            ),
                            true,
                        );
                        if stick_x.abs() < options.deadzone && stick_y.abs() < options.deadzone {
                            if !self.stick_active {
                                // Ignore deadzone events when stick is inactive
                                continue;
                            } else {
                                // Release touch when stick returns to deadzone
                                self.stick_active = false;
                                Event::TouchesUp(HashMap::from([(FingerId::StickToTouch, coords)]))
                            }
                        } else if !self.stick_active {
                            // New touch
                            self.stick_active = true;
                            Event::TouchesDown(HashMap::from([(FingerId::StickToTouch, coords)]))
                        } else {
                            // Move existing touch
                            Event::TouchesMove(HashMap::from([(FingerId::StickToTouch, coords)]))
                        }
                    } else {
                        continue;
                    }
                }
                E::AppWillEnterBackground { .. } => {
                    // [MoleWorld iOS] SDL's "AppWillEnterBackground" is actually
                    // iOS `applicationWillResignActive:` — it fires for ANY
                    // resign-active, INCLUDING Control Center / home-indicator
                    // overlays where the app stays foreground and never enters
                    // the background. So this must NOT exit or gate GL; it only
                    // means "pause". The TRUE background is a separate event
                    // (AppDidEnterBackground) below.
                    log!("Received app-will-resign-active event.");
                    // [MoleWorld iOS] 不再 assert:重负载帧(好友村等)单帧 drawScene 跑很久会饿死
                    // 事件循环,iOS 的 resign→background 会在 pop_event 消费前接连到达;旧 assert 在
                    // 第二个事件上 panic = 画面定格的"彻底冻死"。改为优先级语义:resign 不覆盖已挂起
                    // 的更高优先级事件(background/terminate)。
                    if self.high_priority_event.is_none() {
                        self.high_priority_event = Some(Event::AppWillResignActive);
                    }
                    // `break` (not the old `continue` + permanent
                    // `enable_event_polling=false` latch): exit the pump for THIS
                    // call so the consumer (pop_event) delivers the high-priority
                    // event before SDL's iOS path could re-block, but let the NEXT
                    // poll pump again so we can still observe the following
                    // foreground/background lifecycle events (needed to resume).
                    break;
                }
                E::AppDidEnterBackground { .. } => {
                    // [MoleWorld iOS] iOS `applicationDidEnterBackground:` — the
                    // TRUE background. After this, ANY GL call kills the app
                    // (0x8badf00d); handled by gating GL + delivering the message.
                    log!("Received app-did-enter-background event.");
                    // background 优先级高于 resign:直接覆盖(消费侧 pop_event 取最新状态即可)。不再 assert。
                    self.high_priority_event = Some(Event::AppDidEnterBackground);
                    break;
                }
                E::AppWillEnterForeground { .. } => {
                    // [MoleWorld iOS] iOS `applicationWillEnterForeground:` —
                    // leaving the background. Ungate GL + resume.
                    log!("Received app-will-enter-foreground event.");
                    Event::AppWillEnterForeground
                }
                E::AppDidEnterForeground { .. } => {
                    // [MoleWorld iOS] SDL's "AppDidEnterForeground"
                    // (SDL_APP_DIDENTERFOREGROUND) is iOS
                    // `applicationDidBecomeActive:` — every resume (overlay
                    // dismissal AND background return).
                    log!("Received app-did-become-active event.");
                    Event::AppDidBecomeActive
                }
                E::AppTerminating { .. } => {
                    log!("Received app-will-terminate event.");
                    // terminate 优先级最高:直接覆盖。不再 assert。
                    self.high_priority_event = Some(Event::AppWillTerminate);
                    break;
                }
                E::FingerUp {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerMotion {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                }
                | E::FingerDown {
                    timestamp,
                    finger_id,
                    x,
                    y,
                    ..
                } => {
                    log_dbg!("Starting multi-touch for {:?}", event);
                    // To implement multi-touch we accumulate here same touch
                    // events at the same timestamp. This is consistent with
                    // UIKit, but could be broken if events come out of order.
                    // (in worst case we separate multi-touches in several ones)
                    // TODO: handle out of order touches
                    let curr_timestamp = timestamp;
                    let abs_coords = finger_absolute_coords(self, (x, y));
                    let coords = transform_input_coords(self, abs_coords, false);
                    log_dbg!("Finger event x {}, y {}, coords {:?}", x, y, coords);
                    let mut map = HashMap::from([(FingerId::Touch(finger_id), coords)]);
                    while let Some(next) = self.event_pump.poll_event() {
                        match next {
                            E::Unknown { .. } => (),
                            _ => log_dbg!("Next possible multi-touch event: {:?}", next),
                        }
                        match next {
                            E::FingerUp {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerMotion {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            }
                            | E::FingerDown {
                                timestamp,
                                finger_id,
                                x,
                                y,
                                ..
                            } if timestamp == curr_timestamp && next.is_same_kind_as(&event) => {
                                let abs_coords = finger_absolute_coords(self, (x, y));
                                let coords = transform_input_coords(self, abs_coords, false);
                                map.insert(FingerId::Touch(finger_id), coords);
                            }
                            E::MultiGesture { timestamp, .. } if timestamp == curr_timestamp => {
                                // TODO: handle gestures
                                continue;
                            }
                            _ => {
                                // event_pump doesn't have a method to peek on
                                // events, so we keep track of an unconsumed
                                // one from a previous loop iteration
                                assert!(previous_event.is_none());
                                previous_event = Some(next);
                                break;
                            }
                        }
                    }
                    log_dbg!("Finishing multi-touch for {:?} with {:?}", event, map);
                    match event {
                        E::FingerUp { .. } => Event::TouchesUp(map),
                        E::FingerMotion { .. } => Event::TouchesMove(map),
                        E::FingerDown { .. } => Event::TouchesDown(map),
                        _ => unreachable!(),
                    }
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::F12),
                    ..
                } => {
                    // Log this so you can tell when touchHLE has received
                    // the event but it's stuck in the queue.
                    echo!("F12 pressed, EnterDebugger event queued.");
                    Event::EnterDebugger
                }
                // [MoleWorld] T toggles the built-in debug/cheat menu — but only when
                // no text field is focused, otherwise typing a name containing 't'
                // would pop the menu instead of inserting the character.
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::T),
                    ..
                } if !MOLE_TEXT_INPUT_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) => {
                    echo!("T pressed, toggling MoleWorld debug menu.");
                    Event::ToggleMoleMenu
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Backspace),
                    ..
                } => {
                    log_dbg!("SDL TextInput Backspace");
                    Event::TextInput(TextInputEvent::Backspace)
                }
                E::KeyDown {
                    keycode: Some(sdl2::keyboard::Keycode::Return),
                    ..
                } => {
                    log_dbg!("SDL TextInput Return");
                    Event::TextInput(TextInputEvent::Return)
                }
                E::TextInput { text, .. } => {
                    log_dbg!("SDL TextInput {}", text);
                    Event::TextInput(TextInputEvent::Text(text))
                }
                _ => continue,
            })
        }

        if controller_updated {
            let (new_x, new_y, pressed, pressed_changed, moved) =
                self.update_virtual_cursor(options);
            self.event_queue
                .push_back(match (pressed, pressed_changed, moved) {
                    (true, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesDown(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (false, true, _) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesUp(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    (true, _, true) => {
                        let coords = transform_input_coords(self, (new_x, new_y), false);
                        Event::TouchesMove(HashMap::from([(FingerId::VirtualCursor, coords)]))
                    }
                    _ => return,
                });
        }
    }

    /// Pop an event from the queue (in FIFO order, except for high priority
    /// events)
    pub fn pop_event(&mut self) -> Option<Event> {
        self.high_priority_event
            .take()
            .or_else(|| self.event_queue.pop_front())
    }

    /// [MoleWorld iOS · P0] Host-side peek (does NOT consume): is a background /
    /// terminate high-priority event pending but not yet processed by the guest?
    ///
    /// The guest only gates GL on its OWN `-[UIApplication did_enter_background]`,
    /// which runs between guest frames (NSRunLoop iteration). During a heavy frame
    /// the guest can't reach that, so if iOS backgrounds us mid-frame, any later
    /// present/GL call touches the GPU on a backgrounded surface → 0x8badf00d kill.
    /// `Environment::run` peeks this at each tick-batch boundary and gates GL host-
    /// side immediately, without waiting for the guest to consume the event.
    pub fn background_or_terminate_pending(&self) -> bool {
        matches!(
            self.high_priority_event,
            Some(Event::AppDidEnterBackground) | Some(Event::AppWillTerminate)
        )
    }

    fn controller_added(&mut self, joystick_idx: u32) {
        let Ok(controller) = self.controller_ctx.open(joystick_idx) else {
            log!("Warning: A new controller was connected, but it couldn't be accessed!");
            return;
        };

        let controller_name = controller.name();
        if env::consts::OS == "android" && controller_name.starts_with("uinput-") {
            log!("ignoring fingerprint device: {}", controller_name);
            return;
        }
        log!(
            "New controller connected: {}. Left stick = device tilt. Right stick = touch input (press the stick or shoulder button to tap/hold).",
            controller_name
        );
        self.controllers.push(controller);
    }
    fn controller_removed(&mut self, instance_id: u32) {
        let Some(idx) = self
            .controllers
            .iter()
            .position(|controller| controller.instance_id() == instance_id)
        else {
            return;
        };
        let controller = self.controllers.remove(idx);
        log!("Warning: Controller disconnected: {}", controller.name());
    }
    pub fn print_accelerometer_notice(&self, options: &Options) {
        log!("This app uses the accelerometer.");

        if !self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Your connected controller's left analog stick will be used for accelerometer simulation.");
            if self.accelerometer.is_some() {
                log!("Disconnect the controller if you want to use your device's accelerometer.");
            }
        } else if self.accelerometer.is_some() {
            log!("Your device's accelerometer will be used for accelerometer simulation.");
            if options.analog_stick_tilt_controls {
                log!("Connect a controller if you would prefer to use an analog stick.");
            }
        } else if self.controllers.is_empty() && options.analog_stick_tilt_controls {
            log!("Connect a controller to get accelerometer simulation.");
        }

        if self.accelerometer.is_none() {
            log!(
                "You can {}hold right click and move the cursor to simulate the accelerometer.",
                if options.analog_stick_tilt_controls {
                    "also "
                } else {
                    ""
                }
            );
        }
    }

    /// Get the real or simulated accelerometer output.
    /// See also [crate::frameworks::uikit::ui_accelerometer].
    pub fn get_acceleration(&self, options: &Options) -> (f32, f32, f32) {
        if self.controllers.is_empty() || !options.analog_stick_tilt_controls {
            if let Some(ref accelerometer) = self.accelerometer {
                let data = accelerometer.get_data().unwrap();
                let sdl2::sensor::SensorData::Accel(data) = data else {
                    panic!();
                };
                let [x, y, z] = data;
                // UIAcceleration reports acceleration towards gravity, but SDL2
                // reports acceleration away from gravity.
                let (x, y, z) = (-x, -y, -z);
                // UIAcceleration reports acceleration in units of g-force, but
                // SDL2 reports acceleration in units of m/s^2.
                let gravity: f32 = 9.80665; // SDL_STANDARD_GRAVITY
                let (x, y, z) = (x / gravity, y / gravity, z / gravity);
                return (x, y, z);
            }
        }

        let (x, y) = if self
            .virtual_accelerometer_last
            .is_some_and(|(_x, _y, right_click_hold)| right_click_hold)
        {
            self.virtual_accelerometer_last
                .map(|(x, y, _right_click_hold)| (x, y))
                .unwrap()
        } else {
            // Get left analog stick input. The range is [-1, 1] on each axis.
            let (x, y, _) = self.get_controller_stick(options, true);
            (x, y)
        };

        // Correct for window rotation
        let [x, y] = self.rotation_matrix().inverse().unwrap().transform([x, y]);
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0)); // just in case

        // Let's simulate tilting the device based on the analog stick inputs.
        //
        // If an iPhone is lying flat on its back, level with the ground, and it
        // is on Earth, the accelerometer will report approximately (0, 0, -1).
        // The acceleration x and y axes are aligned with the screen's x and y
        // axes. +x points to the right of the screen, +y points to the top of
        // the screen, and +z points away from the screen. In the example
        // scenario, the z axis is parallel to gravity.

        let gravity: [f32; 3] = [0.0, 0.0, -1.0];

        let neutral_x = options.x_tilt_offset.to_radians();
        let neutral_y = options.y_tilt_offset.to_radians();
        let x_rotation_range = options.x_tilt_range.to_radians() / 2.0;
        let y_rotation_range = options.y_tilt_range.to_radians() / 2.0;
        // (x, y) are swapped because the controller Y axis usually corresponds
        // to forward/backward movement, but rotating about the Y axis means
        // tilting the device left/right.
        let x_rotation = neutral_x - x_rotation_range * y;
        let y_rotation = neutral_y - y_rotation_range * x;
        let matrix =
            Matrix::<3>::y_rotation(y_rotation).multiply(&Matrix::<3>::x_rotation(x_rotation));
        let [x, y, z] = matrix.transform(gravity);

        (x, y, z)
    }

    /// For use when redrawing the screen: Get the cached on-screen position and
    /// press state of the analog stick-controlled virtual cursor, if it is
    /// visible.
    pub fn virtual_cursor_visible_at(&self) -> Option<(f32, f32, bool)> {
        let (x, y, pressed, visible) = self.virtual_cursor_last?;
        if visible {
            // When stickyness is in use, the visual cursor movement appears
            // uncomfortably choppy. Showing the un-sticky position is a bit
            // misleading but it *feels* better, and it is documented.
            if let Some((x_unsticky, y_unsticky, _time)) = self.virtual_cursor_last_unsticky {
                Some((x_unsticky, y_unsticky, pressed))
            } else {
                Some((x, y, pressed))
            }
        } else {
            None
        }
    }

    /// Update the virtual cursor's position, click state and visibility, then
    /// return the new position, pressed state, whether the press state changed
    /// and whether the cursor moved.
    fn update_virtual_cursor(&mut self, options: &Options) -> (f32, f32, bool, bool, bool) {
        // Get right analog stick input. The range is [-1, 1] on each axis.
        let (x, y, pressed) = self.get_controller_stick(options, false);

        // The cursor is intended to only show up once you move the analog stick
        // out of its deadzone, or while the button is held.
        let visible = pressed || x != 0.0 || y != 0.0;

        // Though the analog stick output fits within a square, its actual range
        // is usually a circle enclosed by the square. So we need to cut out the
        // rectangular shape of the screen from that circle within the square.
        let (vx, vy, vw, vh) = self.viewport();
        let (vx, vy, vw, vh) = (vx as f32, vy as f32, vw as f32, vh as f32);

        let (x, y) = {
            // Use Pythagoras's theorem to find the largest size the rectangle
            // can have within the circle.
            let ratio = vw / vh;
            let rect_height = (ratio * ratio + 1.0).powf(-0.5);
            let rect_width = ratio * rect_height;

            let x_abs = x.abs().min(rect_width) / rect_width;
            let y_abs = y.abs().min(rect_height) / rect_height;
            (x_abs.copysign(x), y_abs.copysign(y))
        };

        // Convert to on-screen window co-ordinates
        let x = (x / 2.0 + 0.5) * vw + vx;
        let y = (y / 2.0 + 0.5) * vh + vy;

        let (old_x, old_y, old_pressed, _old_visible) =
            self.virtual_cursor_last.unwrap_or_default();

        let (x, y) = if let Some((smoothing_strength, sticky_radius)) =
            options.stabilize_virtual_cursor
        {
            let new_time = Instant::now();

            let (old_x_unsticky, old_y_unsticky, old_time) = self
                .virtual_cursor_last_unsticky
                .unwrap_or((0.0, 0.0, new_time));

            let delta_t = new_time.saturating_duration_since(old_time).as_secs_f32();

            // Apply a feedback-based smoothing with exponential decay, to try
            // to dampen shakiness in the stick movement.

            let smooth = |old: f32, new: f32| -> f32 {
                if smoothing_strength != 0.0 {
                    let lerp_factor = 1.0 - (0.5_f32).powf(delta_t * (1.0 / smoothing_strength));
                    old + (new - old) * lerp_factor
                } else {
                    new
                }
            };

            let new_x_unsticky = smooth(old_x_unsticky, x);
            let new_y_unsticky = smooth(old_y_unsticky, y);

            self.virtual_cursor_last_unsticky = Some((new_x_unsticky, new_y_unsticky, new_time));

            // Make the reported position "sticky" within a certain radius, i.e.
            // if the new position's distance from the old one is within the
            // radius, report no change in position.

            if (new_x_unsticky - old_x).hypot(new_y_unsticky - old_y) < sticky_radius {
                (old_x, old_y)
            } else {
                (new_x_unsticky, new_y_unsticky)
            }
        } else {
            (x, y)
        };

        self.virtual_cursor_last = Some((x, y, pressed, visible));

        (
            x,
            y,
            pressed,
            pressed != old_pressed,
            x != old_x || y != old_y,
        )
    }

    /// Get the summed X and Y positions and button state of the left or right
    /// analog stick of the game controllers. Each axis value is in the range
    /// [-1, 1].
    fn get_controller_stick(&self, options: &Options, left: bool) -> (f32, f32, bool) {
        fn convert_axis(axis: i16, deadzone: f32) -> f32 {
            assert!(deadzone >= 0.0);
            let axis = ((axis as f32) / (i16::MAX as f32)).clamp(-1.0, 1.0);
            let abs_axis = (axis.abs().max(deadzone) - deadzone) / (1.0 - deadzone);
            abs_axis.copysign(axis)
        }

        let (mut x, mut y) = (0.0, 0.0);
        let mut pressed = false;
        for controller in &self.controllers {
            use sdl2::controller::{Axis, Button};
            let (x_axis, y_axis, button1, button2) = if left {
                (
                    Axis::LeftX,
                    Axis::LeftY,
                    Button::LeftStick,
                    Button::LeftShoulder,
                )
            } else {
                (
                    Axis::RightX,
                    Axis::RightY,
                    Button::RightStick,
                    Button::RightShoulder,
                )
            };
            x += convert_axis(controller.axis(x_axis), options.deadzone);
            y += convert_axis(controller.axis(y_axis), options.deadzone);
            pressed |= controller.button(button1);
            pressed |= controller.button(button2);
        }
        let (x, y) = (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0));

        (x, y, pressed)
    }

    pub fn create_gl_context(&self, version: GLVersion) -> Result<GLContext, String> {
        let attr = self.video_ctx.gl_attr();
        match version {
            GLVersion::GLES11 => {
                attr.set_context_version(1, 1);
                attr.set_context_profile(sdl2::video::GLProfile::GLES);
            }
            GLVersion::GL21Compat => {
                attr.set_context_version(2, 1);
                attr.set_context_profile(sdl2::video::GLProfile::Compatibility);
            }
        }

        let gl_ctx = self.window.gl_create_context()?;

        // [MoleWorld] macOS 26 的窗口服务器对【未同步上屏】的 GL 窗口(尤其独立 Space + 高频刷新)
        // 会合成出闪烁(渲染内容平滑、上屏却闪)。MOLE_VSYNC 让 swap 同步到显示器刷新来消除:
        // 1=VSync(同步),2=自适应撕裂同步(LateSwapTearing),其它/不设=保持原行为(Immediate)。
        // env 门控、便于 A/B,不改默认行为(gl_create_context 后上下文已 current,可设 swap interval)。
        if let Ok(mode) = std::env::var("MOLE_VSYNC") {
            use sdl2::video::SwapInterval;
            let (si, name) = match mode.as_str() {
                "1" => (SwapInterval::VSync, "VSync"),
                "2" => (SwapInterval::LateSwapTearing, "LateSwapTearing(自适应)"),
                _ => (SwapInterval::Immediate, "Immediate"),
            };
            match self.video_ctx.gl_set_swap_interval(si) {
                Ok(()) => {
                    log!("[MoleWorld] gl_set_swap_interval={} 已应用 (MOLE_VSYNC={})", name, mode);
                }
                Err(e) => {
                    log!("[MoleWorld] gl_set_swap_interval={} 失败: {}", name, e);
                }
            }
        }

        Ok(GLContext(gl_ctx))
    }

    pub fn gl_get_proc_address(&self, procname: &str) -> *const std::ffi::c_void {
        // For some reason, rust-sdl2 uses *const (), but () is not meant to be
        // used for void pointees (just void results), so let's fix that.
        Self::gl_proc_ios_fallback(
            self.video_ctx.gl_get_proc_address(procname) as *const _,
            procname,
        )
    }

    /// [MoleWorld iOS] iOS 上的 GL 符号解析:**不信任 SDL_GL_GetProcAddress 的返回,一律
    /// 优先从 OpenGLES.framework 解析。** 实测在 PlayCover / iOS-on-Mac 进程里,桌面
    /// OpenGL.framework(libGL.dylib)被加载且导出同名 glGenTextures/glGetString:SDL 对
    /// 一部分函数(如 glGenTextures)返回的正是【桌面 GL】的指针(addr 非空,它要 CGL/NSOpenGL
    /// 上下文,而我们建的是 GLES/EAGLContext → 调用即崩),对另一些(如 glGetString)返回
    /// NULL。所以「仅在 addr 为空时才回退」是不够的——glGenTextures 这种 addr 非空但指向桌面
    /// GL 的会漏网。改为:只要 OpenGLES.framework 有该符号就用它(覆盖 SDL 的桌面指针),
    /// OpenGLES 没有时才回落到 SDL 的 addr / RTLD_DEFAULT。仅编进 iOS target,桌面/Android
    /// 走原生 SDL(返回 addr,本函数整体不参与)。
    fn gl_proc_ios_fallback(
        addr: *const std::ffi::c_void,
        procname: &str,
    ) -> *const std::ffi::c_void {
        #[cfg(target_os = "ios")]
        if let Ok(cname) = std::ffi::CString::new(procname) {
            unsafe {
                // 优先从 OpenGLES.framework 专属 handle 解析(避开桌面 OpenGL.framework 同名符号)。
                let gles_path = b"/System/Library/Frameworks/OpenGLES.framework/OpenGLES\0"
                    .as_ptr() as *const libc::c_char;
                let mut gles = libc::dlopen(gles_path, libc::RTLD_NOLOAD | libc::RTLD_LAZY);
                if gles.is_null() {
                    gles = libc::dlopen(gles_path, libc::RTLD_LAZY);
                }
                if !gles.is_null() {
                    let sym = libc::dlsym(gles, cname.as_ptr());
                    if !sym.is_null() {
                        return sym as *const std::ffi::c_void;
                    }
                }
                // OpenGLES 无该符号:SDL 的 addr 非空则用它,否则 RTLD_DEFAULT 兜底。
                if !addr.is_null() {
                    return addr;
                }
                let sym = libc::dlsym(libc::RTLD_DEFAULT, cname.as_ptr());
                if !sym.is_null() {
                    return sym as *const std::ffi::c_void;
                }
            }
        }
        addr
    }

    pub fn set_share_with_current_context(&self, value: bool) {
        self.video_ctx
            .gl_attr()
            .set_share_with_current_context(value)
    }

    pub unsafe fn make_gl_context_current(&self, gl_ctx: &GLContext) {
        self.window.gl_make_current(&gl_ctx.0).unwrap();
    }

    /// Make the internal OpenGL ES context (for splash screen and UI rendering)
    /// current.
    #[must_use]
    pub fn make_internal_gl_ctx_current<'win>(&'win mut self) -> Box<dyn GLES + 'win> {
        // The invariant is held up here - since the instance we return is
        // bound to the lifetime of window, it can't outlive the internal GL
        // context and can't outlive the window.
        let gl_ins = unsafe {
            self.internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| {
                        Self::gl_proc_ios_fallback(
                            self.video_ctx.gl_get_proc_address(s) as *const _,
                            s,
                        )
                    },
                )
        };
        gl_ins
    }

    fn display_splash(&mut self) {
        assert!(self.splash_image.is_some());

        // OpenGL ES expects bottom-to-top row order for image data, but our
        // image data will be top-to-bottom. A reflection transform compensates.
        let matrix = self.rotation_matrix().multiply(&Matrix::y_flip());
        let (vx, vy, vw, vh) = self.viewport();
        let viewport = (vx, vy + self.viewport_y_offset(), vw, vh);

        let image = self.splash_image.as_ref().unwrap();
        let window_fbo = self.default_framebuffer();
        let window_rbo = self.default_renderbuffer();
        // [MoleWorld 智能分辨率] 完整 drawable 尺寸,供 present_frame 的 --ambient-fill。
        let full_size = self.window.drawable_size();

        unsafe {
            let mut gl_ctx = self
                .internal_gl_ins
                .as_mut()
                .unwrap()
                .make_current_unchecked_for_window(
                    &mut |gl_ctx| self.window.gl_make_current(&gl_ctx.0).unwrap(),
                    &mut |s| {
                        Self::gl_proc_ios_fallback(
                            self.video_ctx.gl_get_proc_address(s) as *const _,
                            s,
                        )
                    },
                );

            use crate::gles::gles11_raw as gles11; // constants only
            log!("[splash] GL context current (default VAO ensured); uploading splash texture");

            let mut texture = 0;
            gl_ctx.GenTextures(1, &mut texture);
            gl_ctx.BindTexture(gles11::TEXTURE_2D, texture);
            let (width, height) = image.dimensions();
            gl_ctx.TexImage2D(
                gles11::TEXTURE_2D,
                0,
                gles11::RGBA as _,
                width as _,
                height as _,
                0,
                gles11::RGBA,
                gles11::UNSIGNED_BYTE,
                image.pixels().as_ptr() as *const _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MIN_FILTER,
                gles11::LINEAR as _,
            );
            gl_ctx.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MAG_FILTER,
                gles11::LINEAR as _,
            );
            // [MoleWorld iOS] The splash texture is NPOT (image-sized). iOS
            // native GLES1 requires CLAMP_TO_EDGE wrap for NPOT textures to be
            // complete; the default GL_REPEAT leaves it incomplete and the
            // textured present draws solid white (the "白色闪一下" the device
            // shows). Harmless on desktop. See composition.rs for the full note.
            // [MoleWorld] CLAMP only on iOS; Mac keeps REPEAT (default). present_frame on
            // Mac rotates texcoords via the TEXTURE matrix outside [0,1] where REPEAT wraps
            // correctly and CLAMP_TO_EDGE would smear the splash into vertical bands.
            #[cfg(target_os = "ios")]
            {
                gl_ctx.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_S,
                    gles11::CLAMP_TO_EDGE as _,
                );
                gl_ctx.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_T,
                    gles11::CLAMP_TO_EDGE as _,
                );
            }

            log!("[splash] texture ready; calling present_frame (first GL draw / DrawArrays)");
            present_frame(
                gl_ctx.as_mut(),
                viewport,
                full_size,
                matrix,
                /* virtual_cursor_visible_at: */ None,
                window_fbo,
            );
            log!("[splash] present_frame returned OK");
            // [MoleWorld iOS] swap 前把 viewRenderbuffer 绑回 GL_RENDERBUFFER(SDL presentRenderbuffer 契约)。
            #[cfg(target_os = "ios")]
            gl_ctx.BindRenderbufferOES(gles11::RENDERBUFFER_OES, window_rbo);

            gl_ctx.DeleteTextures(1, &texture);
        };

        self.window.gl_swap_window();
        log!("[splash] gl_swap_window done — splash displayed");

        // hold onto GL context so the image doesn't disappear, and hold
        // onto image so we can rotate later if necessary
    }

    /// Swap front-buffer and back-buffer so the result of OpenGL rendering is
    /// presented.
    /// [MoleWorld iOS] 窗口的默认 framebuffer(见字段注释)。供 present_frame 绘制前绑定。
    pub fn default_framebuffer(&self) -> crate::gles::gles11_raw::types::GLuint {
        self.default_framebuffer
    }

    /// [MoleWorld iOS] 窗口的默认 viewRenderbuffer(见字段注释)。各 present 路径 swap 前绑回。
    pub fn default_renderbuffer(&self) -> crate::gles::gles11_raw::types::GLuint {
        self.default_renderbuffer
    }

    /// [MoleWorld iOS] See the `backgrounded` field. While true, every GL /
    /// present path must early-out so we never touch the GPU in the true
    /// background (iOS kills any app that does).
    pub fn is_backgrounded(&self) -> bool {
        self.backgrounded
    }
    pub fn set_backgrounded(&mut self, value: bool) {
        crate::mole_watchdog::BACKGROUNDED.store(value, std::sync::atomic::Ordering::Relaxed);
        if self.backgrounded != value {
            log!(
                "[MoleWorld iOS] backgrounded = {} (GL gate {})",
                value,
                if value { "ON" } else { "OFF" }
            );
        }
        self.backgrounded = value;
    }

    pub fn swap_window(&self) {
        // [MoleWorld iOS] Never flush to the GPU while truly backgrounded — a
        // background present is a guaranteed 0x8badf00d kill by iOS. (Backstop;
        // the present entry points already early-out before emitting any GL.)
        if self.backgrounded {
            return;
        }
        self.window.gl_swap_window();
    }

    /// Consider the emulated device to be rotated to a particular orientation.
    ///
    /// On a PC or laptop, this will make the window be rotated so the app
    /// content appears upright. On a mobile device, this might do something
    /// else, because the user can physically rotate the screen.
    pub fn rotate_device(&mut self, new_orientation: DeviceOrientation) {
        assert!(self.on_main_stack);
        if new_orientation == self.device_orientation {
            return;
        }

        if !self.fullscreen && !Self::rotatable_fullscreen() {
            let (width, height) = if Self::rotatable_fullscreen() {
                set_sdl2_orientation(new_orientation);
                rotate_fullscreen_size(new_orientation, self.window.size())
            } else {
                size_for_orientation(self.device_family, new_orientation, self.scale_hack)
            };

            // macOS quirk: when resizing the window, the new framebuffer's size
            // is apparently max(new_size, old_size) in each dimension, but the
            // viewport is positioned wrong on the y axis for some reason, so we
            // need to apply an offset.
            // Recreating the OpenGL context was an alternative workaround, but
            // that apparently stops other OpenGL contexts drawing to the
            // framebuffer!
            #[cfg(target_os = "macos")]
            {
                let (_old_width, old_height) = self.window.size();
                self.max_height = self.max_height.max(old_height).max(height);
                self.viewport_y_offset = self.max_height - height;
            }

            self.window.set_size(width, height).unwrap();
        }

        if Self::rotatable_fullscreen() {
            set_sdl2_orientation(new_orientation);
            // Hack: from reading SDL2's source code, it seems that SDL2 will
            // only re-do the orientation when changing whether a window is
            // "resizeable" (can be rotated). You can't set the resizeable state
            // on a fullscreen window, so it must be temporarily stop being
            // fulscreen.
            // Apparently, doing this does result in resizing the window.
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::Off)
                .unwrap();
            unsafe {
                let window_raw = self.window.raw();
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_FALSE);
                sdl2_sys::SDL_SetWindowResizable(window_raw, sdl2_sys::SDL_bool::SDL_TRUE);
            }
            self.window
                .set_fullscreen(sdl2::video::FullscreenType::True)
                .unwrap();
        }

        self.device_orientation = new_orientation;

        if self.splash_image.is_some() {
            self.display_splash();
        }
    }

    pub fn device_family(&self) -> DeviceFamily {
        self.device_family
    }

    /// Returns the current device orientation
    pub fn current_rotation(&self) -> DeviceOrientation {
        self.device_orientation
    }

    /// Get the size in pixels of the window without rotation or scaling.
    ///
    /// The aspect ratio, scale and orientation reflect the guest app's view of
    /// the world.
    pub fn size_unrotated_unscaled(&self) -> (u32, u32) {
        size_for_orientation(
            self.device_family,
            DeviceOrientation::Portrait,
            NonZeroU32::new(1).unwrap(),
        )
    }

    /// [MoleWorld 智能分辨率] 完整 drawable 尺寸(整个窗口/全屏区,像素)。present 传给
    /// present_frame 判断 letterbox 空白、做 --ambient-fill 环境补边。
    pub fn drawable_size(&self) -> (u32, u32) {
        self.window.drawable_size()
    }

    /// Get the region of the on-screen window (x, y, width, height) used to
    /// display the app content.
    ///
    /// The aspect ratio of this region always reflects the guest app's view of
    /// the world, but the scale and orientation might not.
    pub fn viewport(&self) -> (u32, u32, u32, u32) {
        let (app_width, app_height) =
            size_for_orientation(self.device_family, self.device_orientation, self.scale_hack);
        let (screen_width, screen_height) = self.window.drawable_size();
        // [MoleWorld VPDIAG] 拖动错位回归排查:打印 app/drawable/窗口尺寸 + viewport_y_offset。
        // render 用 viewport()+yoff,touch(transform_input_coords)用 viewport() 不加 yoff;
        // 若 yoff≠0(启动时被 SizeChanged 置非零)→ render 偏移而 touch 不偏移 = 错位根因。
        // macOS-only:max_height / viewport_y_offset 字段是 #[cfg(target_os="macos")]
        // (窗口可拖动缩放才有意义);iOS 全屏无窗口拖动,该诊断不适用,gate 掉以修复 iOS 构建。
        #[cfg(target_os = "macos")]
        {
            use std::sync::atomic::{AtomicU32, Ordering};
            static N: AtomicU32 = AtomicU32::new(0);
            if N.fetch_add(1, Ordering::Relaxed) % 180 == 0 {
                log!(
                    "[VPDIAG] app=({},{}) drawable=({},{}) winsize={:?} yoff={} max_h={} fullscreen={} scale_hack={}",
                    app_width, app_height, screen_width, screen_height,
                    self.window.size(), self.viewport_y_offset, self.max_height,
                    self.fullscreen, self.scale_hack
                );
            }
        }

        // [MoleWorld] 「自由铺满」分支(返回整个 drawable,无 letterbox):
        //   (a) 窗口模式 + 无定制 guest 逻辑屏 = 旧默认「自由调节适配屏幕拉伸」,drawable==app 原生
        //       尺寸时逐字节等同旧行为 → 零回归;
        //   (b) ★有定制 guest 逻辑屏时(--fill-screen / --logical-size / MOLE_FILL / MOLE_GUEST_PORTRAIT)
        //       也走这里【无条件铺满、绝不 letterbox】——因为窗口已被 resize 事件钉死在 guest 比例
        //       (见 poll_for_events 的 E::Window 分支,custom_guest_size_active() 触发锁比例),且 fullscreen
        //       下 --fill-screen 的 guest 比例=屏比例 → 铺满即等比、不变形、【永远无黑边(连拖拽瞬间都不闪)】。
        //       这是用户要的「无级调节、无黑边、不拉伸」:拖窗口=无级改大小、恒填满;换比例需重启由
        //       --fill-screen 按新屏重算(cocos2d-iphone v1 无 reshape 派发,不能运行时改 guest 逻辑屏重排)。
        // 仅【全屏/rotatable-fullscreen 且非定制尺寸】才落到下面的等比 letterbox(原生行为,不回归)。
        // [MoleWorld 智能分辨率]「4:3 完美模式」--ambient-fill(仅对非定制尺寸=原生 4:3 生效):强制
        // 走下面的等比 letterbox(不 free-stretch),这样窗口/全屏下 4:3 都居中不变形、露出 letterbox
        // 空白供 present 做环境补边。定制尺寸(--fill-screen)永不 ambient(它本就铺满无空白)。
        let custom = custom_guest_size_active();
        let ambient = ambient_fill_active() && !custom;
        // [MoleWorld iOS] iOS 是全屏设备、窗口不可拖拽:非定制尺寸时必须保持游戏原 4:3 等比(两侧 letterbox,
        // 不能拉伸变形),所以【只有】定制尺寸(--fill-screen 已把 guest 比例算成≈屏比例)才走铺满分支。
        #[cfg(target_os = "ios")]
        let free_stretch = custom && !ambient;
        #[cfg(not(target_os = "ios"))]
        let free_stretch = ((!self.fullscreen && !Self::rotatable_fullscreen()) || custom) && !ambient;
        if free_stretch {
            return (0, 0, screen_width, screen_height);
        }

        let app_aspect = app_width as f32 / app_height as f32;
        let screen_aspect = screen_width as f32 / screen_height as f32;
        let (scaled_width, scaled_height) = if app_aspect < screen_aspect {
            (
                (screen_height as f32 * app_aspect).round() as u32,
                screen_height,
            )
        } else {
            (
                screen_width,
                (screen_width as f32 / app_aspect).round() as u32,
            )
        };
        let x = (screen_width - scaled_width) / 2;
        let y = (screen_height - scaled_height) / 2;
        (x, y, scaled_width, scaled_height)
    }

    /// Special offset to add to y co-ordinates, only when drawing to screen.
    pub fn viewport_y_offset(&self) -> u32 {
        #[cfg(target_os = "macos")]
        return self.viewport_y_offset;
        #[cfg(not(target_os = "macos"))]
        return 0;
    }

    /// Transformation matrix for transforming between the window's co-ordinate
    /// space and the app's original co-ordinate space when rotation is in use
    /// (see [Self::rotate_device]). This returns a matrix appropriate for
    /// rotating texture co-ordinates to display the image in the window; when
    /// rotating input co-ordinates, invert the matrix.
    pub fn rotation_matrix(&self) -> Matrix<2> {
        match self.device_orientation {
            DeviceOrientation::Portrait => Matrix::identity(),
            DeviceOrientation::PortraitUpsideDown => Matrix::z_rotation(PI),
            DeviceOrientation::LandscapeLeft => Matrix::z_rotation(-FRAC_PI_2),
            DeviceOrientation::LandscapeRight => Matrix::z_rotation(FRAC_PI_2),
        }
    }

    pub fn is_screen_saver_enabled(&self) -> bool {
        self.video_ctx.is_screen_saver_enabled()
    }
    pub fn set_screen_saver_enabled(&mut self, enabled: bool) {
        assert!(self.on_main_stack);
        match enabled {
            true => self.video_ctx.enable_screen_saver(),
            false => self.video_ctx.disable_screen_saver(),
        }
    }

    pub fn start_text_input(&self) {
        assert!(self.on_main_stack);
        // [MoleWorld] 标记文本输入激活:让物理 T 键在编辑时当普通字符(见 poll_for_events)。
        MOLE_TEXT_INPUT_ACTIVE.store(true, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            sdl2_sys::SDL_StartTextInput();
        }
    }
    pub fn stop_text_input(&self) {
        assert!(self.on_main_stack);
        MOLE_TEXT_INPUT_ACTIVE.store(false, std::sync::atomic::Ordering::Relaxed);
        unsafe {
            sdl2_sys::SDL_StopTextInput();
        }
    }

    pub fn on_main_stack(&self) -> bool {
        self.on_main_stack
    }
}

pub fn open_url(env: &mut Environment, url: &str) -> Result<(), String> {
    env.on_parent_stack_in_coroutine(|_, _| sdl2::url::open_url(url).map_err(|e| e.to_string()))
}

/// Show an SDL messagebox for an error (typically after a panic).
///
/// The window argument allows for passing in the parent window for the
/// messagebox, which is not required but should be done if possible.
pub fn show_error_messagebox(window: Option<&Window>, error_message: &str) {
    assert!(window.is_none_or(|win| win.on_main_stack));
    use sdl2::messagebox;
    let mbox = [
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 0,
            text: "Open touchHLE directory",
        },
        messagebox::ButtonData {
            flags: messagebox::MessageBoxButtonFlag::NOTHING,
            button_id: 1,
            text: "Close",
        },
    ];

    let Ok(clicked_button) = messagebox::show_message_box(
        messagebox::MessageBoxFlag::ERROR,
        &mbox,
        "touchHLE crashed!",
        &format!("touchHLE crashed with the following error: {error_message}"),
        window.map(|win| &win.window),
        None,
    ) else {
        panic!("Failed to show message box!");
    };

    match clicked_button {
        messagebox::ClickedButton::CloseButton => {}
        messagebox::ClickedButton::CustomButton(button) => {
            match button.button_id {
                // Open data directory (contains log file on android)
                0 => match crate::paths::url_for_opening_user_data_dir() {
                    Ok(url) => {
                        if let Err(e) = sdl2::url::open_url(&url).map_err(|e| e.to_string()) {
                            echo!("Couldn't open file manager at {:?}: {}", url, e);
                        } else {
                            echo!("Opened file manager at {:?}, exiting.", url);
                        }
                    }
                    Err(e) => echo!("Couldn't open file manager: {}", e),
                },
                // Close
                1 => {}
                _ => unreachable!(),
            }
        }
    }
}

/// Get current battery state from SDL2.
///
/// Returns:
/// - pct: i32 - percentage of battery remaining.
/// - status: [BatteryState] - the current status of the battery
///   (unplugged, charging, full, etc.)
pub fn get_battery_status() -> (i32, BatteryState) {
    let mut pct = 0;
    // Unfortunately, Rust-SDL2 does not expose this function yet.
    // iPhoneOS does not measure the battery in seconds remaining,
    // so we discard this argument.
    let status = unsafe { sdl2_sys::SDL_GetPowerInfo(null_mut(), &mut pct) };
    (
        pct,
        match status {
            SDL_PowerState::SDL_POWERSTATE_UNKNOWN => BatteryState::Unknown,
            SDL_PowerState::SDL_POWERSTATE_ON_BATTERY => BatteryState::OnBattery,
            SDL_PowerState::SDL_POWERSTATE_NO_BATTERY => BatteryState::NoBattery,
            SDL_PowerState::SDL_POWERSTATE_CHARGING => BatteryState::Charging,
            SDL_PowerState::SDL_POWERSTATE_CHARGED => BatteryState::Full,
        },
    )
}

pub fn get_preferred_language_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .map(|loc| loc.lang)
            .collect()
    })
}

pub fn get_preferred_country_codes(env: &mut Environment) -> Vec<String> {
    env.on_parent_stack_in_coroutine(|_, _| {
        sdl2::locale::get_preferred_locales()
            .filter_map(|loc| loc.country)
            .collect()
    })
}
