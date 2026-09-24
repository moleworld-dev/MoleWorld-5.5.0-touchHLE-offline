/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIGestureRecognizer` 与 `UITapGestureRecognizer`(最小可用实现)。
//!
//! [扫描修 2026-09-15] F8-2:原先整个 UIKit 目录没有手势识别器,游戏里
//! `[UITapGestureRecognizer alloc]` 得到 nil,`addGestureRecognizer:nil` 什么也不做。
//! 强制 VIP 后首次点「VIP功能」、漂流瓶首次教程弹出的 FeatureIntroductionView 里,
//! -[ATPagingView initWithFrame:]@0x38c474 给内部 UIScrollView 挂了单击识别器
//! (action = singleTapGestureCaptured:,delegate = ATPagingView 自己);VIP 教程第 3 页
//! "点击关闭"(-[WrapperManager imageWithIndexDidTouched:touchPoint:]@0x2644e8)只能靠它触发。
//!
//! 取舍:
//! - 只实现点击识别(UITapGestureRecognizer)。基类 UIGestureRecognizer 自身不识别任何手势
//!   (与 UIKit 一致,由子类实现)。UIPanGestureRecognizer 只被广告 SDK(PBWebSiteView)引用,
//!   保持不实现(alloc 仍得 nil)。
//! - touchHLE 的触摸不走 UIWindow sendEvent:(ui_touch.rs 直接发给命中视图),所以挂点放在
//!   UIView / UIScrollView 的 touches* 宿主实现里:响应链上第一个到达的宿主处理器调用
//!   [`process_touches`]。识别器的跟踪状态以"所跟踪的 UITouch"为键,同一次事件在响应链上被
//!   多个处理器重复处理是幂等的:按下阶段已登记的直接跳过;抬起/取消阶段第一次处理就把识别器
//!   移出活动表,后面的处理器找不到它。
//! - 识别成功且 cancelsTouchesInView=YES 时,调用方改为沿响应链转发 touchesCancelled:
//!   (UIKit 语义:命中视图收不到 touchesEnded:),并且**先转发取消、再派发 action**:action 可能
//!   同步拆掉整棵视图树(imageWithIndexDidTouched:touchPoint: 里同步 removeFeatureIntroductionView),
//!   先派发会让响应链断掉,cocos2d 的 EAGLView 收不到取消,CCMenu 会一直卡在跟踪态。
//! - 引用计数:target、delegate 不 retain(UIKit 语义);视图 retain 识别器、识别器的 view 是弱引用;
//!   活动表 retain 识别器和它跟踪的触摸;派发期间 retain 识别器,action 里整棵树被释放也不会悬垂。

use super::ui_touch::{UITouchPhaseCancelled, UITouchPhaseEnded};
use crate::frameworks::core_graphics::{CGFloat, CGPoint};
use crate::frameworks::foundation::{NSInteger, NSTimeInterval, NSUInteger};
use crate::objc::{
    id, msg, msg_send, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
    ObjC, SEL,
};
use crate::Environment;
use std::cell::RefCell;

pub type UIGestureRecognizerState = NSInteger;
pub const UIGestureRecognizerStatePossible: UIGestureRecognizerState = 0;
#[allow(dead_code)]
pub const UIGestureRecognizerStateBegan: UIGestureRecognizerState = 1;
#[allow(dead_code)]
pub const UIGestureRecognizerStateChanged: UIGestureRecognizerState = 2;
/// 离散手势(点击)识别成功即为 Ended(= UIGestureRecognizerStateRecognized)。
pub const UIGestureRecognizerStateEnded: UIGestureRecognizerState = 3;
#[allow(dead_code)]
pub const UIGestureRecognizerStateCancelled: UIGestureRecognizerState = 4;
pub const UIGestureRecognizerStateFailed: UIGestureRecognizerState = 5;

/// 点击允许的最大位移(pt,窗口坐标)。近似 UIKit 内部点击识别器的移动容差;
/// 在 UIScrollView 里,位移超过起拖阈值(10pt)时 scroll view 会主动让点击失败,实际容差更小。
const TAP_ALLOWABLE_MOVEMENT: CGFloat = 45.0;
/// 多击(numberOfTapsRequired > 1)时两击之间的最大间隔(秒)。游戏本身只用默认的单击。
const MULTI_TAP_MAX_INTERVAL: NSTimeInterval = 0.35;

/// 手势识别器挂点传入的触摸阶段。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum TouchStage {
    Began,
    Moved,
    Ended,
    Cancelled,
}

struct UIGestureRecognizerHostObject {
    /// (target, action) 列表。target 是弱引用(UIKit 不 retain target)。
    targets: Vec<(id, SEL)>,
    /// 委托,弱引用。
    delegate: id,
    /// 所挂视图,弱引用。视图 retain 识别器;视图摘下识别器或自身释放时清空。
    view: id,
    state: UIGestureRecognizerState,
    enabled: bool,
    cancels_touches_in_view: bool,
    /// 只存值,触摸延迟投递未实现。
    delays_touches_began: bool,
    /// 只存值,触摸延迟投递未实现。
    delays_touches_ended: bool,
    number_of_taps_required: NSUInteger,
    /// 只存值,只支持单指点击。
    number_of_touches_required: NSUInteger,
    /// 正在跟踪的触摸(强引用;在活动表里时非 nil)。
    tracked_touch: id,
    /// 本次按下位置(窗口坐标)。
    start_location: CGPoint,
    /// 最近一次触摸位置(窗口坐标),供 locationInView: 使用。
    last_location: CGPoint,
    /// 按下时触摸所在的窗口(弱引用,使用前与目标视图的窗口比对)。
    last_window: id,
    /// 多击计数。
    tap_count: NSUInteger,
    last_tap_end_timestamp: NSTimeInterval,
    last_tap_location: CGPoint,
}
impl HostObject for UIGestureRecognizerHostObject {}
impl Default for UIGestureRecognizerHostObject {
    fn default() -> Self {
        UIGestureRecognizerHostObject {
            targets: Vec::new(),
            delegate: nil,
            view: nil,
            state: UIGestureRecognizerStatePossible,
            enabled: true,
            cancels_touches_in_view: true,
            delays_touches_began: false,
            delays_touches_ended: true,
            number_of_taps_required: 1,
            number_of_touches_required: 1,
            tracked_touch: nil,
            start_location: CGPoint { x: 0.0, y: 0.0 },
            last_location: CGPoint { x: 0.0, y: 0.0 },
            last_window: nil,
            tap_count: 0,
            last_tap_end_timestamp: 0.0,
            last_tap_location: CGPoint { x: 0.0, y: 0.0 },
        }
    }
}

thread_local! {
    /// 正在跟踪触摸的识别器(每项持有一份强引用)。平时为空,
    /// 非按下阶段看到它为空就直接返回,不给普通触摸增加开销。
    static ACTIVE: RefCell<Vec<id>> = const { RefCell::new(Vec::new()) };
}

fn distance(a: CGPoint, b: CGPoint) -> CGFloat {
    (a.x - b.x).hypot(a.y - b.y)
}

fn is_tap_recognizer(env: &mut Environment, recognizer: id) -> bool {
    let class = ObjC::read_isa(recognizer, &env.mem);
    let tap_class = env
        .objc
        .get_known_class("UITapGestureRecognizer", &mut env.mem);
    env.objc.class_is_subclass_of(class, tap_class)
}

/// `object` 是否为 UIGestureRecognizer(含子类)。供 UIView addGestureRecognizer: 校验,
/// 避免把别的对象当识别器宿主对象 borrow 而 panic。
pub(super) fn is_gesture_recognizer(env: &mut Environment, object: id) -> bool {
    if object == nil {
        return false;
    }
    let class = ObjC::read_isa(object, &env.mem);
    let base_class = env.objc.get_known_class("UIGestureRecognizer", &mut env.mem);
    env.objc.class_is_subclass_of(class, base_class)
}

/// 活动表里正在跟踪 `touch` 的识别器(快照,不改引用计数)。
fn active_tracking(env: &Environment, touch: id) -> Vec<id> {
    let active: Vec<id> = ACTIVE.with(|a| a.borrow().clone());
    active
        .into_iter()
        .filter(|&recognizer| {
            env.objc
                .borrow::<UIGestureRecognizerHostObject>(recognizer)
                .tracked_touch
                == touch
        })
        .collect()
}

/// 把识别器移出活动表。返回它原先是否在表里;在的话,表持有的那份引用转交给调用方。
/// 注意:RefCell 借用只在闭包内,调用方拿到结果后再 release,避免 dealloc 链重入借用。
fn take_from_active(recognizer: id) -> bool {
    ACTIVE.with(|a| {
        let mut a = a.borrow_mut();
        if let Some(idx) = a.iter().position(|&r| r == recognizer) {
            a.remove(idx);
            true
        } else {
            false
        }
    })
}

/// 中止识别器正在进行的跟踪:回到 Possible、释放跟踪的触摸、移出活动表并释放表里那份引用。
fn reset_tracking(env: &mut Environment, recognizer: id) {
    let old_touch = {
        let host = env
            .objc
            .borrow_mut::<UIGestureRecognizerHostObject>(recognizer);
        host.state = UIGestureRecognizerStatePossible;
        host.tap_count = 0;
        std::mem::take(&mut host.tracked_touch)
    };
    let was_active = take_from_active(recognizer);
    release(env, old_touch);
    if was_active {
        release(env, recognizer);
    }
}

/// 清理跟踪着"已抬起"触摸的识别器。抬起/取消消息如果没经过任何宿主处理器
/// (例如命中视图是覆盖了 touchesEnded: 又不调 super 的游戏类),识别器会残留在活动表里。
fn sweep_stale(env: &mut Environment) {
    let active: Vec<id> = ACTIVE.with(|a| a.borrow().clone());
    for recognizer in active {
        let tracked = env
            .objc
            .borrow::<UIGestureRecognizerHostObject>(recognizer)
            .tracked_touch;
        let stale = if tracked == nil {
            true
        } else {
            let phase: NSInteger = msg![env; tracked phase];
            // [复核修 2026-09-15] 取消的触摸同样是残留。
            phase == UITouchPhaseEnded || phase == UITouchPhaseCancelled
        };
        if stale {
            reset_tracking(env, recognizer);
        }
    }
}

/// 委托 gestureRecognizer:shouldReceiveTouch:(游戏实现,例如 ATPagingView@0x38e130 恒返回 YES)。
fn delegate_should_receive_touch(env: &mut Environment, recognizer: id, touch: id) -> bool {
    let delegate = env
        .objc
        .borrow::<UIGestureRecognizerHostObject>(recognizer)
        .delegate;
    if delegate == nil {
        return true;
    }
    let sel = env.objc.register_host_selector(
        "gestureRecognizer:shouldReceiveTouch:".to_string(),
        &mut env.mem,
    );
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if !responds {
        return true;
    }
    msg_send(env, (delegate, sel, recognizer, touch))
}

/// 委托 gestureRecognizerShouldBegin:(可选)。
fn delegate_should_begin(env: &mut Environment, recognizer: id) -> bool {
    let delegate = env
        .objc
        .borrow::<UIGestureRecognizerHostObject>(recognizer)
        .delegate;
    if delegate == nil {
        return true;
    }
    let sel = env
        .objc
        .register_host_selector("gestureRecognizerShouldBegin:".to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if !responds {
        return true;
    }
    msg_send(env, (delegate, sel, recognizer))
}

fn touch_began(env: &mut Environment, touch: id) {
    sweep_stale(env);

    let touch_view: id = msg![env; touch view];
    if touch_view == nil {
        return;
    }
    let candidates = super::ui_view::gesture_recognizers_in_chain(env, touch_view);
    if candidates.is_empty() {
        return;
    }
    // 委托回调是游戏代码,可能把识别器从视图上摘掉并释放;处理期间先 retain 住。
    for &recognizer in &candidates {
        retain(env, recognizer);
    }

    let location: CGPoint = msg![env; touch locationInView:nil];
    let timestamp: NSTimeInterval = msg![env; touch timestamp];
    let window: id = msg![env; touch_view window];

    for &recognizer in &candidates {
        let (enabled, view, tracked, state) = {
            let host = env
                .objc
                .borrow::<UIGestureRecognizerHostObject>(recognizer);
            (host.enabled, host.view, host.tracked_touch, host.state)
        };
        if !enabled || view == nil || tracked == touch {
            // tracked == touch:同一次按下已被响应链上前一个宿主处理器登记过。
            continue;
        }
        if !is_tap_recognizer(env, recognizer) {
            continue;
        }
        if tracked != nil {
            // 另一根手指仍按着(已抬起的残留在 sweep_stale 里清掉了):只支持单指点击,判失败,
            // 留在活动表里直到那根手指抬起。
            if state != UIGestureRecognizerStateFailed {
                env.objc
                    .borrow_mut::<UIGestureRecognizerHostObject>(recognizer)
                    .state = UIGestureRecognizerStateFailed;
            }
            continue;
        }
        if !delegate_should_receive_touch(env, recognizer, touch) {
            continue;
        }
        {
            // 委托回调后重新确认(可能被禁用 / 摘下 / 已被重入的处理登记)。
            let host = env
                .objc
                .borrow::<UIGestureRecognizerHostObject>(recognizer);
            if !host.enabled || host.view == nil || host.tracked_touch != nil {
                continue;
            }
        }
        retain(env, touch);
        retain(env, recognizer); // 活动表持有的那份
        {
            let host = env
                .objc
                .borrow_mut::<UIGestureRecognizerHostObject>(recognizer);
            if host.tap_count > 0
                && (timestamp - host.last_tap_end_timestamp > MULTI_TAP_MAX_INTERVAL
                    || distance(location, host.last_tap_location) > TAP_ALLOWABLE_MOVEMENT)
            {
                host.tap_count = 0;
            }
            host.tracked_touch = touch;
            host.state = UIGestureRecognizerStatePossible;
            host.start_location = location;
            host.last_location = location;
            host.last_window = window;
        }
        ACTIVE.with(|a| a.borrow_mut().push(recognizer));
        log_dbg!(
            "[扫描修 2026-09-15] 手势识别器 {:?} 开始跟踪触摸 {:?}(视图 {:?})",
            recognizer,
            touch,
            view
        );
    }

    for recognizer in candidates {
        release(env, recognizer);
    }
}

fn touch_moved(env: &mut Environment, touch: id) {
    let trackers = active_tracking(env, touch);
    if trackers.is_empty() {
        return;
    }
    let location: CGPoint = msg![env; touch locationInView:nil];
    for recognizer in trackers {
        let host = env
            .objc
            .borrow_mut::<UIGestureRecognizerHostObject>(recognizer);
        host.last_location = location;
        if host.state != UIGestureRecognizerStateFailed
            && distance(location, host.start_location) > TAP_ALLOWABLE_MOVEMENT
        {
            host.state = UIGestureRecognizerStateFailed;
        }
    }
}

fn touch_ended(env: &mut Environment, touch: id, recognized: &mut Vec<id>) {
    let trackers = active_tracking(env, touch);
    if trackers.is_empty() {
        return;
    }
    let location: CGPoint = msg![env; touch locationInView:nil];
    let timestamp: NSTimeInterval = msg![env; touch timestamp];
    for recognizer in trackers {
        if !take_from_active(recognizer) {
            continue;
        }
        // 此后活动表那份引用归这里:识别成功就转交给 recognized,否则就地释放。
        let (success, old_touch) = {
            let host = env
                .objc
                .borrow_mut::<UIGestureRecognizerHostObject>(recognizer);
            let old_touch = std::mem::take(&mut host.tracked_touch);
            host.last_location = location;
            let moved_too_far = distance(location, host.start_location) > TAP_ALLOWABLE_MOVEMENT;
            if host.state == UIGestureRecognizerStateFailed
                || moved_too_far
                || !host.enabled
                || host.view == nil
            {
                host.state = UIGestureRecognizerStatePossible;
                host.tap_count = 0;
                (false, old_touch)
            } else {
                host.tap_count += 1;
                host.last_tap_end_timestamp = timestamp;
                host.last_tap_location = host.start_location;
                if host.tap_count >= host.number_of_taps_required.max(1) {
                    host.tap_count = 0;
                    host.state = UIGestureRecognizerStateEnded;
                    (true, old_touch)
                } else {
                    // 多击:等下一击
                    host.state = UIGestureRecognizerStatePossible;
                    (false, old_touch)
                }
            }
        };
        release(env, old_touch);
        if success {
            recognized.push(recognizer);
        } else {
            release(env, recognizer);
        }
    }
}

fn touch_cancelled(env: &mut Environment, touch: id) {
    for recognizer in active_tracking(env, touch) {
        reset_tracking(env, recognizer);
    }
}

/// 手势识别挂点:UIView / UIScrollView 的 touches* 宿主实现在转发前调用。
///
/// 返回本次抬起时识别成功、等待派发的识别器(每个持有一份强引用)。只有 `Ended` 阶段可能非空;
/// 非空时调用方必须先按 [`cancels_touches_in_view`] 决定转发 touchesEnded: 还是
/// touchesCancelled:,再调用 [`fire_recognized`](它负责派发并释放引用)。
pub(super) fn process_touches(env: &mut Environment, touches: id, stage: TouchStage) -> Vec<id> {
    let mut recognized = Vec::new();
    if touches == nil {
        return recognized;
    }
    if stage != TouchStage::Began && ACTIVE.with(|a| a.borrow().is_empty()) {
        return recognized;
    }
    let touch_class = env.objc.get_known_class("UITouch", &mut env.mem);
    let touch_arr: id = msg![env; touches allObjects];
    let count: NSUInteger = msg![env; touch_arr count];
    for i in 0..count {
        let touch: id = msg![env; touch_arr objectAtIndex:i];
        if touch == nil {
            continue;
        }
        let class = ObjC::read_isa(touch, &env.mem);
        if !env.objc.class_is_subclass_of(class, touch_class) {
            continue;
        }
        match stage {
            TouchStage::Began => touch_began(env, touch),
            TouchStage::Moved => touch_moved(env, touch),
            TouchStage::Ended => touch_ended(env, touch, &mut recognized),
            TouchStage::Cancelled => touch_cancelled(env, touch),
        }
    }
    recognized
}

/// 识别成功的识别器里是否有 cancelsTouchesInView=YES 的(有则命中视图应收到 touchesCancelled:)。
pub(super) fn cancels_touches_in_view(env: &Environment, recognized: &[id]) -> bool {
    recognized.iter().any(|&recognizer| {
        env.objc
            .borrow::<UIGestureRecognizerHostObject>(recognizer)
            .cancels_touches_in_view
    })
}

/// 派发识别成功的识别器的 target-action(action 形参是识别器本身),然后释放
/// [`process_touches`] 交出的引用。调用方应已完成 touchesEnded:/touchesCancelled: 的转发。
pub(super) fn fire_recognized(env: &mut Environment, recognized: Vec<id>) {
    for recognizer in recognized {
        let can_fire = {
            let host = env
                .objc
                .borrow::<UIGestureRecognizerHostObject>(recognizer);
            host.view != nil && host.enabled && host.state == UIGestureRecognizerStateEnded
        } && delegate_should_begin(env, recognizer);
        if can_fire {
            let (targets, view) = {
                let host = env
                    .objc
                    .borrow::<UIGestureRecognizerHostObject>(recognizer);
                (host.targets.clone(), host.view)
            };
            log!(
                "[扫描修 2026-09-15] UITapGestureRecognizer {:?} 识别成功(视图 {:?}),派发 {} 个 action",
                recognizer,
                view,
                targets.len()
            );
            for (target, action) in targets {
                // 前一个 action 可能已经把视图树拆掉(视图释放时会清空识别器的 view)。
                if env
                    .objc
                    .borrow::<UIGestureRecognizerHostObject>(recognizer)
                    .view
                    == nil
                {
                    break;
                }
                let colon_count = action
                    .as_str(&env.mem)
                    .bytes()
                    .filter(|&b| b == b':')
                    .count();
                log_dbg!(
                    "[扫描修 2026-09-15] 手势 action {:?} -> {:?}",
                    action,
                    target
                );
                if colon_count == 0 {
                    () = msg_send(env, (target, action));
                } else {
                    () = msg_send(env, (target, action, recognizer));
                }
            }
        }
        env.objc
            .borrow_mut::<UIGestureRecognizerHostObject>(recognizer)
            .state = UIGestureRecognizerStatePossible;
        release(env, recognizer);
    }
}

/// UIScrollView 开始拖动时调用:让正在跟踪同一触摸的识别器失败
/// (UIKit 里 scroll view 的拖动手势识别成功会阻止点击识别)。
pub(super) fn fail_recognizers_tracking(env: &mut Environment, touch: id) {
    for recognizer in active_tracking(env, touch) {
        env.objc
            .borrow_mut::<UIGestureRecognizerHostObject>(recognizer)
            .state = UIGestureRecognizerStateFailed;
    }
}

/// UIView addGestureRecognizer: 调用:记录所挂视图(弱引用)。
pub(super) fn attach_to_view(env: &mut Environment, recognizer: id, view: id) {
    env.objc
        .borrow_mut::<UIGestureRecognizerHostObject>(recognizer)
        .view = view;
}

/// UIView removeGestureRecognizer: 或视图 dealloc 调用:清空所挂视图并中止正在进行的跟踪。
/// 调用方随后自行 release 视图持有的那份引用。
pub(super) fn detach_from_view(env: &mut Environment, recognizer: id) {
    env.objc
        .borrow_mut::<UIGestureRecognizerHostObject>(recognizer)
        .view = nil;
    reset_tracking(env, recognizer);
}

fn location_in_view(env: &mut Environment, this: id, view: id) -> CGPoint {
    let (location, window) = {
        let host = env.objc.borrow::<UIGestureRecognizerHostObject>(this);
        (host.last_location, host.last_window)
    };
    if view == nil || window == nil || view == window {
        return location;
    }
    // 目标视图必须还在记录触摸时的那个窗口里,否则 CALayer 坐标转换找不到公共祖先会 panic。
    let view_window: id = msg![env; view window];
    if view_window != window {
        log_dbg!(
            "[扫描修 2026-09-15] locationInView:{:?} 不在窗口 {:?} 里,返回窗口坐标",
            view,
            window
        );
        return location;
    }
    msg![env; view convertPoint:location fromView:window]
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIGestureRecognizer: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIGestureRecognizerHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTarget:(id)target action:(SEL)action {
    if target != nil && !action.is_null() {
        env.objc
            .borrow_mut::<UIGestureRecognizerHostObject>(this)
            .targets
            .push((target, action));
    }
    this
}

- (())dealloc {
    // 在活动表或派发列表里时对象被 retain 着,不会走到这里;tracked_touch 正常为 nil,保险起见仍释放。
    let tracked_touch = std::mem::take(
        &mut env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).tracked_touch,
    );
    release(env, tracked_touch);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (())addTarget:(id)target action:(SEL)action {
    if target == nil || action.is_null() {
        log!(
            "[扫描修 2026-09-15] 忽略 [(UIGestureRecognizer*){:?} addTarget:{:?} action:{:?}]",
            this,
            target,
            action
        );
        return;
    }
    let host = env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this);
    if !host.targets.contains(&(target, action)) {
        host.targets.push((target, action));
    }
}

- (())removeTarget:(id)target action:(SEL)action {
    // UIKit 语义:target 为 nil 表示所有 target,action 为 NULL 表示该 target 的所有 action。
    env.objc
        .borrow_mut::<UIGestureRecognizerHostObject>(this)
        .targets
        .retain(|&(t, a)| !((target == nil || t == target) && (action.is_null() || a == action)));
}

- (id)view {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).view
}

- (id)delegate {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).delegate
}
- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).delegate = delegate;
}

- (UIGestureRecognizerState)state {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).state
}
// UIGestureRecognizerSubclass.h 里给子类用的 setter。
- (())setState:(UIGestureRecognizerState)state {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).state = state;
}

- (bool)isEnabled {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).enabled
}
- (())setEnabled:(bool)enabled {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).enabled = enabled;
    if !enabled {
        // UIKit:禁用会取消正在进行的识别。reset_tracking 可能释放活动表那份引用,之后不再访问 this。
        reset_tracking(env, this);
    }
}

- (bool)cancelsTouchesInView {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).cancels_touches_in_view
}
- (())setCancelsTouchesInView:(bool)cancels {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).cancels_touches_in_view = cancels;
}

- (bool)delaysTouchesBegan {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).delays_touches_began
}
- (())setDelaysTouchesBegan:(bool)delays {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).delays_touches_began = delays;
}

- (bool)delaysTouchesEnded {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).delays_touches_ended
}
- (())setDelaysTouchesEnded:(bool)delays {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).delays_touches_ended = delays;
}

- (CGPoint)locationInView:(id)view { // UIView*
    location_in_view(env, this, view)
}

- (CGPoint)locationOfTouch:(NSUInteger)_index inView:(id)view { // UIView*
    // 只支持单指,任何下标都返回同一个位置。
    location_in_view(env, this, view)
}

- (NSUInteger)numberOfTouches {
    let host = env.objc.borrow::<UIGestureRecognizerHostObject>(this);
    if host.tracked_touch != nil || host.state == UIGestureRecognizerStateEnded {
        1
    } else {
        0
    }
}

- (())requireGestureRecognizerToFail:(id)other { // UIGestureRecognizer*
    log_dbg!(
        "[扫描修 2026-09-15] TODO: [(UIGestureRecognizer*){:?} requireGestureRecognizerToFail:{:?}](忽略)",
        this,
        other
    );
}

// 以下是给子类覆盖的空实现(子类调用 super 时不至于落进"不响应选择子"的日志)。
- (())reset {
}
- (())touchesBegan:(id)_touches withEvent:(id)_event {
}
- (())touchesMoved:(id)_touches withEvent:(id)_event {
}
- (())touchesEnded:(id)_touches withEvent:(id)_event {
}
- (())touchesCancelled:(id)_touches withEvent:(id)_event {
}

@end

@implementation UITapGestureRecognizer: UIGestureRecognizer

- (NSUInteger)numberOfTapsRequired {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).number_of_taps_required
}
- (())setNumberOfTapsRequired:(NSUInteger)count {
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).number_of_taps_required = count.max(1);
}

- (NSUInteger)numberOfTouchesRequired {
    env.objc.borrow::<UIGestureRecognizerHostObject>(this).number_of_touches_required
}
- (())setNumberOfTouchesRequired:(NSUInteger)count {
    if count > 1 {
        log!(
            "[扫描修 2026-09-15] TODO: UITapGestureRecognizer {:?} 请求 {} 指点击,只支持单指",
            this,
            count
        );
    }
    env.objc.borrow_mut::<UIGestureRecognizerHostObject>(this).number_of_touches_required = count.max(1);
}

@end

};
