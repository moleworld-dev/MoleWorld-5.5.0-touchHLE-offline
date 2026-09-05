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
}

/// For use by `NSRunLoop`: handles any events that have queued up.
///
/// Returns the next time this function must be called, if any, e.g. the next
/// time an accelerometer input is due.
pub fn handle_events(env: &mut Environment) -> Option<Instant> {
    use crate::window::Event;
    use crate::window::TextInputEvent;

    // [MoleWorld DIAG] Inject a synthetic tap from /tmp/mole_input so the game
    // can be driven without host input (the window is on its own macOS Space and
    // can't be clicked via the host). One Down/Up step per call; coordinates are
    // guest screen points.
    if let Some(inject) = crate::mole_diag::next_inject() {
        match inject {
            crate::mole_diag::Inject::Menu => crate::mole_menu::toggle(env),
            crate::mole_diag::Inject::Down(x, y) => {
                if crate::mole_menu::is_open() {
                    crate::mole_menu::handle_touch(env, x, y);
                } else {
                    ui_touch::handle_event(
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
                    ui_touch::handle_event(
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
                    ui_touch::handle_event(
                        env,
                        Event::TouchesUp(std::collections::HashMap::from([(
                            crate::window::FingerId::Mouse,
                            (x, y),
                        )])),
                    );
                }
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
            Event::ToggleMoleMenu => crate::mole_menu::toggle(env),
            // While the menu is open, route touches to it instead of the game.
            Event::TouchesDown(ref map) if crate::mole_menu::is_open() => {
                if let Some((_, &(x, y))) = map.iter().next() {
                    crate::mole_menu::handle_touch(env, x, y);
                }
            }
            Event::TouchesMove(..) | Event::TouchesUp(..) if crate::mole_menu::is_open() => {
                // Swallow move/up while the menu is open.
            }
            Event::TouchesDown(..) | Event::TouchesMove(..) | Event::TouchesUp(..) => {
                ui_touch::handle_event(env, event)
            }
            // [MoleWorld iOS] On iOS, losing focus (Control Center, home-indicator,
            // notification) or going to the background must PAUSE — not kill — the
            // running game. Desktop/Android keep the old exit-on-resign behavior
            // (they don't drive the background/foreground lifecycle events below).
            #[cfg(target_os = "ios")]
            Event::AppWillResignActive => {
                // iOS `applicationWillResignActive:` — fires for ANY focus loss,
                // including foreground overlays (Control Center). The game saves +
                // pauses CCDirector in its own handler. Do NOT exit and do NOT gate
                // GL (still foreground → GL legal). A true background, if it
                // follows, arrives as AppDidEnterBackground.
                log!("Handling app-will-resign-active: pausing (game saves + pauses).");
                ui_application::resign_active(env);
            }
            #[cfg(not(target_os = "ios"))]
            Event::AppWillResignActive => {
                // Desktop/Android can't yet run an inactive app well, so simulate
                // early-iOS "switching away = terminate" (exit() saves first, so no
                // data loss).
                log!("Handling app-will-resign-active event: exiting.");
                ui_application::exit(env);
            }
            #[cfg(target_os = "ios")]
            Event::AppDidEnterBackground => {
                // iOS `applicationDidEnterBackground:` — TRUE background. After this,
                // any GL call kills us; gate GL first, then deliver the message (the
                // game calls `stopAnimation`).
                log!("Handling app-did-enter-background: gating GL, suspending render.");
                ui_application::did_enter_background(env);
            }
            #[cfg(target_os = "ios")]
            Event::AppWillEnterForeground => {
                // iOS `applicationWillEnterForeground:` — leaving background. Ungate
                // GL first (legal again) so `startAnimation` renders, then deliver.
                log!("Handling app-will-enter-foreground: ungating GL, resuming render.");
                ui_application::will_enter_foreground(env);
            }
            #[cfg(target_os = "ios")]
            Event::AppDidBecomeActive => {
                // iOS `applicationDidBecomeActive:` — every resume (overlay dismissal
                // AND background return). Resume + clear the gate (idempotent).
                log!("Handling app-did-become-active: resuming game.");
                ui_application::did_become_active(env);
            }
            #[cfg(not(target_os = "ios"))]
            Event::AppDidEnterBackground
            | Event::AppWillEnterForeground
            | Event::AppDidBecomeActive => {
                // These lifecycle transitions are only driven on iOS.
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
                } else {
                    // [MoleWorld 改名诊断] 收到文本输入但没有聚焦的 UITextField → 输入被丢弃。
                    // 改名时若一直走这里,说明 becomeFirstResponder 没触发(tap 没命中输入框)。
                    log!(
                        "[改名诊断] 收到文本输入但 first_responder={:?} 不是 UITextField,输入被丢弃",
                        responder
                    );
                }
            }
        }
    }

    ui_accelerometer::handle_accelerometer(env)
}
