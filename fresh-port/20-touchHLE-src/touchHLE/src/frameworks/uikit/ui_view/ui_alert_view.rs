/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIAlertView`.
//!
//! [扫描修 2026-09-15] F12-1 / F11-6:以前这里是"无界面"实现——show 后约 1 帧自动按索引 0
//! 回调,玩家看不到任何系统弹框文案(首次欢迎、坏档说明、联机公告/挤号/封号/网络错误……),
//! 凡是"索引 0 不是安全选项"的框也被替玩家做了决定(例如 Porter/NewScenePorter/WrapperManager/
//! AppDelegate 的 tag 框,index 0 就是 exit(0);MainMenuScene 关框即重连,等于无退避自动重连)。
//!
//! 现在的做法:
//! - `show` 只把弹框压进 Rust 队列(持有一次 retain),同一时刻只显示队首一个(防叠加);
//!   由 [pump](uikit::handle_events 每轮运行循环调用)挂到 keyWindow 最上层。
//!   keyWindow 还没建好、或作弊菜单开着时继续排队,下一帧再试。
//! - 覆盖层 = 独立的全屏 UIView(半透明遮罩)+ 边框/底板 + 标题/正文 UILabel + 按钮条。
//!   容器的旋转与居中由窗口旋转矩阵推出:--landscape-right 下正好等于 mole_menu 的 -90° 写法,
//!   --landscape-left / 竖屏 / --fill-screen 宽屏(portrait_size 被覆盖)也自洽。
//!   刻意【不】把 UIAlertView 自己挂进 keyWindow:iOS 4+ 弹框在独立的 _UIAlertOverlayWindow 里,
//!   而游戏的 -[LoadingManager exitLoading]@0x2382f0 会遍历 keyWindow.subviews 找 UIAlertView 并
//!   dismiss + release;真机(MinimumOSVersion 4.3)上找不到,我们若挂进去就会被多 release 一次。
//! - 触摸由 uikit.rs 在交给游戏前先过 [filter_touch_event](仿 mole_menu::is_open 的改道,
//!   不依赖 UIButton 目标-动作链):按下落在弹框上就吞掉整次触摸,松手时仍在同一按钮内才算点击
//!   (UIKit 的 touch-up-inside)。[复核修 2026-09-15] R1-1/R1-2/R1-3:按手指记账——只有按下时被弹框
//!   吞掉的手指归弹框(move/up/cancel 都吞),其余手指照常交给游戏;只有做点击判定的那根手指抬起才算点击;
//!   滚轮虚拟捏合手指(PinchA/PinchB)只吞不点;取消事件一律不点。
//! - 点击按真实索引依次回调 alertView:clickedButtonAtIndex: → alertView:willDismissWithButtonIndex:
//!   → alertView:didDismissWithButtonIndex:;`dismissWithClickedButtonIndex:animated:` 只回调后两个
//!   (与 UIKit 一致)并拆掉视图。
//! - 保留旧行为:无头模式或 MOLE_ALERT_AUTODISMISS=1 时仍"约 1 帧后按索引 0 自动关闭"(无头测试用)。
//! - GameData 的存档校验框(-[GameData alertView:clickedButtonAtIndex:]@0x754b4 = exit(0),
//!   由 -[GameData loadUserInfoData] +0x298/+0x3c8 弹出)不进队列:此时游戏带着空 UserInfoData
//!   继续跑,等玩家点确定的这段时间可能自动存档把好档覆盖掉(见 mole_cheats.rs #3 注释)。
//!   所以它保持旧的"约 1 帧后回调索引 0"时序不变,只在回调前用宿主原生【阻塞】消息框把标题/正文
//!   给玩家看(阻塞期间游戏不前进),点掉后照原版 exit(0)。

use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string;
use crate::frameworks::foundation::NSInteger;
use crate::frameworks::uikit::ui_font::{
    UILineBreakMode, UILineBreakModeCharacterWrap, UILineBreakModeWordWrap, UITextAlignmentCenter,
};
use crate::impl_HostObject_with_superclass;
use crate::objc::{
    id, msg, msg_class, msg_super, nil, objc_classes, release, retain, ClassExports, NSZonePtr,
    ObjC,
};
use crate::window::{Coords, Event, FingerId};
use crate::Environment;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

pub(crate) struct UIAlertViewHostObject {
    superclass: super::UIViewHostObject,
    /// Not retained (delegates are weak in UIKit).
    delegate: id,
    /// `NSString*`, retained. Delegates (e.g. MoleWorld's offline-notice handler)
    /// read these back via -title / -message, so we must actually keep them.
    title: id,
    message: id,
    /// [扫描修 2026-09-15] 全部按钮标题(非 nil 的各 retain 一次),下标就是按钮索引。
    /// 以前只数个数,且变参 otherButtonTitles 最多只数到 1 个。
    button_titles: Vec<id>,
    /// [扫描修 2026-09-15] 取消按钮索引;-1 表示没有(与 UIKit 一致,以前恒返回 0)。
    cancel_index: NSInteger,
    /// [扫描修 2026-09-15] alertViewStyle:只存值,带输入框的样式暂不渲染 UITextField。
    alert_view_style: NSInteger,
}
impl_HostObject_with_superclass!(UIAlertViewHostObject);
impl Default for UIAlertViewHostObject {
    fn default() -> Self {
        UIAlertViewHostObject {
            superclass: Default::default(),
            delegate: nil,
            title: nil,
            message: nil,
            button_titles: Vec::new(),
            cancel_index: -1,
            alert_view_style: 0,
        }
    }
}

/// [扫描修 2026-09-15] 系统弹框的队列与当前覆盖层,挂在 `uikit::State` 上。
#[derive(Default)]
pub struct State {
    /// 已 show、尚未关闭的弹框(先进先出),每个持有一次 retain。队首 = 正在显示或等待显示。
    queue: Vec<id>,
    /// 当前挂在 keyWindow 上的覆盖层(总是对应 queue 里的某个弹框,通常是队首)。
    overlay: Option<Overlay>,
    /// 一次按在弹框上的触摸:松手前的 move/up 都吞掉,不让游戏收到半截触摸。
    capture: Option<Capture>,
    /// [复核修 2026-09-15] R1-1/R1-2:按下时被弹框吞掉的手指(弹框模态,显示期间的按下全部归弹框)。
    /// 这些手指的 move/up/cancel 都吞掉;不在这里的手指(弹框出现前就按下、已交给游戏的)照常交给游戏。
    /// 以前只有一个不记手指的 capture 槽:别的手指的抬起会被吞掉(游戏残留只有 began 的触点),
    /// 真正按弹框那根的抬起反被当成孤立抬起传给游戏,弹框点击也丢了。
    owned_fingers: HashSet<FingerId>,
    /// "keyWindow 还没建好"的日志只打一次。
    waiting_logged: bool,
}

struct Overlay {
    alert: id,
    /// 全屏容器视图(alloc 得来的 +1,拆除时 removeFromSuperview + release)。
    container: id,
    /// 按钮命中区(逻辑坐标 x, y, 宽, 高)与按钮索引。
    buttons: Vec<(CGFloat, CGFloat, CGFloat, CGFloat, NSInteger)>,
    /// 没有任何按钮的框:点任意处按索引 0 关闭(见 [present] 注释)。
    tap_anywhere: bool,
    /// guest 屏幕坐标 → 覆盖层逻辑坐标的换算参数。
    map: CoordMap,
}

#[derive(Clone, Copy)]
struct CoordMap {
    /// guest 竖屏点坐标尺寸(与 UIScreen bounds 同源)。
    screen_w: CGFloat,
    screen_h: CGFloat,
    /// 覆盖层逻辑尺寸(横屏时宽高互换)。
    logical_w: CGFloat,
    logical_h: CGFloat,
    /// 窗口旋转矩阵(guest 归一化坐标 → 窗口归一化坐标)的两列。
    col0: [f32; 2],
    col1: [f32; 2],
}

impl CoordMap {
    /// 与 window.rs transform_input_coords 互逆:guest 点 → 屏幕上"摆正"的逻辑坐标。
    fn guest_to_logical(&self, gx: CGFloat, gy: CGFloat) -> (CGFloat, CGFloat) {
        let u = gx / self.screen_w - 0.5;
        let v = gy / self.screen_h - 0.5;
        let wx = self.col0[0] * u + self.col1[0] * v;
        let wy = self.col0[1] * u + self.col1[1] * v;
        ((wx + 0.5) * self.logical_w, (wy + 0.5) * self.logical_h)
    }
}

struct Capture {
    /// 按下时正在显示的弹框。[复核修 2026-09-15] R1-2:该框在松手前被关闭时由 [dismiss] 置 nil,
    /// 防止原框释放、新框复用同一地址后松手误点新框。
    alert: id,
    /// [复核修 2026-09-15] R1-2:做点击判定的那根手指;只有它的抬起才判定点击。
    finger: FingerId,
    /// 按下时命中的按钮索引(没按在按钮上为 None)。
    pressed: Option<NSInteger>,
}

/// 弹框底板宽度(逻辑点)。真机 iPad 弹框约 284 点宽,这里放宽一些,桌面缩放后中文正文更易读。
const BOX_W: CGFloat = 440.0;
const PAD: CGFloat = 20.0;
const BTN_H: CGFloat = 46.0;
const BTN_GAP: CGFloat = 10.0;
const TITLE_FONT_SIZE: CGFloat = 21.0;
const MESSAGE_FONT_SIZE: CGFloat = 17.0;
const BUTTON_FONT_SIZE: CGFloat = 18.0;
/// 松手判定的容差:手指从按钮上略微滑出仍算点击。
const UP_SLOP: CGFloat = 24.0;
/// 同时排队的弹框上限;超出后按旧行为自动关闭,防止周期性弹框无限堆积。
const MAX_QUEUED: usize = 8;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIAlertView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIAlertViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// [扫描修 2026-09-15] 用 DotDotDot 读完 nil 结尾的 otherButtonTitles 变参,拿到全部按钮标题。
- (id)initWithTitle:(id)title
                      message:(id)message
                     delegate:(id)delegate
            cancelButtonTitle:(id)cancelButtonTitle
            otherButtonTitles:(id)otherButtonTitles, ...other_titles {
    let msg_s = if message == nil { Cow::from("(nil)") } else { ns_string::to_rust_string(env, message) };
    let title_s = if title == nil { Cow::from("(nil)") } else { ns_string::to_rust_string(env, title) };
    log!("UIAlertView init: title {:?}, message {:?} (delegate {:?})", title_s, msg_s, delegate);

    let this: id = msg_super![env; this init];

    let mut titles: Vec<id> = Vec::new();
    let mut cancel_index: NSInteger = -1;
    if cancelButtonTitle != nil {
        // UIKit:有取消键时它固定是 0 号按钮,其它按钮从 1 开始。
        cancel_index = 0;
        titles.push(cancelButtonTitle);
    }
    if otherButtonTitles != nil {
        titles.push(otherButtonTitles);
        let mut varargs = other_titles.start();
        // nil 结尾;加个上限,防止调用方漏写 nil 时一路读飞栈。
        for _ in 0..32 {
            let next: id = varargs.next(env);
            if next == nil {
                break;
            }
            titles.push(next);
        }
    }
    for &t in &titles {
        retain(env, t);
    }
    log!("UIAlertView init: {} 个按钮,取消键索引 {}", titles.len(), cancel_index);

    let title_copy: id = if title != nil { retain(env, title); title } else { nil };
    let message_copy: id = if message != nil { retain(env, message); message } else { nil };
    let host = env.objc.borrow_mut::<UIAlertViewHostObject>(this);
    host.delegate = delegate;
    host.title = title_copy;
    host.message = message_copy;
    host.button_titles = titles;
    host.cancel_index = cancel_index;
    this
}

// [扫描修 2026-09-15] 以前没有 dealloc,标题/正文泄漏;现在连同按钮标题一起释放。
- (())dealloc {
    let (title, message, titles) = {
        let host = env.objc.borrow_mut::<UIAlertViewHostObject>(this);
        (
            std::mem::replace(&mut host.title, nil),
            std::mem::replace(&mut host.message, nil),
            std::mem::take(&mut host.button_titles),
        )
    };
    if title != nil {
        release(env, title);
    }
    if message != nil {
        release(env, message);
    }
    for t in titles {
        if t != nil {
            release(env, t);
        }
    }
    () = msg_super![env; this dealloc];
}

- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).delegate = delegate;
}
- (id)delegate {
    env.objc.borrow::<UIAlertViewHostObject>(this).delegate
}
- (id)title {
    env.objc.borrow::<UIAlertViewHostObject>(this).title
}
- (())setTitle:(id)title {
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIAlertViewHostObject>(this).title, nil);
    let new: id = if title != nil { retain(env, title); title } else { nil };
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).title = new;
    if old != nil { release(env, old); }
}
- (id)message {
    env.objc.borrow::<UIAlertViewHostObject>(this).message
}
- (())setMessage:(id)message {
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIAlertViewHostObject>(this).message, nil);
    let new: id = if message != nil { retain(env, message); message } else { nil };
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).message = new;
    if old != nil { release(env, old); }
}

// [扫描修 2026-09-15] 真正记下标题,返回新按钮的索引。
- (NSInteger)addButtonWithTitle:(id)title {
    if title != nil {
        retain(env, title);
    }
    let host = env.objc.borrow_mut::<UIAlertViewHostObject>(this);
    host.button_titles.push(title);
    (host.button_titles.len() - 1) as NSInteger
}

- (NSInteger)numberOfButtons {
    env.objc.borrow::<UIAlertViewHostObject>(this).button_titles.len() as NSInteger
}

// [扫描修 2026-09-15] 补 buttonTitleAtIndex:(越界返回 nil)。
- (id)buttonTitleAtIndex:(NSInteger)index {
    if index < 0 {
        return nil;
    }
    env.objc
        .borrow::<UIAlertViewHostObject>(this)
        .button_titles
        .get(index as usize)
        .copied()
        .unwrap_or(nil)
}

- (NSInteger)cancelButtonIndex {
    env.objc.borrow::<UIAlertViewHostObject>(this).cancel_index
}
- (())setCancelButtonIndex:(NSInteger)index {
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).cancel_index = index;
}

// [扫描修 2026-09-15] 第一个非取消按钮的索引,没有则 -1。
- (NSInteger)firstOtherButtonIndex {
    let host = env.objc.borrow::<UIAlertViewHostObject>(this);
    let cancel = host.cancel_index;
    (0..host.button_titles.len())
        .map(|i| i as NSInteger)
        .find(|&i| i != cancel)
        .unwrap_or(-1)
}

- (NSInteger)alertViewStyle {
    env.objc.borrow::<UIAlertViewHostObject>(this).alert_view_style
}
- (())setAlertViewStyle:(NSInteger)style {
    if style != 0 {
        log!("TODO: UIAlertView {:?} alertViewStyle {} 带输入框的样式未实现,只显示标题/正文/按钮", this, style);
    }
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).alert_view_style = style;
}

- (bool)isVisible {
    env.framework_state
        .uikit
        .ui_alert_view
        .overlay
        .as_ref()
        .is_some_and(|o| o.alert == this)
}

- (())show {
    let delegate = env.objc.borrow::<UIAlertViewHostObject>(this).delegate;
    let exit_on_ok = is_exit_on_ok_delegate(env, delegate);
    if auto_dismiss_mode(env) || exit_on_ok {
        // 旧行为。CRUCIALLY the dismissal must happen ASYNCHRONOUSLY, not inline
        // here: callers like -[LoadingScene init] create the alert and call `show`
        // while still inside their own init (before the object has become the
        // running scene / had onEnter run), and their handlers do real work.
        if exit_on_ok {
            log!("[弹框] GameData 存档校验框:保持旧时序(约 1 帧后回调索引 0,原版随即 exit(0))");
        }
        log!("UIAlertView show: scheduling async auto-dismiss (index 0)");
        schedule_auto_dismiss(env, this);
        return;
    }

    let (already, queued) = {
        let state = &env.framework_state.uikit.ui_alert_view;
        (state.queue.contains(&this), state.queue.len())
    };
    if already {
        log!("[弹框] {:?} 已在显示队列里,重复 show 忽略", this);
        return;
    }
    if queued >= MAX_QUEUED {
        log!(
            "[弹框] 同时排队的弹框已达 {} 个,{:?} 按旧行为约 1 帧后自动按索引 0 关闭",
            MAX_QUEUED,
            this
        );
        schedule_auto_dismiss(env, this);
        return;
    }
    // 显示期间由我们持有(真机 UIKit 也会在弹框可见时 retain 它)。
    retain(env, this);
    env.framework_state.uikit.ui_alert_view.queue.push(this);
    log!("[弹框] show 入队 {:?}(前面还有 {} 个),下一轮运行循环挂到 keyWindow", this, queued);
}

- (())_touchHLE_autoDismiss {
    let dismiss_index: NSInteger = 0;
    let delegate = env.objc.borrow::<UIAlertViewHostObject>(this).delegate;
    log!("UIAlertView async auto-dismiss firing: index {} (delegate {:?})", dismiss_index, delegate);

    // GameData 存档校验框:在原版 exit(0) 之前,用宿主原生阻塞消息框把原文给玩家看。
    if !auto_dismiss_mode(env) && is_exit_on_ok_delegate(env, delegate) {
        show_blocking_host_notice(env, this);
    }
    send_delegate_callbacks(env, this, dismiss_index, /* clicked: */ true);
    // Balance the retain in `show`.
    release(env, this);
}

// [扫描修 2026-09-15] 程序主动关框:拆视图、出队,回调 willDismiss/didDismiss(不回调 clicked,与 UIKit 一致)。
// 不在显示队列里(已关闭 / 从没 show / 自动关闭模式)就忽略,避免对同一个框重复回调——
// 常见写法是在 alertView:clickedButtonAtIndex: 里再调一次 dismissWithClickedButtonIndex:animated:。
- (())dismissWithClickedButtonIndex:(NSInteger)button_index
                           animated:(bool)_animated {
    let queued = env.framework_state.uikit.ui_alert_view.queue.contains(&this);
    if !queued {
        log!(
            "[弹框] dismissWithClickedButtonIndex:{} 对象 {:?} 不在显示中(已关闭/未 show/自动关闭模式),忽略",
            button_index,
            this
        );
        return;
    }
    log!("[弹框] 程序关闭 {:?},index {}", this, button_index);
    dismiss(env, this, button_index, /* clicked: */ false);
}

@end

};

/// 无头模式或 MOLE_ALERT_AUTODISMISS(设为非 "0" 的值)时,沿用旧的"约 1 帧后按索引 0 自动关闭"。
fn auto_dismiss_mode(env: &Environment) -> bool {
    if env.options.headless {
        return true;
    }
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("MOLE_ALERT_AUTODISMISS")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// 回调里直接 exit(0)、且弹出时游戏数据不完整、不能让游戏在弹框后面继续跑的委托。
/// 目前只有 GameData(存档校验失败)。Porter(tag 1326)/NewScenePorter(tag 1521)/
/// WrapperManager(tag 1717)/iMoleVillageAppDelegate(tag 1442)的框虽然 didDismiss index 0 也会
/// exit(0),但弹出时游戏数据完好,按原版等玩家点确定即可,不在此列。
fn is_exit_on_ok_delegate(env: &Environment, delegate: id) -> bool {
    if delegate == nil {
        return false;
    }
    let class = ObjC::read_isa(delegate, &env.mem);
    class != nil && env.objc.try_get_class_name(class) == Some("GameData")
}

fn schedule_auto_dismiss(env: &mut Environment, this: id) {
    // Keep ourselves alive until the deferred dismissal runs.
    retain(env, this);
    let sel = env
        .objc
        .lookup_selector("_touchHLE_autoDismiss")
        .expect("_touchHLE_autoDismiss selector should be registered");
    () = msg![env; this performSelector:sel withObject:nil afterDelay:0.05f64];
}

/// 用宿主原生消息框(SDL,阻塞到玩家点掉为止)显示弹框标题/正文。只用于 GameData 存档校验框。
fn show_blocking_host_notice(env: &mut Environment, alert: id) {
    let (title, message) = {
        let host = env.objc.borrow::<UIAlertViewHostObject>(alert);
        (host.title, host.message)
    };
    let title = if title == nil {
        String::from("摩尔庄园")
    } else {
        ns_string::to_rust_string(env, title).into_owned()
    };
    let message = if message == nil {
        String::new()
    } else {
        ns_string::to_rust_string(env, message).into_owned()
    };
    echo!("[弹框] 存档校验失败提示(点确定后原版 exit(0)):{} / {}", title, message);
    let result = env.on_parent_stack_in_coroutine(move |_window, _options| {
        sdl2::messagebox::show_simple_message_box(
            sdl2::messagebox::MessageBoxFlag::WARNING,
            &title,
            &message,
            None::<&sdl2::video::Window>,
        )
    });
    if result.is_err() {
        log!("[弹框] 宿主原生消息框显示失败(内容已打印到日志)");
    }
}

fn responds(env: &mut Environment, obj: id, sel_name: &str) -> bool {
    let Some(sel) = env.objc.lookup_selector(sel_name) else {
        return false;
    };
    msg![env; obj respondsToSelector:sel]
}

/// 按 UIKit 顺序回调委托:(clicked 时)alertView:clickedButtonAtIndex: →
/// alertView:willDismissWithButtonIndex: → alertView:didDismissWithButtonIndex:。
fn send_delegate_callbacks(env: &mut Environment, alert: id, index: NSInteger, clicked: bool) {
    let delegate = env.objc.borrow::<UIAlertViewHostObject>(alert).delegate;
    if delegate == nil {
        return;
    }
    let pool: id = msg_class![env; NSAutoreleasePool new];
    retain(env, delegate);
    if clicked && responds(env, delegate, "alertView:clickedButtonAtIndex:") {
        () = msg![env; delegate alertView:alert clickedButtonAtIndex:index];
    }
    if responds(env, delegate, "alertView:willDismissWithButtonIndex:") {
        () = msg![env; delegate alertView:alert willDismissWithButtonIndex:index];
    }
    if responds(env, delegate, "alertView:didDismissWithButtonIndex:") {
        () = msg![env; delegate alertView:alert didDismissWithButtonIndex:index];
    }
    release(env, delegate);
    let _: () = msg![env; pool drain];
}

/// 关闭一个排队中的弹框:出队、拆视图,再回调委托,最后释放 show 时的 retain。
/// 先出队再回调:委托在回调里再 dismiss 自己会被忽略,在回调里 show 新框会正常入队。
fn dismiss(env: &mut Environment, alert: id, index: NSInteger, clicked: bool) {
    let overlay = {
        let state = &mut env.framework_state.uikit.ui_alert_view;
        let Some(pos) = state.queue.iter().position(|&a| a == alert) else {
            return;
        };
        state.queue.remove(pos);
        // [复核修 2026-09-15] R1-2:手指还按着时框被关闭(程序关闭或点击关闭):让这次点击判定失效,
        // 松手时不再与之后显示的弹框比较(原框释放后新框可能复用同一地址)。触摸本身仍归弹框、继续吞掉。
        if let Some(capture) = state.capture.as_mut() {
            if capture.alert == alert {
                capture.alert = nil;
                capture.pressed = None;
            }
        }
        if state.overlay.as_ref().is_some_and(|o| o.alert == alert) {
            state.overlay.take()
        } else {
            None
        }
    };
    if let Some(overlay) = overlay {
        let container = overlay.container;
        () = msg![env; container removeFromSuperview];
        release(env, container);
    }
    send_delegate_callbacks(env, alert, index, clicked);
    // 平衡 show 里的 retain。
    release(env, alert);
}

/// [扫描修 2026-09-15] 由 uikit::handle_events 每轮运行循环调用:队首弹框还没挂上就挂到 keyWindow。
pub fn pump(env: &mut Environment) {
    let front = {
        let state = &env.framework_state.uikit.ui_alert_view;
        if state.overlay.is_some() {
            return;
        }
        let Some(&front) = state.queue.first() else {
            return;
        };
        front
    };
    // 作弊菜单开着时先不挂:菜单独占触摸,弹框若盖在菜单上面,点击会落到下面的菜单按钮上。
    if crate::mole_menu::is_open() {
        return;
    }
    let app: id = msg_class![env; UIApplication sharedApplication];
    let window: id = if app == nil { nil } else { msg![env; app keyWindow] };
    if window == nil {
        if !env.framework_state.uikit.ui_alert_view.waiting_logged {
            env.framework_state.uikit.ui_alert_view.waiting_logged = true;
            log!("[弹框] keyWindow 尚未建好,弹框 {:?} 延后到下一帧再挂", front);
        }
        return;
    }
    env.framework_state.uikit.ui_alert_view.waiting_logged = false;
    present(env, front, window);
}

/// [扫描修 2026-09-15] uikit.rs 在把触摸交给游戏前先调用。
/// [复核修 2026-09-15] R1-1/R1-2/R1-3:改为按手指拆分,返回应交给游戏的剩余事件(None = 整个事件被弹框吞掉)。
/// 以前返回 bool、整包吞或整包放行,且不分手指:任取一根算命中,capture 不记手指。
/// - 按下:弹框显示中这批手指全部归弹框(模态);挑一根非虚拟捏合手指做点击判定,优先按在按钮上的那根。
///   同一根手指再次按下说明它上一次触摸早已结束(抬起没传到这里,例如作弊菜单开着时被吞),先清掉残留;
///   新的按下覆盖旧的点击判定(后按者生效),免得旧手指的抬起丢失后弹框再也点不动。
/// - 移动:去掉归弹框的手指,其余交给游戏。
/// - 抬起:去掉归弹框的手指;只有做点击判定的那根抬起、且仍在同一按钮内才算点击(UIKit touch-up-inside)。
///   弹框开着之前就按下的触摸照常交给游戏,免得游戏里留下只有 began 没有 ended 的半截触摸。
/// - 取消:同抬起,但一律不算点击。
/// - 滚轮合成的虚拟捏合手指(FingerId::PinchA/PinchB,R1-1)归弹框时只吞不点:以前光标停在按钮上
///   滚一格,两根虚拟手指都落在按钮内,约 150ms 后结束时就回调 clickedButtonAtIndex:(Porter tag1326 等框
///   索引 0 = exit(0)),无按钮框(tap_anywhere)滚一下就被关闭。
pub fn filter_touch_event(env: &mut Environment, event: Event) -> Option<Event> {
    match event {
        Event::TouchesDown(map) => filter_touches_down(env, map).map(Event::TouchesDown),
        Event::TouchesMove(map) => {
            strip_owned_fingers(&env.framework_state.uikit.ui_alert_view.owned_fingers, map)
                .map(Event::TouchesMove)
        }
        Event::TouchesUp(map) => {
            filter_touches_end(env, map, /* cancelled: */ false).map(Event::TouchesUp)
        }
        Event::TouchesCancel(map) => {
            filter_touches_end(env, map, /* cancelled: */ true).map(Event::TouchesCancel)
        }
        other => Some(other),
    }
}

/// [复核修 2026-09-15] R1-1:滚轮合成的虚拟捏合手指(window.rs 的 PinchState)。
fn is_pinch_finger(finger: FingerId) -> bool {
    matches!(finger, FingerId::PinchA | FingerId::PinchB)
}

/// [复核修 2026-09-15] 去掉归弹框的手指。原本有手指、去掉后一根不剩时返回 None(整个事件被吞掉)。
fn strip_owned_fingers(
    owned: &HashSet<FingerId>,
    mut map: HashMap<FingerId, Coords>,
) -> Option<HashMap<FingerId, Coords>> {
    if owned.is_empty() || map.is_empty() {
        return Some(map);
    }
    map.retain(|finger, _| !owned.contains(finger));
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

/// [复核修 2026-09-15] 按下:见 [filter_touch_event]。返回 None = 被弹框吞掉。
fn filter_touches_down(
    env: &mut Environment,
    map: HashMap<FingerId, Coords>,
) -> Option<HashMap<FingerId, Coords>> {
    let state = &mut env.framework_state.uikit.ui_alert_view;
    // 同一根手指又按下:它上一次触摸早已结束,清掉残留的归属与点击判定。
    for finger in map.keys() {
        state.owned_fingers.remove(finger);
    }
    if state
        .capture
        .as_ref()
        .is_some_and(|c| map.contains_key(&c.finger))
    {
        state.capture = None;
    }
    let (alert, chosen) = {
        let Some(overlay) = state.overlay.as_ref() else {
            return Some(map);
        };
        // 挑做点击判定的手指:虚拟捏合手指不参与(R1-1);优先按在按钮上的那根。
        let mut chosen: Option<(FingerId, Option<NSInteger>)> = None;
        for (&finger, &(x, y)) in &map {
            if is_pinch_finger(finger) {
                continue;
            }
            let (lx, ly) = overlay.map.guest_to_logical(x, y);
            let pressed = hit_button(overlay, lx, ly, 0.0);
            let better = match chosen {
                None => true,
                Some((_, previous)) => previous.is_none() && pressed.is_some(),
            };
            if better {
                chosen = Some((finger, pressed));
            }
        }
        (overlay.alert, chosen)
    };
    // 弹框模态:这批手指整次触摸都归弹框。
    state.owned_fingers.extend(map.keys().copied());
    if let Some((finger, pressed)) = chosen {
        state.capture = Some(Capture {
            alert,
            finger,
            pressed,
        });
    }
    None
}

/// [复核修 2026-09-15] 抬起 / 取消:去掉归弹框的手指(同时从记账里移除),剩下的交给游戏;
/// 做点击判定的那根手指抬起时按原逻辑判定点击,取消时一律不点。返回 None = 被弹框吞掉。
fn filter_touches_end(
    env: &mut Environment,
    mut map: HashMap<FingerId, Coords>,
    cancelled: bool,
) -> Option<HashMap<FingerId, Coords>> {
    let (capture, release_coords, swallowed_any) = {
        let state = &mut env.framework_state.uikit.ui_alert_view;
        let capture = if state
            .capture
            .as_ref()
            .is_some_and(|c| map.contains_key(&c.finger))
        {
            state.capture.take()
        } else {
            None
        };
        let release_coords: Option<Coords> =
            capture.as_ref().and_then(|c| map.get(&c.finger).copied());
        let capture_finger: Option<FingerId> = capture.as_ref().map(|c| c.finger);
        let mut swallowed_any = false;
        if !map.is_empty() && (!state.owned_fingers.is_empty() || capture_finger.is_some()) {
            let owned = &mut state.owned_fingers;
            map.retain(|finger, _| {
                // 做点击判定的手指按下时一定已记进 owned_fingers;这里再兜一次底。
                let is_owned = owned.remove(finger) || capture_finger == Some(*finger);
                swallowed_any |= is_owned;
                !is_owned
            });
        }
        (capture, release_coords, swallowed_any)
    };

    if let Some(capture) = capture {
        let clicked: Option<NSInteger> = match release_coords {
            Some((x, y)) if !cancelled && capture.alert != nil => {
                let state = &env.framework_state.uikit.ui_alert_view;
                match state.overlay.as_ref() {
                    Some(overlay) if overlay.alert == capture.alert => {
                        if overlay.tap_anywhere {
                            Some(0)
                        } else {
                            let (lx, ly) = overlay.map.guest_to_logical(x, y);
                            match (capture.pressed, hit_button(overlay, lx, ly, UP_SLOP)) {
                                (Some(down), Some(up)) if down == up => Some(down),
                                _ => None,
                            }
                        }
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        if cancelled {
            log!("[弹框] 按在弹框 {:?} 上的触摸被取消,不算点击", capture.alert);
        }
        if let Some(index) = clicked {
            log!("[弹框] 玩家点击 {:?} 的按钮 {}", capture.alert, index);
            dismiss(env, capture.alert, index, /* clicked: */ true);
        }
    }

    if swallowed_any && map.is_empty() {
        None
    } else {
        Some(map)
    }
}

fn hit_button(overlay: &Overlay, lx: CGFloat, ly: CGFloat, slop: CGFloat) -> Option<NSInteger> {
    overlay
        .buttons
        .iter()
        .find(|&&(x, y, w, h, _)| {
            lx >= x - slop && lx <= x + w + slop && ly >= y - slop && ly <= y + h + slop
        })
        .map(|&(_, _, _, _, index)| index)
}

fn rect(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

fn color(env: &mut Environment, r: CGFloat, g: CGFloat, b: CGFloat, a: CGFloat) -> id {
    msg_class![env; UIColor colorWithRed:r green:g blue:b alpha:a]
}

/// 由窗口旋转矩阵推出覆盖层的逻辑尺寸、坐标换算与视图变换。
/// 视图变换把"逻辑偏移 (dx,dy)"映射到 guest 点偏移:先除以逻辑宽高得到窗口归一化坐标,
/// 乘旋转矩阵的逆(纯旋转的逆 = 转置)回到 guest 归一化坐标,再乘 guest 宽高。
/// 结果只会是 0/±1,四舍五入去掉浮点噪声。--landscape-right 下得到 (a,b,c,d)=(0,-1,1,0),
/// 与 mole_menu 实测可用的写法一致。
fn coord_map_and_transform(env: &Environment) -> (CoordMap, CGAffineTransform) {
    let (pw, ph) = env.window().device_family().portrait_size();
    let (screen_w, screen_h) = (pw as CGFloat, ph as CGFloat);
    let rotation = env.window().rotation_matrix();
    let col0 = rotation.transform([1.0, 0.0]);
    let col1 = rotation.transform([0.0, 1.0]);
    // 横屏:guest 的 x 轴被转到了窗口的 y 轴上。
    let landscape = col0[0].abs() < 0.5;
    let (logical_w, logical_h) = if landscape {
        (screen_h, screen_w)
    } else {
        (screen_w, screen_h)
    };
    let transform = CGAffineTransform {
        a: (screen_w * col0[0] / logical_w).round(),
        b: (screen_h * col1[0] / logical_w).round(),
        c: (screen_w * col0[1] / logical_h).round(),
        d: (screen_h * col1[1] / logical_h).round(),
        tx: 0.0,
        ty: 0.0,
    };
    (
        CoordMap {
            screen_w,
            screen_h,
            logical_w,
            logical_h,
            col0,
            col1,
        },
        transform,
    )
}

/// 含非 ASCII(中文)时按字符换行:font.rs 的按词换行只在空白处断行,整句中文会挤成一行。
fn line_break_mode_for(env: &mut Environment, text: id) -> UILineBreakMode {
    if text == nil {
        return UILineBreakModeWordWrap;
    }
    let s = ns_string::to_rust_string(env, text);
    if s.chars().any(|c| !c.is_ascii()) {
        UILineBreakModeCharacterWrap
    } else {
        UILineBreakModeWordWrap
    }
}

/// 量出一段文字在给定宽度下的高度(不超过 max_h);nil 或空白串返回高度 0。
fn measure_text(
    env: &mut Environment,
    text: id,
    font: id,
    width: CGFloat,
    max_h: CGFloat,
) -> (CGFloat, UILineBreakMode) {
    if text == nil {
        return (0.0, UILineBreakModeWordWrap);
    }
    let empty = ns_string::to_rust_string(env, text).trim().is_empty();
    let mode = line_break_mode_for(env, text);
    if empty {
        return (0.0, mode);
    }
    let limit = CGSize {
        width,
        height: max_h,
    };
    let size: CGSize = msg![env; text sizeWithFont:font
                                constrainedToSize:limit
                                    lineBreakMode:mode];
    let height: CGFloat = size.height;
    (height.min(max_h).ceil(), mode)
}

fn add_plain_view(env: &mut Environment, parent: id, frame: CGRect, background: id) {
    let view: id = msg_class![env; UIView alloc];
    let view: id = msg![env; view initWithFrame:frame];
    () = msg![env; view setBackgroundColor:background];
    () = msg![env; parent addSubview:view];
    release(env, view);
}

#[allow(clippy::too_many_arguments)]
fn add_text_label(
    env: &mut Environment,
    parent: id,
    frame: CGRect,
    text: id,
    font: id,
    text_color: id,
    background: id,
    mode: UILineBreakMode,
) {
    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label initWithFrame:frame];
    () = msg![env; label setText:text];
    () = msg![env; label setFont:font];
    () = msg![env; label setTextColor:text_color];
    () = msg![env; label setBackgroundColor:background];
    () = msg![env; label setTextAlignment:UITextAlignmentCenter];
    let unlimited_lines: NSInteger = 0;
    () = msg![env; label setNumberOfLines:unlimited_lines];
    () = msg![env; label setLineBreakMode:mode];
    () = msg![env; parent addSubview:label];
    release(env, label);
}

/// 把弹框渲染成覆盖层挂到 keyWindow 最上层。
///
/// 布局(逻辑坐标,横屏时即 1024×768 或宽屏宽度×768):全屏半透明遮罩,中间边框 + 深蓝底板,
/// 自上而下标题(粗体)、正文、按钮条。按钮排布照 iOS:两个按钮并排且取消键在左;
/// 三个及以上竖排、取消键放最后;一个按钮占满整行。
/// 没有任何按钮的框(真机上只能由程序关闭):离线时可能永远等不到程序关闭而把玩家卡死,
/// 所以显示"点击任意处关闭",点一下按索引 0 回调——与改动前的自动关闭语义相同,只是等玩家确认。
fn present(env: &mut Environment, alert: id, window: id) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let (map, transform) = coord_map_and_transform(env);
    let (w, h) = (map.logical_w, map.logical_h);

    let (title, message, button_titles, cancel_index) = {
        let host = env.objc.borrow::<UIAlertViewHostObject>(alert);
        (
            host.title,
            host.message,
            host.button_titles.clone(),
            host.cancel_index,
        )
    };

    let box_w = BOX_W.min(w - 40.0);
    let text_w = box_w - 2.0 * PAD;
    let max_message_h = (h - 260.0).max(80.0);

    let title_font: id = msg_class![env; UIFont boldSystemFontOfSize:TITLE_FONT_SIZE];
    let message_font: id = msg_class![env; UIFont systemFontOfSize:MESSAGE_FONT_SIZE];
    let button_font: id = msg_class![env; UIFont boldSystemFontOfSize:BUTTON_FONT_SIZE];

    let (title_h, title_mode) = measure_text(env, title, title_font, text_w, 160.0);
    let (message_h, message_mode) = measure_text(env, message, message_font, text_w, max_message_h);

    // 按钮显示顺序。
    let n = button_titles.len();
    let cancel = if cancel_index >= 0 && (cancel_index as usize) < n {
        Some(cancel_index as usize)
    } else {
        None
    };
    let mut order: Vec<usize> = (0..n).filter(|&i| Some(i) != cancel).collect();
    if let Some(ci) = cancel {
        if n == 2 {
            order.insert(0, ci);
        } else {
            order.push(ci);
        }
    }
    let side_by_side = n == 2;
    let buttons_h: CGFloat = match n {
        0 => 0.0,
        1 | 2 => BTN_H,
        _ => n as CGFloat * BTN_H + (n - 1) as CGFloat * BTN_GAP,
    };
    let tap_anywhere = n == 0;
    let hint_h: CGFloat = if tap_anywhere { 22.0 } else { 0.0 };

    // 内容总高:各段之间留间距(段为空则不留)。
    let mut content_h: CGFloat = 0.0;
    for (part_h, gap) in [
        (title_h, 0.0),
        (message_h, 10.0),
        (buttons_h, 18.0),
        (hint_h, 12.0),
    ] {
        if part_h > 0.0 {
            if content_h > 0.0 {
                content_h += gap;
            }
            content_h += part_h;
        }
    }
    let box_h = content_h + 2.0 * PAD;
    let box_x = ((w - box_w) / 2.0).floor();
    let box_y = ((h - box_h) / 2.0).floor().max(10.0);

    let container: id = msg_class![env; UIView alloc];
    let full = rect(0.0, 0.0, w, h);
    let container: id = msg![env; container initWithFrame:full];
    let dim = color(env, 0.0, 0.0, 0.0, 0.45);
    () = msg![env; container setBackgroundColor:dim];

    let border = color(env, 0.85, 0.88, 0.95, 0.9);
    add_plain_view(
        env,
        container,
        rect(box_x - 2.0, box_y - 2.0, box_w + 4.0, box_h + 4.0),
        border,
    );
    let panel = color(env, 0.07, 0.12, 0.29, 0.96);
    add_plain_view(env, container, rect(box_x, box_y, box_w, box_h), panel);

    let white = color(env, 1.0, 1.0, 1.0, 1.0);
    let soft_white = color(env, 0.9, 0.93, 1.0, 1.0);
    let clear = color(env, 0.0, 0.0, 0.0, 0.0);

    let mut y = box_y + PAD;
    let mut placed_any = false;
    if title_h > 0.0 {
        add_text_label(
            env,
            container,
            rect(box_x + PAD, y, text_w, title_h),
            title,
            title_font,
            white,
            clear,
            title_mode,
        );
        y += title_h;
        placed_any = true;
    }
    if message_h > 0.0 {
        if placed_any {
            y += 10.0;
        }
        add_text_label(
            env,
            container,
            rect(box_x + PAD, y, text_w, message_h),
            message,
            message_font,
            soft_white,
            clear,
            message_mode,
        );
        y += message_h;
        placed_any = true;
    }

    let mut hits: Vec<(CGFloat, CGFloat, CGFloat, CGFloat, NSInteger)> = Vec::new();
    if n > 0 {
        if placed_any {
            y += 18.0;
        }
        let normal_bg = color(env, 0.33, 0.43, 0.70, 1.0);
        let cancel_bg = color(env, 0.20, 0.25, 0.44, 1.0);
        for (slot, &index) in order.iter().enumerate() {
            let (bx, by, bw) = if side_by_side {
                let bw = (text_w - BTN_GAP) / 2.0;
                (box_x + PAD + slot as CGFloat * (bw + BTN_GAP), y, bw)
            } else {
                (box_x + PAD, y + slot as CGFloat * (BTN_H + BTN_GAP), text_w)
            };
            let background = if Some(index) == cancel {
                cancel_bg
            } else {
                normal_bg
            };
            let button_title = button_titles[index];
            let mode = line_break_mode_for(env, button_title);
            add_text_label(
                env,
                container,
                rect(bx, by, bw, BTN_H),
                button_title,
                button_font,
                white,
                background,
                mode,
            );
            hits.push((bx, by, bw, BTN_H, index as NSInteger));
        }
    }
    if tap_anywhere {
        if placed_any {
            y += 12.0;
        }
        let hint = ns_string::get_static_str(env, "(点击任意处关闭)");
        add_text_label(
            env,
            container,
            rect(box_x + PAD, y, text_w, hint_h),
            hint,
            message_font,
            soft_white,
            clear,
            UILineBreakModeWordWrap,
        );
    }

    // 旋转 + 居中,与屏幕方向一致地"摆正"。
    () = msg![env; container setTransform:transform];
    let center = CGPoint {
        x: map.screen_w / 2.0,
        y: map.screen_h / 2.0,
    };
    () = msg![env; container setCenter:center];
    () = msg![env; window addSubview:container];

    let _: () = msg![env; pool drain];

    log!(
        "[弹框] 已显示 {:?}:{} 个按钮(取消键 {}),逻辑尺寸 {}x{}",
        alert,
        n,
        cancel_index,
        w,
        h
    );
    env.framework_state.uikit.ui_alert_view.overlay = Some(Overlay {
        alert,
        container,
        buttons: hits,
        tap_anywhere,
        map,
    });
}
