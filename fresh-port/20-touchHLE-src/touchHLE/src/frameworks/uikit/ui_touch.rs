/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITouch`.

use super::ui_event;
use crate::frameworks::core_graphics::{CGPoint, CGRect};
use crate::frameworks::foundation::{NSInteger, NSTimeInterval, NSUInteger};
use crate::mem::{GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain, ClassExports,
    HostObject, NSZonePtr, ObjC, SEL,
};
use crate::window::{Coords, Event, FingerId};
use crate::Environment;
use std::collections::hash_map::{Entry, HashMap};
use std::collections::HashSet;

pub type UITouchPhase = NSInteger;
pub const UITouchPhaseBegan: UITouchPhase = 0;
pub const UITouchPhaseMoved: UITouchPhase = 1;
pub const UITouchPhaseStationary: UITouchPhase = 2;
pub const UITouchPhaseEnded: UITouchPhase = 3;
/// [复核修 2026-09-15] R1-3:取消阶段,取值与 UIKit 一致。游戏二进制里没有 -phase 的 selref(不读阶段),
/// 只有宿主侧代码(滚动视图/手势识别器的残留触点判断)会看到这个值。
pub const UITouchPhaseCancelled: UITouchPhase = 4;

#[derive(Default)]
pub struct State {
    current_touches: HashMap<FingerId, id>,
}

pub(super) struct UITouchHostObject {
    /// Strong reference to the `UIView`
    pub(super) view: id,
    /// Strong reference to the `UIWindow`, used as a reference for co-ordinate
    /// space conversion
    pub(super) window: id,
    /// Relative to the screen
    location: CGPoint,
    /// Relative to the screen
    previous_location: CGPoint,
    timestamp: NSTimeInterval,
    phase: UITouchPhase,
}
impl HostObject for UITouchHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITouch: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UITouchHostObject {
        view: nil,
        window: nil,
        location: CGPoint { x: 0.0, y: 0.0 },
        previous_location: CGPoint { x: 0.0, y: 0.0 },
        timestamp: 0.0,
        phase: UITouchPhaseBegan,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())dealloc {
    let &mut UITouchHostObject { view, window, .. } = env.objc.borrow_mut(this);
    release(env, view);
    release(env, window);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (CGPoint)locationInView:(id)that_view { // UIView*
    let &UITouchHostObject { location, window, .. } = env.objc.borrow(this);
    let location_in_window: CGPoint = msg![env; window convertPoint:location fromWindow:nil];
    if that_view == nil || !view_is_in_window(env, that_view, window) {
        location_in_window
    } else {
        msg![env; that_view convertPoint:location_in_window fromView:window]
    }
}
- (CGPoint)previousLocationInView:(id)that_view { // UIView*
    let &UITouchHostObject { previous_location, window, .. } = env.objc.borrow(this);
    let location_in_window: CGPoint = msg![env; window convertPoint:previous_location fromWindow:nil];
    if that_view == nil || !view_is_in_window(env, that_view, window) {
        location_in_window
    } else {
        msg![env; that_view convertPoint:location_in_window fromView:window]
    }
}

- (id)view {
    env.objc.borrow::<UITouchHostObject>(this).view
}

- (NSTimeInterval)timestamp {
    env.objc.borrow::<UITouchHostObject>(this).timestamp
}

- (NSUInteger)tapCount {
    1 // TODO: support double-taps etc
}

- (UITouchPhase)phase {
    env.objc.borrow::<UITouchHostObject>(this).phase
}

@end

};

/// [super::handle_events] will forward touch events to this function.
/// [复核修 2026-09-15] locationInView:/previousLocationInView: 的前置检查:视图是否仍在触摸所属窗口里。
/// 游戏代码可能在触摸过程中把视图移出窗口后再对它取坐标(例如 -[TMAUserIDListView touchesEnded:withEvent:]
/// @0x4e3b30 先 removeFromSuperview),这时两边图层没有公共祖先,convertPoint:fromView: 会在
/// ca_layer.rs 里 panic。离窗时退化为返回窗口坐标(真机上结果同样无意义,但不会崩)。
fn view_is_in_window(env: &mut Environment, view: id, window: id) -> bool {
    if window == nil {
        return false;
    }
    let view_window: id = msg![env; view window];
    if view_window == window || view == window {
        true
    } else {
        log_dbg!(
            "UITouch 取坐标:视图 {:?} 不在触摸窗口 {:?} 里(当前窗口 {:?}),退化为窗口坐标",
            view,
            window,
            view_window
        );
        false
    }
}

pub fn handle_event(env: &mut Environment, event: Event) {
    // before processing anything, we mark all current touches as stationary
    let current_touches = &env.framework_state.uikit.ui_touch.current_touches;
    for &touch in (*current_touches).values() {
        env.objc.borrow_mut::<UITouchHostObject>(touch).phase = UITouchPhaseStationary;
    }
    match event {
        Event::TouchesDown(map) => handle_touches_down(env, map),
        Event::TouchesMove(map) => handle_touches_move(env, map),
        Event::TouchesUp(map) => handle_touches_up(env, map),
        // [复核修 2026-09-15] R1-3:取消(目前只由 window.rs 结束滚轮虚拟捏合时发出)。
        Event::TouchesCancel(map) => handle_touches_cancelled(env, map),
        _ => unreachable!(),
    }
}

fn handle_touches_down(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    // UIKit creates and drains autorelease pools when handling events.
    let pool: id = msg_class![env; NSAutoreleasePool new];

    // Note: if the emulator is heavily lagging, this timestamp is going
    // to be far off from the truth, since it should represent the
    // time when the event actually happened, not the time when the
    // event was dispatched. Maybe we'll need to fix this eventually.
    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    // [扫描修 2026-09-15] 已按下的手指又收到 Down:先记下来,本次 Down 分发完再按移动处理。
    let mut repeated_downs: HashMap<FingerId, Coords> = HashMap::new();
    // [扫描修 2026-09-15] 本次新建的滚轮虚拟捏合手指(FingerId::PinchA/PinchB)对应的 UITouch。
    let mut pinch_touches: HashSet<id> = HashSet::new();

    for (finger_id, coords) in map {
        let current_touches = &mut env.framework_state.uikit.ui_touch.current_touches;

        if current_touches.contains_key(&finger_id) {
            // [扫描修 2026-09-15] 原来这里 assert_eq!(current_touches.len(), 1) 后直接 return:
            // ① 多指并存(左键 / 真触屏 / 滚轮虚拟捏合手指同时按住)时断言直接 panic;
            // ② 提前 return 会丢掉同一批里的其它新手指,还漏掉上面 autorelease pool 的 release。
            // 改为容错:记下这根手指,本次 Down 正常分发完后统一当作移动处理。
            log!(
                "Warning: New touch {:?} initiated but current touch did not end yet, treating as movement.",
                finger_id
            );
            repeated_downs.insert(finger_id, coords);
            continue;
        }

        log_dbg!("Finger {:?} touch down: {:?}", finger_id, coords);

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        // TODO: is this the correct state of the UITouch and UIEvent during
        //       hit testing?

        let new_touch: id = msg_class![env; UITouch alloc];
        *env.objc.borrow_mut(new_touch) = UITouchHostObject {
            view: nil,
            window: nil,
            location,
            previous_location: location,
            timestamp,
            phase: UITouchPhaseBegan,
        };
        autorelease(env, new_touch);
        if matches!(finger_id, FingerId::PinchA | FingerId::PinchB) {
            pinch_touches.insert(new_touch);
        }

        let _: () = msg![env; touches addObject:new_touch];

        let _ = &env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .insert(finger_id, new_touch);
        retain(env, new_touch);
    }

    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    // views with existing touches (see isMultipleTouchEnabled check below)
    let views_with_existing_touches: HashSet<id> = env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .values()
        .map(|&touch| env.objc.borrow::<UITouchHostObject>(touch).view)
        .collect();

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    let touches_arr: id = msg![env; touches allObjects];
    let touches_count: NSUInteger = msg![env; touches_arr count];
    for i in 0..touches_count {
        let touch: id = msg![env; touches_arr objectAtIndex:i];
        let &UITouchHostObject { location, .. } = env.objc.borrow(touch);

        // Assumes the windows in the list are ordered back-to-front.
        // TODO: this may not be correct once we support windowLevel.
        let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
        let Some((window, location_in_window)) = windows.into_iter().rev().find_map(|window| {
            let location_in_window: CGPoint =
                msg![env; window convertPoint:location fromWindow:nil];
            if msg![env; window pointInside:location_in_window withEvent:event] {
                Some((window, location_in_window))
            } else {
                None
            }
        }) else {
            log!(
                "Couldn't find a window for touch at {:?}, discarding",
                location,
            );
            continue;
        };

        let view: id = msg![env; window hitTest:location_in_window withEvent:event];
        if view == nil {
            log!(
                "Couldn't find a view for touch at {:?} in window {:?}, discarding",
                location_in_window,
                window,
            );
            continue;
        } else {
            log_dbg!(
                "Found view {:?} with frame {:?} for touch at {:?} in window {:?}",
                view,
                {
                    let f: CGRect = msg![env; view frame];
                    f
                },
                location_in_window,
                window,
            );
        }

        let is_multi_touch_enabled: bool = msg![env; view isMultipleTouchEnabled];
        if !is_multi_touch_enabled && pinch_touches.contains(&touch) {
            // [扫描修 2026-09-15] 滚轮合成的虚拟捏合手指只投给允许多点触控的视图(游戏的 EAGLView,
            // -[iMoleVillageAppDelegate applicationDidFinishLaunching:] 里 setMultipleTouchEnabled:YES)。
            // 落在不支持多点触控的 UIKit 视图(列表、输入框等)上时不投递,否则第一根虚拟手指会被当成
            // 单指拖动,滚一下滚轮列表就被拖走。处理方式与下面相同:触点照常跟踪到抬起,但 view 为 nil。
            log_dbg!(
                "Ignoring virtual pinch touch {:?} for view {:?}, !isMultipleTouchEnabled",
                touch,
                view
            );
            continue;
        }
        if !is_multi_touch_enabled {
            // When a view has multi-touch disabled, it can only have one active
            // touch at once. So, we can only report a new touch to the view if
            // there are no other touches currently associated with it, and if
            // there are multiple new touches for this view, we can only report
            // one of them.
            let view_has_other_new_touches = view_touches.contains_key(&view);
            let view_has_existing_touches = views_with_existing_touches.contains(&view);
            if view_has_other_new_touches || view_has_existing_touches {
                log!(
                    "Ignoring new touch {:?} for view {:?}, !isMultipleTouchEnabled",
                    touch,
                    view
                );
                // The touch will continue to be tracked until it ends, but the
                // view will be nil, so messages sent to it will be ignored.
                // TODO: Figure out if/how these should be delivered elsewhere
                //       in the responder chain.
                // FIXME: The fact the view is nil might be observed via
                //        touchesForView:nil or allTouches on UIEvent.
                //        This might cause problems. What does the real OS do?
                //        Does this need to be prevented?
                continue;
            }
        }

        // Only create the set after the isMultipleTouchEnabled checks so we
        // won't end up with an empty set.
        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];

        retain(env, view);
        retain(env, window);
        {
            let new_touch = env.objc.borrow_mut::<UITouchHostObject>(touch);
            new_touch.view = view;
            new_touch.window = window;
        }
    }

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesBegan:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesBegan:touches withEvent:event];
    }

    release(env, pool);

    // [扫描修 2026-09-15] 重复 Down 的手指按移动处理。它们的 phase 已在 handle_event 开头置为
    // Stationary,本函数没有改动,满足 handle_touches_move 里的断言。
    if !repeated_downs.is_empty() {
        handle_touches_move(env, repeated_downs);
    }
}

fn handle_touches_move(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    for (finger_id, coords) in map {
        let Some(&touch) = env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .get(&finger_id)
        else {
            log!(
                "Warning: Finger {:?} touch move event received but no current touch, ignoring.",
                finger_id
            );
            continue;
        };

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        let view = env.objc.borrow::<UITouchHostObject>(touch).view;
        let host_object = env.objc.borrow_mut::<UITouchHostObject>(touch);

        if host_object.location == location {
            continue;
        }

        log_dbg!("Finger {:?} touch move: {:?}", finger_id, coords);

        host_object.previous_location = host_object.location;
        host_object.location = location;
        host_object.timestamp = timestamp;
        assert_eq!(host_object.phase, UITouchPhaseStationary);
        host_object.phase = UITouchPhaseMoved;

        let _: () = msg![env; touches addObject:touch];

        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];
    }

    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesMoved:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesMoved:touches withEvent:event];
    }

    release(env, pool);
}

fn handle_touches_up(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];

    // We need to construct all touches set _BEFORE_ removing touches!
    // (as removed one are reported as the part of the event)
    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();

    for (finger_id, coords) in map {
        let Some(&touch) = env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .get(&finger_id)
        else {
            log!(
                "Warning: Finger {:?} touch up event received but no current touch, ignoring.",
                finger_id
            );
            continue;
        };

        log_dbg!("Finger {:?} touch up: {:?}", finger_id, coords);

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        let view = env.objc.borrow::<UITouchHostObject>(touch).view;
        let host_object = env.objc.borrow_mut::<UITouchHostObject>(touch);
        host_object.previous_location = host_object.location;
        host_object.location = location;
        host_object.timestamp = timestamp;
        assert_eq!(host_object.phase, UITouchPhaseStationary);
        host_object.phase = UITouchPhaseEnded;

        let _: () = msg![env; touches addObject:touch];

        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];

        let _ = &env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .remove(&finger_id);
        release(env, touch); // only owner now should be the NSSet
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    for (view, touches) in view_touches {
        log_dbg!(
            "Sending [{:?} touchesEnded:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg![env; view touchesEnded:touches withEvent:event];
    }

    release(env, pool);
}

/// [复核修 2026-09-15] R1-3:触摸被取消。与 [handle_touches_up] 一样结束触点跟踪,但阶段记为 Cancelled,
/// 并给视图发 touchesCancelled:withEvent:(真机 UIKit 在来电、手势识别器接管等情况下也这样结束触摸)。
/// 目前只有 window.rs 结束滚轮虚拟捏合时发出。游戏侧:-[EAGLView touchesCancelled:withEvent:]@0x2f7750 转给
/// -[CCTouchDispatcher touchesCancelled:withEvent:]@0x2f62ec(类型 3):目标代理走 ccTouchCancelled:
/// (CCMenu@0x2ceb10 只 unselected,不 activate),标准代理走 ccTouchesCancelled:(VillageLayer@0x3558c /
/// InGameLayer@0x2403f8 → processTouch:withType:3,各子处理器只对类型 2 做点击)。
/// [复核修 2026-09-15] R1-3 返修:游戏自己的取消处理不完整(真机上取消极少,原版没踩到;滚轮捏合以取消
/// 结束后就很容易踩到),给 EAGLView 发取消的前后各补一段 cocos2d 收尾:发之前清"不认取消"的目标代理的
/// 门控([cocos2d_prepare_cancel]),发完清村庄 ObjSelector 的残留标志([reset_obj_selector_after_cancel])。
fn handle_touches_cancelled(env: &mut Environment, map: HashMap<FingerId, Coords>) {
    let pool: id = msg_class![env; NSAutoreleasePool new];

    let timestamp: NSTimeInterval = {
        let process_info = msg_class![env; NSProcessInfo processInfo];
        msg![env; process_info systemUptime]
    };

    // 与抬起相同:必须在移除触点【之前】收集全部触点(被取消的触点也属于本次事件)。
    let all_touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
    for &touch in env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .clone()
        .values()
    {
        let _: () = msg![env; all_touches addObject:touch];
    }

    // view to set of touches for this view
    let mut view_touches: HashMap<id, id> = HashMap::new();
    // [复核修 2026-09-15] R1-3 返修:视图 → 本次被取消的触点(与 view_touches 里的集合一一对应)。
    let mut view_touch_lists: HashMap<id, Vec<id>> = HashMap::new();

    for (finger_id, coords) in map {
        let Some(&touch) = env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .get(&finger_id)
        else {
            log!(
                "Warning: Finger {:?} touch cancel event received but no current touch, ignoring.",
                finger_id
            );
            continue;
        };

        log_dbg!("Finger {:?} touch cancelled: {:?}", finger_id, coords);

        let location = CGPoint {
            x: coords.0,
            y: coords.1,
        };

        let view = env.objc.borrow::<UITouchHostObject>(touch).view;
        let host_object = env.objc.borrow_mut::<UITouchHostObject>(touch);
        host_object.previous_location = host_object.location;
        host_object.location = location;
        host_object.timestamp = timestamp;
        // 不像抬起那样断言 Stationary:取消只做收尾,不该因为阶段异常让模拟器 panic。
        host_object.phase = UITouchPhaseCancelled;

        if let Entry::Vacant(e) = view_touches.entry(view) {
            let touches: id = msg_class![env; NSMutableSet allocWithZone:(MutVoidPtr::null())];
            e.insert(touches);
        }
        let touches: id = *view_touches.get(&view).unwrap();
        let _: () = msg![env; touches addObject:touch];
        // [复核修 2026-09-15] R1-3 返修:同样按视图记一份触点列表,给 cocos2d 收尾查 claimedTouches 用。
        view_touch_lists.entry(view).or_default().push(touch);

        let _ = &env
            .framework_state
            .uikit
            .ui_touch
            .current_touches
            .remove(&finger_id);
        release(env, touch); // only owner now should be the NSSet
    }

    let event = ui_event::new_event(env, all_touches);
    autorelease(env, event);

    let cancel_sel: SEL = env
        .objc
        .register_host_selector("touchesCancelled:withEvent:".to_string(), &mut env.mem);
    // [复核修 2026-09-15] R1-3 返修:本次取消送到了走 GameManager/NewGameManager 的村庄层,发完要清 ObjSelector 残留。
    let mut village_cancel = false;
    for (view, touches) in view_touches {
        // 开始时没投递给任何视图的触点(view 为 nil,见 handle_touches_down)无处可发。
        if view == nil {
            continue;
        }
        let responds: bool = msg![env; view respondsToSelector:cancel_sel];
        if !responds {
            // 命中视图都是 UIView 子类,宿主 UIView 实现了 touchesCancelled:withEvent:,正常走不到这里。
            // 刻意不退回 touchesEnded::那样会把取消重新变回一次点击。
            log!(
                "[复核修 2026-09-15] 视图 {:?} 不响应 touchesCancelled:withEvent:,取消消息丢弃",
                view
            );
            continue;
        }
        // [复核修 2026-09-15] R1-3 返修:发取消之前做 cocos2d 收尾(此时分发器的 claimedTouches 还没移除触点,
        // 才认得出是谁认领了它)。
        if let Some(list) = view_touch_lists.get(&view) {
            village_cancel |= cocos2d_prepare_cancel(env, view, list);
        }
        log_dbg!(
            "Sending [{:?} touchesCancelled:{:?} withEvent:{:?}]",
            view,
            touches,
            event
        );
        let _: () = msg_send(env, (view, cancel_sel, touches, event));
    }
    if village_cancel {
        reset_obj_selector_after_cancel(env);
    }

    release(env, pool);
}

/// [复核修 2026-09-15] R1-3 返修(复核 issue 1):不认取消、却只靠 ccTouchEnded: 清门控的 cocos2d 目标代理
/// → 门控 ivar(都是 int)。这些类实现了 ccTouchBegan:/ccTouchEnded:,但自己和父类链(CCNode,或
/// CCLayerColor → CCLayer → CCNode)都没有 ccTouchCancelled:withEvent:(整个二进制只有 CCMenu、CCScrollView、
/// InviteFriendsLayer 实现)。CCTouchDispatcher 注册时按 respondsToSelector: 置 enabledSelectors
/// (-[CCTargetedTouchHandler initWithDelegate:priority:swallowsTouches:]@0x2f6712),取消时只把触点移出
/// claimedTouches(0x2f5ed2-0x2f5ee2),取消位没置就不回调(0x2f5eb6),门控于是永远停在 1:
/// - OutputHanlder.state_:ccTouchBegan@0x14ca9a 非 0 直接返回 NO,认领时 @0x14cb0a 置 1,只有
///   ccTouchEnded@0x14cb46 清零;onEnter@0x14b2fe 以优先级 0 注册并吞触摸。卡住后建筑头顶的产出图标
///   再也收不了(触摸落到下面的 ObjSelector,变成打开建筑),直到它被重建。
/// - TreasureRewardLayer._touchState:ccTouchBegan@0x4157ca 判断、@0x4157ee 置 1,ccTouchEnded@0x4158bc 清零;
///   可见时认领任意位置的触摸(优先级 -128)。卡住后宝箱/兔子奖励领不了,层一直留在屏幕上。
/// - FinalRewardAnimation._touchState:@0x2da896 判断、@0x2da8ba 置 1,ccTouchEnded@0x2da8e0 清零;秋季终奖同上。
/// 核查:列出实现 ccTouchBegan: 却不认取消的类,取 Began/Ended 共同读写的 ivar,门控只有这三处
/// (BugGame.m_level/level4、CutFruit.m_touchLayer 只是选关 / 子层引用,不是门控)。
/// 取舍:清零等于 ccTouchEnded: 里"只清零、不调 processTouched / onSpriteTouched:"的那半截,不收产出、
/// 不领奖励;不改成给它们补发 ccTouchEnded:(那样滚一下滚轮就收产出、领奖励,等于把取消又变回点击)。
/// 以后发现新的同类门控,往表里加一行即可;表外不认取消的代理照旧不回调。
const COCOS_CANCEL_GATES: &[(&str, &str)] = &[
    ("OutputHanlder", "state_"),
    ("TreasureRewardLayer", "_touchState"),
    ("FinalRewardAnimation", "_touchState"),
];

/// [复核修 2026-09-15] R1-3 返修(复核 issue 2):走 GameManager / NewGameManager 的村庄层(标准代理)。
/// -[VillageLayer ccTouchesCancelled:withEvent:]@0x3558c → [GameManager processTouch:withType:3];
/// -[InGameLayer ccTouchesCancelled:withEvent:]@0x2403f8(节日村 HolidayVillageLayer 等子类继承它)→
/// [NewGameManager processTouch:withType:3]。两者单指时都会转给 ObjSelector,见 [reset_obj_selector_after_cancel]。
const COCOS_VILLAGE_LAYERS: &[&str] = &["VillageLayer", "InGameLayer"];

/// [复核修 2026-09-15] R1-3 返修:沿对象的 isa/superclass 链找第一个名字在 names 里的类(不发消息)。
/// 只用于游戏自己的对象(EAGLView、CCTouchDispatcher、cocos2d 节点),它们的类链都是真实类;遇到 NSObject 就停。
fn class_chain_match(env: &Environment, obj: id, names: &[&'static str]) -> Option<&'static str> {
    if obj == nil {
        return None;
    }
    let mut class = ObjC::read_isa(obj, &env.mem);
    for _ in 0..32 {
        if class == nil {
            return None;
        }
        let name = env.objc.try_get_class_name(class)?;
        if let Some(&hit) = names.iter().find(|&&n| n == name) {
            return Some(hit);
        }
        if name == "NSObject" {
            return None;
        }
        class = env.objc.get_superclass(class);
    }
    None
}

/// [复核修 2026-09-15] R1-3 返修:按名字读对象型 ivar(沿类链查,偏移取运行时值,不写死)。没有这个 ivar 返回 None。
fn read_id_ivar(env: &Environment, obj: id, name: &str) -> Option<id> {
    if obj == nil {
        return None;
    }
    let ptr = env
        .objc
        .object_lookup_ivar(&env.mem, obj, &name.to_string())?;
    let bits: GuestUSize = env.mem.read(ptr);
    Some(Ptr::from_bits(bits))
}

/// [复核修 2026-09-15] R1-3 返修:给 EAGLView 发 touchesCancelled:withEvent: 之前的 cocos2d 收尾。
/// ① 目标代理:被取消的触点在某个 CCTargetedTouchHandler 的 claimedTouches 里、代理又不认
///    ccTouchCancelled:withEvent: 时,按 [COCOS_CANCEL_GATES] 把代理的门控清零。必须在发取消之前做:
///    分发器处理取消时会把触点移出 claimedTouches,之后就认不出认领者了;它对这类代理不回调,先清不影响分发。
///    CCMenu 等认取消的照常交给分发器走 ccTouchCancelled:(只 unselected,不 activate)。
/// ② 标准代理里有 [COCOS_VILLAGE_LAYERS] 时返回 true,由调用方发完取消后清 ObjSelector 残留。
/// 在运行循环处理事件时调用(不在 drawScene/mainLoop 帧栈上);只读 ivar、发宿主实现的
/// count / objectAtIndex: / containsObject: / respondsToSelector:,不跑游戏代码。
fn cocos2d_prepare_cancel(env: &mut Environment, view: id, cancelled: &[id]) -> bool {
    // -[EAGLView touchesCancelled:withEvent:]@0x2f7750 把取消转给 touchDelegate_(CCTouchDispatcher)。
    if cancelled.is_empty() || class_chain_match(env, view, &["EAGLView"]).is_none() {
        return false;
    }
    let Some(dispatcher) = read_id_ivar(env, view, "touchDelegate_") else {
        return false;
    };
    if class_chain_match(env, dispatcher, &["CCTouchDispatcher"]).is_none() {
        return false;
    }

    // ① 目标代理(-[CCTouchDispatcher init]@0x2f558c 建的 NSMutableArray,宿主实现)。
    let targeted = read_id_ivar(env, dispatcher, "targetedHandlers").unwrap_or(nil);
    if targeted != nil {
        let cancel_sel: SEL = env
            .objc
            .register_host_selector("ccTouchCancelled:withEvent:".to_string(), &mut env.mem);
        let count: NSUInteger = msg![env; targeted count];
        for i in 0..count {
            let handler: id = msg![env; targeted objectAtIndex:i];
            let claimed = read_id_ivar(env, handler, "claimedTouches").unwrap_or(nil);
            let delegate = read_id_ivar(env, handler, "delegate").unwrap_or(nil);
            if claimed == nil || delegate == nil {
                continue;
            }
            let mut claims_cancelled = false;
            for &touch in cancelled {
                let contains: bool = msg![env; claimed containsObject:touch];
                if contains {
                    claims_cancelled = true;
                    break;
                }
            }
            if !claims_cancelled {
                continue;
            }
            let responds: bool = msg![env; delegate respondsToSelector:cancel_sel];
            if responds {
                continue;
            }
            let mut gate: Option<(&'static str, &'static str)> = None;
            for &(class, ivar) in COCOS_CANCEL_GATES {
                if class_chain_match(env, delegate, &[class]).is_some() {
                    gate = Some((class, ivar));
                    break;
                }
            }
            let Some((class, ivar)) = gate else {
                log_dbg!(
                    "[复核修 2026-09-15] 取消触摸:目标代理 {:?} 不认 ccTouchCancelled:withEvent:,不在门控表里,不回调",
                    delegate
                );
                continue;
            };
            let Some(ptr) = env
                .objc
                .object_lookup_ivar(&env.mem, delegate, &ivar.to_string())
            else {
                log!(
                    "[复核修 2026-09-15] 取消触摸:{} {:?} 找不到门控 ivar {},跳过",
                    class,
                    delegate,
                    ivar
                );
                continue;
            };
            let old: GuestUSize = env.mem.read(ptr);
            if old != 0 {
                env.mem.write(ptr, 0);
            }
            log!(
                "[复核修 2026-09-15] 取消触摸:{} {:?} 认领了被取消的触点但不认 ccTouchCancelled:withEvent:,门控 {} 由 {} 清零(不收产出/不领奖励)",
                class,
                delegate,
                ivar,
                old
            );
        }
    }

    // ② 标准代理里有没有村庄层。
    let standard = read_id_ivar(env, dispatcher, "standardHandlers").unwrap_or(nil);
    if standard != nil {
        let count: NSUInteger = msg![env; standard count];
        for i in 0..count {
            let handler: id = msg![env; standard objectAtIndex:i];
            let delegate = read_id_ivar(env, handler, "delegate").unwrap_or(nil);
            if class_chain_match(env, delegate, COCOS_VILLAGE_LAYERS).is_some() {
                return true;
            }
        }
    }
    false
}

/// [复核修 2026-09-15] R1-3 返修(复核 issue 2):取消后清村庄 ObjSelector 的单次触摸标志。
/// 捏合的一根虚拟手指被 CCMenu 等目标代理吞掉时,另一根以单指身份进 GameManager / NewGameManager:
/// -[ObjSelector touchBegan:] 在 0x4b268 置 isSelected=1,touchMove: 在 0x4b27e 置 isMoved=1;取消(类型 3)
/// ObjSelector 什么也不做(processTouch:withType:@0x4af9a、specialObjectProcessTouch:withType:@0x4b024 只处理
/// 0/1/2),而 touchBegan: 从不清 isMoved,只有 touchEnd: 末尾 0x4bcee/0x4bcf2(specialObjectTouchEnd: 末尾同理)
/// 清两个标志。于是下一次正常点建筑时 touchEnd: 在 0x4b2c8 看到 isMoved=1,直接跳去清零、不选中,这次点击丢失。
/// 这里补上 touchEnd: 那段"不选中、只清零"的收尾。
/// 复核说节日村 HolidayVillageLayer 没有 ccTouchesCancelled:、NewGameManager 收不到类型 3——不对:它继承
/// InGameLayer 的实现(@0x2403f8),分发器按 respondsToSelector: 置位(@0x2f6580)照样回调;但 NewGameManager
/// 单指时同样转给 ObjSelector(@0x2456d2),残留一样,这里一并清掉。
/// 只在没有别的手指还按着时清(别的手指可能正在点建筑,标志归它)。+[ObjSelector instance]@0x4adb4 是原版
/// 单例取法;调用方已确认标准代理里有 VillageLayer/InGameLayer(本游戏的类),类一定存在。
fn reset_obj_selector_after_cancel(env: &mut Environment) {
    if !env
        .framework_state
        .uikit
        .ui_touch
        .current_touches
        .is_empty()
    {
        return;
    }
    let class = env.objc.get_known_class("ObjSelector", &mut env.mem);
    let instance_sel: SEL = env
        .objc
        .register_host_selector("instance".to_string(), &mut env.mem);
    let selector: id = msg_send(env, (class, instance_sel));
    if selector == nil {
        return;
    }
    for name in ["isSelected", "isMoved"] {
        let Some(ptr) = env
            .objc
            .object_lookup_ivar(&env.mem, selector, &name.to_string())
        else {
            continue;
        };
        // 两个标志都是 BOOL(char,1 字节),不能按 4 字节写。
        let ptr: MutPtr<u8> = ptr.cast();
        let old: u8 = env.mem.read(ptr);
        if old != 0 {
            env.mem.write(ptr, 0u8);
            log!(
                "[复核修 2026-09-15] 取消触摸后清 ObjSelector.{}({} → 0),免得下一次点建筑被当成拖动丢掉",
                name,
                old
            );
        }
    }
}
