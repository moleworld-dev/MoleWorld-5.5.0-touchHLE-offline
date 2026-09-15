/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The UIKit framework.
//!
//! For the time being the focus of this project is on running games, which are
//! likely to use UIKit in very simple and limited ways, so this implementation
//! will probably take a lot of shortcuts.

use crate::{msg, Environment};
use std::time::Instant;

pub mod ui_accelerometer;
pub mod ui_activity_indicator_view;
pub mod ui_application;
pub mod ui_color;
pub mod ui_device;
pub mod ui_event;
pub mod ui_font;
pub mod ui_gesture_recognizer;
pub mod ui_geometry;
pub mod ui_graphics;
pub mod ui_image;
pub mod ui_image_picker_controller;
pub mod ui_local_notification;
pub mod ui_nib;
pub mod ui_pasteboard;
pub mod ui_responder;
pub mod ui_screen;
pub mod ui_touch;
pub mod ui_view;
pub mod ui_view_controller;

pub const DYLIB: crate::dyld::HostDylib = crate::dyld::HostDylib {
    path: "/System/Library/Frameworks/UIKit.framework/UIKit",
    aliases: &[],
    class_exports: &[
        ui_accelerometer::CLASSES,
        ui_activity_indicator_view::CLASSES,
        ui_application::CLASSES,
        ui_color::CLASSES,
        ui_device::CLASSES,
        ui_event::CLASSES,
        ui_font::CLASSES,
        ui_gesture_recognizer::CLASSES,
        ui_image::CLASSES,
        ui_image_picker_controller::CLASSES,
        ui_local_notification::CLASSES,
        ui_nib::CLASSES,
        ui_pasteboard::CLASSES,
        ui_responder::CLASSES,
        ui_screen::CLASSES,
        ui_touch::CLASSES,
        ui_view::CLASSES,
        ui_view::ui_alert_view::CLASSES,
        ui_view::ui_control::CLASSES,
        ui_view::ui_control::ui_button::CLASSES,
        ui_view::ui_control::ui_segmented_control::CLASSES,
        ui_view::ui_control::ui_slider::CLASSES,
        ui_view::ui_control::ui_text_field::CLASSES,
        ui_view::ui_control::ui_switch::CLASSES,
        ui_view::ui_image_view::CLASSES,
        ui_view::ui_label::CLASSES,
        ui_view::ui_picker_view::CLASSES,
        ui_view::ui_scroll_view::CLASSES,
        ui_view::ui_scroll_view::ui_text_view::CLASSES,
        ui_view::ui_table_view::CLASSES,
        ui_view::ui_web_view::CLASSES,
        ui_view::ui_window::CLASSES,
        ui_view_controller::CLASSES,
        ui_view_controller::ui_navigation_controller::CLASSES,
    ],
    constant_exports: &[
        ui_application::CONSTANTS,
        ui_device::CONSTANTS,
        ui_view::ui_control::ui_text_field::CONSTANTS,
        ui_view::ui_scroll_view::ui_text_view::CONSTANTS,
        ui_view::ui_window::CONSTANTS,
    ],
    function_exports: &[
        ui_application::FUNCTIONS,
        ui_geometry::FUNCTIONS,
        ui_graphics::FUNCTIONS,
        ui_image::FUNCTIONS,
    ],
};

#[derive(Default)]
pub struct State {
    ui_accelerometer: ui_accelerometer::State,
    ui_application: ui_application::State,
    ui_color: ui_color::State,
    ui_device: ui_device::State,
    ui_font: ui_font::State,
    ui_graphics: ui_graphics::State,
    ui_image: ui_image::State,
    ui_screen: ui_screen::State,
    ui_touch: ui_touch::State,
    pub ui_view: ui_view::State,
    ui_responder: ui_responder::State,
    /// [扫描修 2026-09-15] F12-1:系统弹框(UIAlertView)的显示队列与覆盖层。
    ui_alert_view: ui_view::ui_alert_view::State,
    /// [补完 2026-09-15] 已经经 route_touch 交给系统弹框/游戏、还没抬起或取消的手指及其最后坐标。
    /// 切后台挂起前据此把它们以取消结束(见 cancel_tracked_touches);ui_touch 的触点表是它模块私有的,
    /// 这里在分发入口单独记一份。
    touch_shadow: std::collections::HashMap<crate::window::FingerId, crate::window::Coords>,
}

/// [扫描修 2026-09-15] F12-1:触摸先问系统弹框(模态,显示中会吞掉),没被吞才交给游戏。
/// [复核修 2026-09-15] R1-2:按手指拆分——弹框只拿走"按下时落在弹框上"的手指,同一事件里其余手指
/// (弹框出现前就按下、已交给游戏的)照常交给游戏,不再整包吞掉或整包放行。
fn route_touch(env: &mut Environment, event: crate::window::Event) {
    // [补完 2026-09-15] 先记账(touch_shadow),再分发。
    track_touch_shadow(env, &event);
    if let Some(event) = ui_view::ui_alert_view::filter_touch_event(env, event) {
        ui_touch::handle_event(env, event);
    }
}

/// [补完 2026-09-15] 维护 touch_shadow:按下记入、移动更新坐标(只更新已记录的手指)、抬起/取消移除。
fn track_touch_shadow(env: &mut Environment, event: &crate::window::Event) {
    use crate::window::Event;
    let shadow = &mut env.framework_state.uikit.touch_shadow;
    match event {
        Event::TouchesDown(map) => {
            for (&finger, &coords) in map {
                shadow.insert(finger, coords);
            }
        }
        Event::TouchesMove(map) => {
            for (finger, &coords) in map {
                if let Some(last) = shadow.get_mut(finger) {
                    *last = coords;
                }
            }
        }
        Event::TouchesUp(map) | Event::TouchesCancel(map) => {
            for finger in map.keys() {
                shadow.remove(finger);
            }
        }
        _ => {}
    }
}

/// [补完 2026-09-15] 切后台挂起前:把仍按着的手指以取消结束(UITouchPhaseCancelled →
/// touchesCancelled:withEvent:,与真机来电/切后台时 UIKit 的做法一致)。
/// 根因:挂起期间真实的抬起事件被丢弃(安卓切走时 SDL 才补发的 FingerUp 也在其中),不收尾的话回来后
/// 游戏里残留一根"一直按着"的手指(村庄拖动卡住、按钮停在按下态、下一次按下被当成移动)。
/// 经 route_touch 发出:系统弹框先收回归它的手指(取消不算点击),其余交给 ui_touch 的
/// handle_touches_cancelled(含 cocos2d 门控收尾);ui_touch 不认识的手指只记警告、不 panic。
/// 调用上下文与 route_touch 相同(handle_events 内,可以发 msg_send)。
fn cancel_tracked_touches(env: &mut Environment, reason: &str) {
    let map = std::mem::take(&mut env.framework_state.uikit.touch_shadow);
    if map.is_empty() {
        return;
    }
    log!(
        "[生命周期] {}:以取消结束 {} 个仍按着的触点 {:?}",
        reason,
        map.len(),
        map.keys().collect::<Vec<_>>()
    );
    route_touch(env, crate::window::Event::TouchesCancel(map));
}

/// For use by `NSRunLoop`: handles any events that have queued up.
///
/// Returns the next time this function must be called, if any, e.g. the next
/// time an accelerometer input is due.
pub fn handle_events(env: &mut Environment) -> Option<Instant> {
    use crate::window::Event;
    use crate::window::TextInputEvent;

    // [扫描修 2026-09-15] F12-1:队首系统弹框还没挂上就在这里挂到 keyWindow
    // (show 时只入队;keyWindow 未建好 / 作弊菜单开着时下一轮再试)。
    ui_view::ui_alert_view::pump(env);

    // [MoleWorld DIAG] Inject a synthetic tap from /tmp/mole_input so the game
    // can be driven without host input (the window is on its own macOS Space and
    // can't be clicked via the host). One Down/Up step per call; coordinates are
    // guest screen points.
    // [扫描修 2026-09-15] 注入的触摸同样先经过系统弹框(route_touch),脚本可以点弹框按钮。
    if let Some(inject) = crate::mole_diag::next_inject() {
        match inject {
            crate::mole_diag::Inject::Menu => {
                // [2026-09-16] F2-03:同下面 T 键分支,打开菜单前先收尾仍按着的手指。
                if !crate::mole_menu::is_open() {
                    cancel_tracked_touches(env, "打开修改器菜单");
                }
                crate::mole_menu::toggle(env)
            }
            crate::mole_diag::Inject::Down(x, y) => {
                if crate::mole_menu::is_open() {
                    crate::mole_menu::handle_touch(env, x, y);
                } else {
                    route_touch(
                        env,
                        Event::TouchesDown(std::collections::HashMap::from([(
                            crate::window::FingerId::Mouse,
                            (x, y),
                        )])),
                    );
                }
            }
            crate::mole_diag::Inject::Move(x, y) => {
                if !crate::mole_menu::is_open() {
                    route_touch(
                        env,
                        Event::TouchesMove(std::collections::HashMap::from([(
                            crate::window::FingerId::Mouse,
                            (x, y),
                        )])),
                    );
                }
            }
            crate::mole_diag::Inject::Up(x, y) => {
                if !crate::mole_menu::is_open() {
                    route_touch(
                        env,
                        Event::TouchesUp(std::collections::HashMap::from([(
                            crate::window::FingerId::Mouse,
                            (x, y),
                        )])),
                    );
                }
            }
            crate::mole_diag::Inject::Suspend(secs) => {
                // [补完 2026-09-15] 无头验证切后台:走与 Android `Event::AppWillResignActive` 完全相同的
                // 失活→挂起→激活 路径(ui_application::suspend_app),只是挂起的结束条件换成计时到点
                // (桌面上没有 SDL 前后台事件)。这里与上面分发触摸同在 run loop 顶部,可以安全发 msg_send。
                log!("[生命周期] 收到注入命令 suspend {}s", secs);
                ui_application::suspend_app(
                    env,
                    crate::window::SuspendEnd::Timer(std::time::Duration::from_secs_f32(secs)),
                    "注入 suspend",
                );
            }
        }
    }

    // NSRunLoop will never call this function in headless mode.
    while let Some(event) = env.window_mut().pop_event() {
        match event {
            Event::Quit => {
                echo!("User requested quit, exiting.");
                ui_application::exit(env);
            }
            // [MoleWorld] T toggles the built-in debug/cheat menu.
            Event::ToggleMoleMenu => {
                // [2026-09-16] F2-03:即将打开菜单时,先把已经交给游戏、仍按着的手指以取消结束。
                // 根因:菜单开着时下面的分支直接吞掉 TouchesMove/TouchesUp,不经过 route_touch,ui_touch 和
                // touch_shadow 里一直留着那根手指;关菜单后的第一次按下会被 ui_touch 当成旧触点的移动
                // (日志 "treating as movement"),点建筑/按钮没反应。取消路径与切后台挂起、滚轮捏合共用。
                if !crate::mole_menu::is_open() {
                    cancel_tracked_touches(env, "打开修改器菜单");
                }
                crate::mole_menu::toggle(env)
            }
            // While the menu is open, route touches to it instead of the game.
            Event::TouchesDown(ref map) if crate::mole_menu::is_open() => {
                if let Some((_, &(x, y))) = map.iter().next() {
                    crate::mole_menu::handle_touch(env, x, y);
                }
            }
            Event::TouchesMove(..) | Event::TouchesUp(..) if crate::mole_menu::is_open() => {
                // Swallow move/up while the menu is open.
            }
            // [复核修 2026-09-15] R1-3:取消事件(滚轮虚拟捏合结束)不走上面"菜单开着就吞掉"的分支:
            // 被取消的触点只可能是菜单打开前就交给游戏的(菜单开着时按下的触摸进了菜单、ui_touch 没有记录,
            // 虚拟捏合也不会在菜单开着时开始),照常交给游戏收尾,免得游戏里残留只有 began 的触点。
            Event::TouchesDown(..)
            | Event::TouchesMove(..)
            | Event::TouchesUp(..)
            | Event::TouchesCancel(..) => {
                // [扫描修 2026-09-15] F12-1:系统弹框显示中先由弹框处理(模态)。
                route_touch(env, event)
            }
            // [扫描修 2026-09-15] F12-3:桌面窗口最小化/还原(W2 在 window.rs 发出)→ 原版失活/激活回调
            // 与对应通知;不发 DidEnterBackground/WillEnterForeground。细节见 ui_application.rs。
            Event::WindowMinimized => ui_application::handle_window_minimized(env),
            Event::WindowRestored => ui_application::handle_window_restored(env),
            Event::AppWillResignActive => {
                if ui_application::suspend_on_background(env) {
                    // [补完 2026-09-15] Android 切后台(按 Home、切应用、来电)不再退出,改为挂起等回前台,
                    // 回来停在原画面继续。此前保持退出的三条理由现在各有处理:
                    // ① window.rs 收到 SDL AppWillEnterBackground 时仍把 enable_event_polling 置 false,
                    //    但挂起循环(Window::suspend_until_foreground)返回前会把它恢复为 true;
                    // ② SDL(BLOCK_ON_PAUSE=0)在挂起循环的 pump 里备份 EGL 上下文、回前台时恢复;挂起期间
                    //    模拟器线程停在循环里,不跑 guest、不渲染,不需要额外的 GL 闸门;恢复失败
                    //    (SDL_RENDER_DEVICE_RESET)则存档后退出;
                    // ③ LogoLayer/LoadingScene 场景下不发生命周期回调(原版失活会 exit(0)),只挂起;
                    // ④ [补完 2026-09-15] 上游说的"音频不暂停":宿主 OpenAL 混音线程不随模拟器线程挂起,
                    //    挂起前把游戏的 CocosDenshion 静音、回前台再解除(不管发没发回调),循环音效不会在后台一直响。
                    // 挂起期间系统要结束应用(SDL_APP_TERMINATING / SDL_QUIT)→ 走 exit,先发失活 + 终止落盘。
                    // 流程、四个回调的反汇编依据与取舍见 ui_application::suspend_app。
                    log!("Handling app-will-resign-active event: suspending until foreground.");
                    ui_application::suspend_app(
                        env,
                        crate::window::SuspendEnd::Foreground,
                        "系统切后台",
                    );
                } else {
                    // [补完 2026-09-15] 注释更新:现在只有 iOS(以及应用选择器)还按上游退出——iOS 移植线的
                    // 真机呈现 / EAGL 生命周期没有验证过挂起后恢复。现有 exit 路径会先发
                    // resignActive + terminate,游戏的 saveToLocal:/saveSettings 能落盘。
                    // 桌面最小化见上面的 WindowMinimized 分支(桌面不会收到本事件)。
                    // Getting this event means touchHLE is becoming inactive, e.g.
                    // due to switching apps. The obvious way to handle this would
                    // be to just send `applicationWillResignActive:` to the
                    // UIApplicationDelegate. However:
                    // - touchHLE's event loop can't handle an inactive app well
                    //   right now. For example, audio isn't paused.
                    // - touchHLE's event loop can't handle the subsequent
                    //   termination of an app right now: it doesn't manage to send
                    //   the `applicationWillTerminate:` message in time. This can
                    //   mean loss of data!
                    // Therefore, for the moment we will simulate the early iOS
                    // behavior where switching app usually resulted in termination.
                    // We can usually handle this in time, so there won't be data
                    // loss, nor problems with background resource usage or audio.
                    // TODO: Handle this better.
                    log!("Handling app-will-resign-active event: exiting.");
                    ui_application::exit(env);
                }
            }
            Event::AppWillTerminate => {
                log!("Handling app-will-terminate event.");
                ui_application::exit(env);
            }
            Event::EnterDebugger => {
                if env.is_debugging_enabled() {
                    log!("Handling EnterDebugger event: entering debugger.");
                    env.enter_debugger(/* reason: */ None);
                } else {
                    log!("Ignoring EnterDebugger event: no debugger connected.");
                }
            }
            Event::TextInput(text_event) => {
                let responder = env.framework_state.uikit.ui_responder.first_responder;
                let class = msg![env; responder class];
                let ui_text_field_class = env.objc.get_known_class("UITextField", &mut env.mem);
                let ui_text_view_class = env.objc.get_known_class("UITextView", &mut env.mem);
                if !responder.is_null() && env.objc.class_is_subclass_of(class, ui_text_field_class)
                {
                    match text_event {
                        TextInputEvent::Text(text) => {
                            ui_view::ui_control::ui_text_field::handle_text(env, responder, text)
                        }
                        TextInputEvent::Backspace => {
                            ui_view::ui_control::ui_text_field::handle_backspace(env, responder)
                        }
                        TextInputEvent::Return => {
                            ui_view::ui_control::ui_text_field::handle_return(env, responder)
                        }
                    }
                } else if !responder.is_null()
                    && env.objc.class_is_subclass_of(class, ui_text_view_class)
                {
                    // [MoleWorld] UITextView(留言板/漂流瓶/好友留言)的输入路由。
                    match text_event {
                        TextInputEvent::Text(text) => {
                            ui_view::ui_scroll_view::ui_text_view::handle_text(env, responder, text)
                        }
                        TextInputEvent::Backspace => {
                            ui_view::ui_scroll_view::ui_text_view::handle_backspace(env, responder)
                        }
                        TextInputEvent::Return => {
                            ui_view::ui_scroll_view::ui_text_view::handle_return(env, responder)
                        }
                    }
                } else {
                    // [2026-09-16] B-01:没有聚焦的 UITextField/UITextView 时丢弃。window.rs 把回车/退格的
                    // KeyDown 无条件翻译成文本输入事件,未聚焦时按键也会走到这里,只留调试级日志。
                    log_dbg!(
                        "收到文本输入但 first_responder={:?} 不是输入框,丢弃",
                        responder
                    );
                }
            }
        }
    }

    ui_accelerometer::handle_accelerometer(env)
}
