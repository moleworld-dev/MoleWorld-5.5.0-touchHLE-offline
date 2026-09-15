/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIView`.
//!
//! Useful resources:
//! - Apple's [View Programming Guide for iOS](https://developer.apple.com/library/archive/documentation/WindowsViews/Conceptual/ViewPG_iPhoneOS/Introduction/Introduction.html)

pub mod ui_alert_view;
pub mod ui_control;
pub mod ui_image_view;
pub mod ui_label;
pub mod ui_picker_view;
pub mod ui_scroll_view;
pub mod ui_table_view;
pub mod ui_web_view;
pub mod ui_window;

use super::ui_gesture_recognizer::{self, TouchStage};
use super::ui_graphics::{UIGraphicsPopContext, UIGraphicsPushContext};
use crate::frameworks::core_foundation::time::CFTimeInterval;
use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::cg_color::CGColorRef;
use crate::frameworks::core_graphics::cg_context::{CGContextClearRect, CGContextRef};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::{ns_array, NSInteger, NSUInteger};
use crate::mem::MutVoidPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain,
    todo_objc_setter, Class, ClassExports, HostObject, NSZonePtr, ObjC, SEL,
};
use crate::Environment;
use std::cell::RefCell;

#[derive(Default)]
pub struct State {
    /// List of views for internal purposes. Non-retaining!
    pub(super) views: Vec<id>,
    pub ui_window: ui_window::State,
    /// [深扫修 2026-09-11] #23(a):当前 `needs_layout == true` 的视图个数。
    /// 合成前的布局遍历先看它,为 0 时零开销直接返回(游戏大部分时间走
    /// CAEAGLLayer 快路径,没有任何 UIKit 视图打脏标记)。
    dirty_layout_count: usize,
    /// [深扫修 2026-09-11] #23(a):合成前布局遍历的重入保护。
    in_layout_pass: bool,
}

pub(super) struct UIViewHostObject {
    /// CALayer or subclass.
    layer: id,
    /// Subviews in back-to-front order. These are strong references.
    subviews: Vec<id>,
    /// The superview. This is a weak reference.
    superview: id,
    /// The view controller that controls this view. This is a weak reference
    view_controller: id,
    tag: NSInteger,
    clears_context_before_drawing: bool,
    user_interaction_enabled: bool,
    multiple_touch_enabled: bool,
    /// [深扫修 2026-09-11] #23(a):`setNeedsLayout` 打的脏标记,由合成前的
    /// 布局遍历或 `layoutIfNeeded` 消费。
    needs_layout: bool,
    /// [扫描修 2026-09-15] F8-2:挂在本视图上的手势识别器(强引用;识别器的 view 是弱引用)。
    gesture_recognizers: Vec<id>,
}
impl HostObject for UIViewHostObject {}
impl Default for UIViewHostObject {
    fn default() -> UIViewHostObject {
        // The Default trait is implemented so subclasses will get the same
        // defaults.
        UIViewHostObject {
            layer: nil,
            subviews: Vec::new(),
            superview: nil,
            view_controller: nil,
            tag: 0,
            clears_context_before_drawing: true,
            user_interaction_enabled: true,
            multiple_touch_enabled: false,
            needs_layout: false,
            gesture_recognizers: Vec::new(),
        }
    }
}

pub fn set_view_controller(env: &mut Environment, view: id, controller: id) {
    let host_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
    host_obj.view_controller = controller;
}

/// Shared parts of `initWithCoder:` and `initWithFrame:`. These can't call
/// `init`: the subclass may have overridden `init` and will not expect to be
/// called here.
///
/// Do not call this in subclasses of `UIView`.
fn init_common(env: &mut Environment, this: id) -> id {
    let view_class: Class = msg![env; this class];
    let layer_class: Class = msg![env; view_class layerClass];
    let layer: id = msg![env; layer_class layer];

    // CALayer is not opaque by default, but UIView is
    () = msg![env; layer setDelegate:this];
    () = msg![env; layer setOpaque:true];

    env.objc.borrow_mut::<UIViewHostObject>(this).layer = layer;

    env.framework_state.uikit.ui_view.views.push(this);

    this
}

/// One active `+beginAnimations:context:` … `+commitAnimations` block.
///
/// While the stack is non-empty, the animatable UIView setters (`setAlpha:`,
/// `setCenter:`, `setBounds:`, `setFrame:`) record the layer's *old* value
/// (boxed) so `commitAnimations` can build a `CABasicAnimation` from the old
/// value to the now-current model value. Only the keyPaths the interpolation
/// engine supports (opacity/position/bounds — see `core_animation::animation`)
/// are captured; everything else just applies its final value with no tween
/// (so e.g. a `transform` change never reaches the keyPath panic).
struct AnimContext {
    duration: CFTimeInterval,
    /// UIViewAnimationCurve: 0=EaseInOut, 1=EaseIn, 2=EaseOut, 3=Linear.
    curve: NSInteger,
    /// (layer, keyPath, boxed-old-value). The box is retained until commit.
    captures: Vec<(id, &'static str, id)>,
}

thread_local! {
    /// Stack of active UIView animation blocks (begin/commitAnimations nest).
    /// Empty almost always — the capture path costs one bool check otherwise.
    static ANIM_STACK: RefCell<Vec<AnimContext>> = const { RefCell::new(Vec::new()) };
}

/// True while inside a `beginAnimations`/`commitAnimations` block.
fn anim_active() -> bool {
    ANIM_STACK.with(|s| !s.borrow().is_empty())
}

/// Box the layer's current value for `key_path` (the supported animatable
/// keyPaths only). Returns `nil` for unsupported keyPaths.
fn box_layer_value(env: &mut Environment, layer: id, key_path: &str) -> id {
    match key_path {
        "opacity" => {
            let v: f32 = msg![env; layer opacity];
            msg_class![env; NSNumber numberWithFloat:v]
        }
        "position" => {
            let v: CGPoint = msg![env; layer position];
            msg_class![env; NSValue valueWithCGPoint:v]
        }
        "bounds" => {
            let v: CGRect = msg![env; layer bounds];
            msg_class![env; NSValue valueWithCGRect:v]
        }
        _ => nil,
    }
}

/// Record the *old* value of `key_path` on `layer` if an animation block is
/// active and it hasn't been captured yet (keep the earliest "from" value).
/// Call this in the setter BEFORE writing the new value.
fn capture_old(env: &mut Environment, layer: id, key_path: &'static str) {
    if !anim_active() {
        return;
    }
    let already = ANIM_STACK.with(|s| {
        s.borrow()
            .last()
            .map_or(true, |c| c.captures.iter().any(|&(l, k, _)| l == layer && k == key_path))
    });
    if already {
        return;
    }
    let boxed = box_layer_value(env, layer, key_path);
    if boxed == nil {
        return;
    }
    retain(env, boxed);
    ANIM_STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            c.captures.push((layer, key_path, boxed));
        }
    });
}

/// Pop the top animation block and turn each captured property change into a
/// `CABasicAnimation` (old value -> current model value) on its layer.
fn commit_animations(env: &mut Environment) {
    let Some(ctx) = ANIM_STACK.with(|s| s.borrow_mut().pop()) else {
        return; // unbalanced commitAnimations — ignore
    };
    let timing_name: &str = match ctx.curve {
        1 => "easeIn",
        2 => "easeOut",
        3 => "linear",
        _ => "easeInEaseOut",
    };
    for (layer, key_path, from_box) in ctx.captures {
        let to_box = box_layer_value(env, layer, key_path);
        if to_box != nil {
            let kp: id = get_static_str(env, key_path);
            let anim: id = msg_class![env; CABasicAnimation animationWithKeyPath:kp];
            () = msg![env; anim setFromValue:from_box];
            () = msg![env; anim setToValue:to_box];
            () = msg![env; anim setDuration:(ctx.duration)];
            let tname: id = get_static_str(env, timing_name);
            let timing: id = msg_class![env; CAMediaTimingFunction functionWithName:tname];
            () = msg![env; anim setTimingFunction:timing];
            () = msg![env; layer addAnimation:anim forKey:kp];
        }
        release(env, from_box);
    }
}

/// [深扫修 2026-09-11] #23(a):给视图打"需要布局"脏标记(幂等),并维护脏计数。
fn mark_needs_layout(env: &mut Environment, view: id) {
    let host_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
    if !host_obj.needs_layout {
        host_obj.needs_layout = true;
        env.framework_state.uikit.ui_view.dirty_layout_count += 1;
    }
}

/// [深扫修 2026-09-11] #23(a):取走脏标记。返回该视图此前是否需要布局。
/// 在调用 layoutSubviews **之前**清标记:若 layoutSubviews 内部又对自己
/// setNeedsLayout,标记会保留到下一帧,而不会在同一轮里无限循环。
fn take_needs_layout(env: &mut Environment, view: id) -> bool {
    let host_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
    if host_obj.needs_layout {
        host_obj.needs_layout = false;
        let state = &mut env.framework_state.uikit.ui_view;
        state.dirty_layout_count = state.dirty_layout_count.saturating_sub(1);
        true
    } else {
        false
    }
}

/// [深扫修 2026-09-11] #23(a):自顶向下布局 `view` 子树里所有脏视图
/// (父视图先布局,因为父的 layoutSubviews 常会改子视图 frame / 添加子视图)。
/// 返回本次是否真的调用过 layoutSubviews。
fn layout_subtree_if_needed(env: &mut Environment, view: id, depth: u32) -> bool {
    // 防御异常深/成环的层级,正常 UIKit 层级远达不到。
    const MAX_DEPTH: u32 = 64;
    if depth > MAX_DEPTH {
        return false;
    }
    let mut did_layout = false;
    if take_needs_layout(env, view) {
        () = msg![env; view layoutSubviews];
        did_layout = true;
    }
    // layoutSubviews 可能增删子视图,所以此时才取子视图列表;遍历期间 retain
    // 住,避免某个子视图的 layoutSubviews 把兄弟视图移除并释放后访问悬垂对象。
    let subviews = env.objc.borrow::<UIViewHostObject>(view).subviews.clone();
    for &subview in &subviews {
        retain(env, subview);
    }
    for &subview in &subviews {
        did_layout |= layout_subtree_if_needed(env, subview, depth + 1);
    }
    for subview in subviews {
        release(env, subview);
    }
    did_layout
}

/// [深扫修 2026-09-11] #23(a):UIKit 合成前布局所有可见窗口层级里的脏视图。
///
/// 仅供 `core_animation::composition::recomposite_if_necessary` 在
/// display_layers(即 drawRect:)之前调用 —— 它由 NSRunLoop 每轮调用,
/// 不在游戏 drawScene/mainLoop 帧栈里,因此在这里给 guest 发 layoutSubviews
/// 是安全的(与 display_layers 发 drawRect: 同一上下文)。
///
/// 无脏视图时零开销返回;有重入保护;多轮遍历有上限,防止
/// "布局 A 弄脏 B、布局 B 又弄脏 A"式的无限循环(剩下的留到下一帧)。
pub fn layout_dirty_views_before_composition(env: &mut Environment) {
    {
        let state = &env.framework_state.uikit.ui_view;
        if state.dirty_layout_count == 0 || state.in_layout_pass {
            return;
        }
    }
    env.framework_state.uikit.ui_view.in_layout_pass = true;
    const MAX_PASSES: usize = 4;
    for _ in 0..MAX_PASSES {
        let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
        let mut did_layout = false;
        for window in windows {
            // 窗口列表不持有引用;上一个窗口的布局可能改变了窗口列表,
            // 只处理仍在列表中的窗口。
            if !env
                .framework_state
                .uikit
                .ui_view
                .ui_window
                .windows
                .contains(&window)
            {
                continue;
            }
            did_layout |= layout_subtree_if_needed(env, window, 0);
        }
        // [审查修 2026-09-13] E12:这一轮一个视图都没布局、计数却仍 > 0,说明剩下的
        // 计数要么来自不在任何窗口里的脏视图,要么是子类 dealloc 泄漏的。
        // 用全局视图表重新统计来校准(见 recount_dirty_views)。
        if !did_layout && env.framework_state.uikit.ui_view.dirty_layout_count > 0 {
            recount_dirty_views(env);
        }
        if !did_layout || env.framework_state.uikit.ui_view.dirty_layout_count == 0 {
            break;
        }
    }
    env.framework_state.uikit.ui_view.in_layout_pass = false;
}

/// [审查修 2026-09-13] E12:用全局视图表重新统计 `needs_layout == true` 的视图个数,
/// 校准 `dirty_layout_count`。
///
/// 根因:计数只在 take_needs_layout 和 UIView dealloc 里扣减。UIControl、UIButton、
/// UISwitch、UITextField、UITextView 的 dealloc 会先对整个子类宿主对象做
/// std::mem::take,把内嵌的 UIViewHostObject(连同 needs_layout)清成 Default,再
/// msg_super 到 UIView dealloc。那里读到的 needs_layout 恒为 false,不会扣减,于是计数
/// 永久多 1,"计数为 0 零开销返回"的短路从此失效,每轮 run loop 都要走一遍窗口树。
///
/// 取舍:不改这 5 个子类的 dealloc。把 superclass 放回去会让 UIView dealloc 开始真正
/// release layer/subviews 并执行 superview == nil 断言,可能暴露别的原有问题。这里只在
/// "一轮遍历没布局任何视图而计数仍 > 0"时做一次宿主侧校准,泄漏的计数当轮就能自愈;
/// 确实还脏着、但不在任何窗口里的视图仍会计入,语义不变。
///
/// 安全性:views 表不持有引用,但每个条目都会在 UIView dealloc 里移除(uikit 里所有
/// 视图子类的 dealloc 都 msg_super 到 UIView,没有绕过它直接 dealloc_object 的),所以
/// 表里都是存活对象。ObjC::borrow 会沿 as_superclass 链往下找,子类宿主对象内嵌
/// superclass 时也能取到 UIViewHostObject(与本文件遍历子视图的现有写法一致)。
/// 全程只做宿主侧读取,不发任何消息,不会跑 guest 代码。
fn recount_dirty_views(env: &mut Environment) {
    let dirty_count = {
        let objc: &ObjC = &env.objc;
        env.framework_state
            .uikit
            .ui_view
            .views
            .iter()
            .filter(|&&view| objc.borrow::<UIViewHostObject>(view).needs_layout)
            .count()
    };
    let state = &mut env.framework_state.uikit.ui_view;
    if state.dirty_layout_count != dirty_count {
        log_dbg!(
            "dirty_layout_count recalibrated: {} -> {}",
            state.dirty_layout_count,
            dirty_count
        );
    }
    state.dirty_layout_count = dirty_count;
}

/// [扫描修 2026-09-15] F8-2:从 `view` 起沿父视图链收集挂着的手势识别器(不改引用计数)。
/// 供 ui_gesture_recognizer 在按下阶段找候选识别器。只做宿主侧读取,不发消息。
pub(super) fn gesture_recognizers_in_chain(env: &Environment, view: id) -> Vec<id> {
    // 防御异常深/成环的层级,正常 UIKit 层级远达不到。
    const MAX_DEPTH: u32 = 64;
    let mut result = Vec::new();
    let mut current = view;
    let mut depth: u32 = 0;
    while current != nil && depth < MAX_DEPTH {
        let host = env.objc.borrow::<UIViewHostObject>(current);
        result.extend_from_slice(&host.gesture_recognizers);
        current = host.superview;
        depth += 1;
    }
    result
}

/// [扫描修 2026-09-15] F8-2:按 UIResponder 默认语义把触摸消息转给下一响应者。
///
/// Began/Moved/Ended 与 UIResponder 的默认实现完全一致(直接发给 nextResponder)。
/// touchHLE 的 UIResponder / UIViewController / UIApplication 没有 touchesCancelled:withEvent:,
/// 取消消息沿响应链找第一个响应它的对象再发(UIKit 里 UIResponder 默认实现就是一路往上转发,
/// 结果相同),找不到就丢弃。
pub(super) fn forward_touches(
    env: &mut Environment,
    this: id,
    stage: TouchStage,
    touches: id,
    event: id,
) {
    let next: id = msg![env; this nextResponder];
    if next == nil {
        return;
    }
    match stage {
        TouchStage::Began => {
            () = msg![env; next touchesBegan:touches withEvent:event];
        }
        TouchStage::Moved => {
            () = msg![env; next touchesMoved:touches withEvent:event];
        }
        TouchStage::Ended => {
            () = msg![env; next touchesEnded:touches withEvent:event];
        }
        TouchStage::Cancelled => {
            const MAX_DEPTH: u32 = 64;
            let sel: SEL = env
                .objc
                .register_host_selector("touchesCancelled:withEvent:".to_string(), &mut env.mem);
            let mut responder = next;
            let mut depth: u32 = 0;
            while responder != nil && depth < MAX_DEPTH {
                let responds: bool = msg![env; responder respondsToSelector:sel];
                if responds {
                    () = msg_send(env, (responder, sel, touches, event));
                    return;
                }
                responder = msg![env; responder nextResponder];
                depth += 1;
            }
        }
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIView: UIResponder

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (Class)layerClass {
    env.objc.get_known_class("CALayer", &mut env.mem)
}

// Legacy (begin/commit) UIView animation block. Property changes made to a view
// between beginAnimations: and commitAnimations are turned into CABasicAnimations
// at commit time (see the helpers above). Previously these all no-op'd, so
// transitions hard-cut; now they tween. iOS default duration is 0.2s.
+ (())beginAnimations:(id)_animation_id context:(MutVoidPtr)_context {
    ANIM_STACK.with(|s| {
        s.borrow_mut().push(AnimContext {
            duration: 0.2,
            curve: 0,
            captures: Vec::new(),
        })
    });
}
+ (())commitAnimations {
    commit_animations(env);
}
+ (())setAnimationDuration:(CFTimeInterval)duration {
    ANIM_STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            c.duration = duration;
        }
    });
}
+ (())setAnimationCurve:(NSInteger)curve {
    ANIM_STACK.with(|s| {
        if let Some(c) = s.borrow_mut().last_mut() {
            c.curve = curve;
        }
    });
}
// Accepted and recorded only enough to not break flow. The game (cocos2d) does
// not use these in practice, but having them present stops them no-op'ing
// through the missing-selector shim.
+ (())setAnimationDelay:(CFTimeInterval)_delay {}
+ (())setAnimationDelegate:(id)_delegate {}
+ (())setAnimationWillStartSelector:(SEL)_sel {}
+ (())setAnimationDidStopSelector:(SEL)_sel {}
+ (())setAnimationRepeatCount:(f32)_count {}
+ (())setAnimationRepeatAutoreverses:(bool)_autoreverses {}
+ (())setAnimationBeginsFromCurrentState:(bool)_begins {}
+ (())setAnimationTransition:(NSInteger)_transition forView:(id)_view cache:(bool)_cache {}
+ (())setAnimationsEnabled:(bool)_enabled {}
+ (bool)areAnimationsEnabled {
    true
}

// TODO: accessors etc

// initWithCoder: and initWithFrame: are basically UIView's designated
// initializers. init is not, it's a shortcut for the latter.
// Subclasses need to override both.

- (id)init {
    msg![env; this initWithFrame:(<CGRect as Default>::default())]
}

- (id)initWithFrame:(CGRect)frame {
    let this = init_common(env, this);

    () = msg![env; this setFrame:frame];

    log_dbg!(
        "[(UIView*){:?} initWithFrame:{:?}] => bounds {:?}, center {:?}",
        this,
        frame,
        { let bounds: CGRect = msg![env; this bounds]; bounds },
        { let center: CGPoint = msg![env; this center]; center },
    );

    this
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    let this = init_common(env, this);

    // TODO: decode the various other UIView properties

    let key_ns_string = get_static_str(env, "UIBounds");
    let bounds: CGRect = msg![env; coder decodeCGRectForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UICenter");
    let center: CGPoint = msg![env; coder decodeCGPointForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UIHidden");
    let hidden: bool = msg![env; coder decodeBoolForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UIOpaque");
    let opaque: bool = msg![env; coder decodeBoolForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UIBackgroundColor");
    let bg_color: id = msg![env; coder decodeObjectForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UITag");
    let tag: NSInteger = msg![env; coder decodeIntegerForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UIMultipleTouchEnabled");
    let multi_touch_enabled: bool = msg![env; coder decodeBoolForKey:key_ns_string];

    let key_ns_string = get_static_str(env, "UISubviews");
    let subviews: id = msg![env; coder decodeObjectForKey:key_ns_string];
    let subview_count: NSUInteger = msg![env; subviews count];

    log_dbg!(
        "[(UIView*){:?} initWithCoder:{:?}] => bounds {}, center {}, hidden {}, bg color {:?}, tag {}, opaque {}, multi touch enabled {}, {} subviews",
        this,
        coder,
        bounds,
        center,
        hidden,
        bg_color,
        tag,
        opaque,
        multi_touch_enabled,
        subview_count,
    );

    () = msg![env; this setBounds:bounds];
    () = msg![env; this setCenter:center];
    () = msg![env; this setHidden:hidden];
    () = msg![env; this setOpaque:opaque];
    () = msg![env; this setBackgroundColor:bg_color];
    () = msg![env; this setTag:tag];
    () = msg![env; this setMultipleTouchEnabled:multi_touch_enabled];

    for i in 0..subview_count {
        let subview: id = msg![env; subviews objectAtIndex:i];
        () = msg![env; this addSubview:subview];
    }

    this
}

- (NSInteger)tag {
    env.objc.borrow::<UIViewHostObject>(this).tag
}
- (())setTag:(NSInteger)tag {
    env.objc.borrow_mut::<UIViewHostObject>(this).tag = tag;
}

- (id)viewWithTag:(NSInteger)tag {
    let &UIViewHostObject {
        ref subviews,
        tag: view_tag,
        ..
    } = env.objc.borrow(this);
    if view_tag == tag {
        return this;
    }
    for view in subviews {
        if env.objc.borrow::<UIViewHostObject>(*view).tag == tag {
            return *view;
        }
    }
    nil
}

- (bool)isUserInteractionEnabled {
    env.objc.borrow::<UIViewHostObject>(this).user_interaction_enabled
}
- (())setUserInteractionEnabled:(bool)enabled {
    env.objc.borrow_mut::<UIViewHostObject>(this).user_interaction_enabled = enabled;
}

- (bool)isMultipleTouchEnabled {
    env.objc.borrow::<UIViewHostObject>(this).multiple_touch_enabled
}
- (())setMultipleTouchEnabled:(bool)enabled {
    env.objc.borrow_mut::<UIViewHostObject>(this).multiple_touch_enabled = enabled;
}

- (())setExclusiveTouch:(bool)exclusive {
    log!("TODO: ignoring setExclusiveTouch:{} for view {:?}", exclusive, this);
}

- (())layoutSubviews {
    // On iOS 5.1 and earlier, the default implementation of this method does
    // nothing.
}

// [深扫修 2026-09-11] #23(a):补 -setNeedsLayout / -layoutIfNeeded。
// 根因:此前两者都不存在(找不到选择子 = 空操作),touchHLE 只在启动时对当时
// 已有的 view 调一次 layoutSubviews,之后创建的 view 永远不会再布局。
// MBProgressHUD 的底框宽高(width/height ivar)、指示器居中 frame 全在
// layoutSubviews 里算,它靠 setNeedsLayout 触发 → HUD 整个不可见。
// 修法(按对抗复核):setNeedsLayout 只打脏标记,**不**同步调 layoutSubviews
// (MBProgressHUD 在 init 半途就调它,那时 labelFont 等属性尚未设好,同步调
// 还有重入风险);真正的布局放到 UIKit 合成阶段(composition.rs 在
// display_layers 之前调 layout_dirty_views_before_composition),保证先布局
// 后 drawRect:。layoutIfNeeded 按原版语义同步布局本视图子树里的脏视图。
- (())setNeedsLayout {
    mark_needs_layout(env, this);
}

- (())layoutIfNeeded {
    let _: bool = layout_subtree_if_needed(env, this, 0);
}

- (id)superview {
    env.objc.borrow::<UIViewHostObject>(this).superview
}

- (id)window {
    // Looks up window in the superview hierarchy
    // TODO: cache the result somehow?
    let mut window: id = env.objc.borrow::<UIViewHostObject>(this).superview;
    let window_class = env.objc.get_known_class("UIWindow", &mut env.mem);
    while window != nil {
        let current_class: Class = msg![env; window class];
        log_dbg!("maybe window {:?} curr class {}", window, env.objc.get_class_name(current_class));
        if env.objc.class_is_subclass_of(current_class, window_class) {
            break;
        }
        window = env.objc.borrow::<UIViewHostObject>(window).superview;
    }
    log_dbg!("view {:?} has window {:?}", this, window);
    window
}

- (id)subviews {
    let views = env.objc.borrow::<UIViewHostObject>(this).subviews.clone();
    for view in &views {
        retain(env, *view);
    }
    let subs = ns_array::from_vec(env, views);
    autorelease(env, subs)
}

- (())addSubview:(id)view {
    log_dbg!("[(UIView*){:?} addSubview:{:?}] => ()", this, view);

    if view == nil {
        log_dbg!("Tolerating [(UIView*){:?} addSubview:nil]", this);
        return;
    }

    if env.objc.borrow::<UIViewHostObject>(view).superview == this {
        () = msg![env; this bringSubviewToFront:view];
    } else {
        retain(env, view);
        () = msg![env; view removeFromSuperview];
        let subview_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
        subview_obj.superview = this;
        let subview_layer = subview_obj.layer;
        let this_obj = env.objc.borrow_mut::<UIViewHostObject>(this);
        this_obj.subviews.push(view);
        let this_layer = this_obj.layer;
        () = msg![env; this_layer addSublayer:subview_layer];
    }
}

- (())insertSubview:(id)view atIndex:(NSInteger)index {
    assert!(view != nil);
    retain(env, view);
    () = msg![env; view removeFromSuperview];

    let subview_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
    subview_obj.superview = this;
    let subview_layer = subview_obj.layer;

    let &mut UIViewHostObject {
        ref mut subviews,
        layer: this_layer,
        ..
    } = env.objc.borrow_mut(this);

    subviews.insert(index as usize, view);

    assert!(index >= 0);
    () = msg![env; this_layer insertSublayer:subview_layer atIndex:(index as u32)];
}

- (())insertSubview:(id)view belowSubview:(id)sibling {
    retain(env, view);
    () = msg![env; view removeFromSuperview];

    let subview_obj = env.objc.borrow_mut::<UIViewHostObject>(view);
    subview_obj.superview = this;
    let subview_layer = subview_obj.layer;

    let sibling_layer = env.objc.borrow_mut::<UIViewHostObject>(sibling).layer;

    let &mut UIViewHostObject {
        ref mut subviews,
        layer: this_layer,
        ..
    } = env.objc.borrow_mut(this);

    let idx = subviews.iter().position(|&subview2| subview2 == sibling).unwrap();
    subviews.insert(idx, view);

    () = msg![env; this_layer insertSublayer:subview_layer below:sibling_layer];
}

- (())bringSubviewToFront:(id)subview {
    if subview == nil {
        // This happens in Touch & Go LITE. It's probably due to the ad classes
        // being replaced with fakes.
        log_dbg!("Tolerating [{:?} bringSubviewToFront:nil]", this);
        return;
    }

    let &mut UIViewHostObject {
        ref mut subviews,
        layer,
        ..
    } = env.objc.borrow_mut(this);

    let Some(idx) = subviews.iter().position(|&subview2| subview2 == subview) else {
        log_dbg!("Warning: Unable to find the subview {:?} in subviews of {:?}", subview, this);
        return;
    };
    let subview2 = subviews.remove(idx);
    assert!(subview2 == subview);
    subviews.push(subview);

    let subview_layer = env.objc.borrow::<UIViewHostObject>(subview).layer;
    () = msg![env; subview_layer removeFromSuperlayer];
    () = msg![env; layer addSublayer:subview_layer];
}

- (())sendSubviewToBack:(id)subview {
    if subview == nil {
        log_dbg!("Tolerating [{:?} sendSubviewToBack:nil]", this);
        return;
    }

    let &mut UIViewHostObject {
        ref mut subviews,
        layer,
        ..
    } = env.objc.borrow_mut(this);

    let Some(idx) = subviews.iter().position(|&subview2| subview2 == subview) else {
        log_dbg!("Warning: Unable to find the subview {:?} in subviews of {:?}", subview, this);
        return;
    };
    let subview2 = subviews.remove(idx);
    assert!(subview2 == subview);
    subviews.insert(0, subview);

    let subview_layer = env.objc.borrow::<UIViewHostObject>(subview).layer;
    () = msg![env; subview_layer removeFromSuperlayer];
    () = msg![env; layer insertSublayer:subview_layer atIndex:0u32];
}

- (())removeFromSuperview {
    let &mut UIViewHostObject {
        ref mut superview,
        layer: this_layer,
        ..
    } = env.objc.borrow_mut(this);
    let superview = std::mem::take(superview);
    if superview == nil {
        return;
    }
    () = msg![env; this_layer removeFromSuperlayer];

    let UIViewHostObject { ref mut subviews, .. } = env.objc.borrow_mut(superview);
    let idx = subviews.iter().position(|&subview| subview == this).unwrap();
    let subview = subviews.remove(idx);
    assert!(subview == this);
    release(env, this);
}

- (())dealloc {
    let UIViewHostObject {
        layer,
        superview,
        subviews,
        view_controller,
        tag: _,
        clears_context_before_drawing: _,
        user_interaction_enabled: _,
        multiple_touch_enabled: _,
        needs_layout,
        gesture_recognizers,
    } = std::mem::take(env.objc.borrow_mut(this));

    // [深扫修 2026-09-11] #23(a):脏视图被释放时同步扣减计数,避免计数漂移
    // 导致每帧空跑布局遍历。
    if needs_layout {
        let state = &mut env.framework_state.uikit.ui_view;
        state.dirty_layout_count = state.dirty_layout_count.saturating_sub(1);
    }

    release(env, layer);
    assert!(view_controller == nil);
    assert!(superview == nil);
    for subview in subviews {
        env.objc.borrow_mut::<UIViewHostObject>(subview).superview = nil;
        release(env, subview);
    }

    // [扫描修 2026-09-15] F8-2:视图持有识别器的强引用;释放前清空识别器的弱引用 view 并中止跟踪。
    for recognizer in gesture_recognizers {
        ui_gesture_recognizer::detach_from_view(env, recognizer);
        release(env, recognizer);
    }

    let state = &mut env.framework_state.uikit.ui_view.views;
    state.swap_remove(
        state.iter().position(|&v| v == this).unwrap()
    );

    env.objc.dealloc_object(this, &mut env.mem);
}

- (id)layer {
    env.objc.borrow_mut::<UIViewHostObject>(this).layer
}

- (bool)isHidden {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer isHidden]
}
- (())setHidden:(bool)hidden {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer setHidden:hidden]
}

- (())setClipsToBounds:(bool)clips {
    todo_objc_setter!(this, clips);
}

- (bool)isOpaque {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer isOpaque]
}
- (())setOpaque:(bool)opaque {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer setOpaque:opaque]
}

- (CGFloat)alpha {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer opacity]
}
- (())setAlpha:(CGFloat)alpha {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    capture_old(env, layer, "opacity");
    msg![env; layer setOpacity:alpha]
}

- (id)backgroundColor {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    let cg_color: CGColorRef = msg![env; layer backgroundColor];
    msg_class![env; UIColor colorWithCGColor:cg_color]
}
- (())setBackgroundColor:(id)color { // UIColor*
    let color: CGColorRef = msg![env; color CGColor];
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer setBackgroundColor:color]
}

// TODO: support setNeedsDisplayInRect:
- (())setNeedsDisplay {
    // UIView has a method called drawRect: that subclasses override if they
    // need custom drawing. touchHLE's UIView (a CALayerDelegate) provides
    // an implementation of drawLayer:inContext: that calls drawRect:.
    // This maintains a clean separation of UIView and CALayer.
    //
    // To avoid wasting space and time on unnecessary bitmaps and drawing,
    // let's optimize here by only marking the layer as needing display if
    // the UIView's subclass overrides drawRect: or drawLayer:inContext:.
    let this_class = ObjC::read_isa(this, &env.mem);

    let ui_view_class = env.objc.get_known_class("UIView", &mut env.mem);

    let draw_layer_sel = env.objc.lookup_selector("drawLayer:inContext:").unwrap();
    let draw_rect_sel = env.objc.lookup_selector("drawRect:").unwrap();

    if env
        .objc
        .class_overrides_method_of_superclass(this_class, draw_rect_sel, ui_view_class)
        || env
            .objc
            .class_overrides_method_of_superclass(this_class, draw_layer_sel, ui_view_class)
    {
        let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
        msg![env; layer setNeedsDisplay]
    }
}

- (CGRect)bounds {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer bounds]
}
- (())setBounds:(CGRect)bounds {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    capture_old(env, layer, "bounds");
    msg![env; layer setBounds:bounds]
}
- (CGPoint)center {
    // FIXME: what happens if [layer anchorPoint] isn't (0.5, 0.5)?
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer position]
}
- (())setCenter:(CGPoint)center {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    capture_old(env, layer, "position");
    msg![env; layer setPosition:center]
}
- (CGRect)frame {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer frame]
}
- (())setFrame:(CGRect)frame {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    capture_old(env, layer, "position");
    capture_old(env, layer, "bounds");
    msg![env; layer setFrame:frame]
}
- (CGAffineTransform)transform {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer affineTransform]
}
- (())setTransform:(CGAffineTransform)transform {
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer setAffineTransform:transform]
}

- (())setContentMode:(NSInteger)content_mode { // should be UIViewContentMode
    todo_objc_setter!(this, content_mode);
}

- (bool)clearsContextBeforeDrawing {
    env.objc.borrow::<UIViewHostObject>(this).clears_context_before_drawing
}
- (())setClearsContextBeforeDrawing:(bool)v {
    env.objc.borrow_mut::<UIViewHostObject>(this).clears_context_before_drawing = v;
}

// Drawing stuff that views should override
- (())drawRect:(CGRect)_rect {
    // default implementation does nothing
}

// CALayerDelegate implementation
- (())drawLayer:(id)layer // CALayer*
      inContext:(CGContextRef)context {
    let mut bounds: CGRect = msg![env; layer bounds];
    bounds.origin = CGPoint { x: 0.0, y: 0.0 }; // FIXME: not tested
    if env.objc.borrow::<UIViewHostObject>(this).clears_context_before_drawing {
        CGContextClearRect(env, context, bounds);
    }
    UIGraphicsPushContext(env, context);
    () = msg![env; this drawRect:bounds];
    UIGraphicsPopContext(env);
}

// Event handling

- (bool)pointInside:(CGPoint)point
          withEvent:(id)_event { // UIEvent* (possibly nil)
    let layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    msg![env; layer containsPoint:point]
}

- (id)hitTest:(CGPoint)point
    withEvent:(id)event { // UIEvent* (possibly nil)
    if !msg![env; this pointInside:point withEvent:event] {
        return nil;
    }
    // TODO: avoid copy somehow?
    let subviews = env.objc.borrow::<UIViewHostObject>(this).subviews.clone();
    for subview in subviews.into_iter().rev() { // later views are on top
        let hidden: bool = msg![env; subview isHidden];
        let alpha: CGFloat = msg![env; subview alpha];
        let interactible: bool = msg![env; subview isUserInteractionEnabled];
        if hidden || alpha < 0.01 || !interactible {
           continue;
        }
        let point: CGPoint = msg![env; subview convertPoint:point fromView:this];
        let subview: id = msg![env; subview hitTest:point withEvent:event];
        if subview != nil {
            return subview;
        }
    }
    this
}

// [扫描修 2026-09-15] F8-2:手势识别器挂载。视图持有识别器的强引用,识别器的 view 是弱引用。
// 根因:此前没有这些方法(也没有识别器类),ATPagingView 的单击识别器挂不上,VIP 教程
// 第 3 页点击关闭永远触发不了。
- (())addGestureRecognizer:(id)recognizer { // UIGestureRecognizer*
    if recognizer == nil {
        // 缺类时 [XxxGestureRecognizer alloc] 得 nil(例如广告 SDK 引用的 UIPanGestureRecognizer)。
        log_dbg!("Tolerating [(UIView*){:?} addGestureRecognizer:nil]", this);
        return;
    }
    if !ui_gesture_recognizer::is_gesture_recognizer(env, recognizer) {
        log!(
            "[扫描修 2026-09-15] 忽略 [(UIView*){:?} addGestureRecognizer:{:?}]:不是 UIGestureRecognizer",
            this,
            recognizer
        );
        return;
    }
    if env
        .objc
        .borrow::<UIViewHostObject>(this)
        .gesture_recognizers
        .contains(&recognizer)
    {
        return;
    }
    // 一个识别器只能挂在一个视图上:先从旧视图摘下(UIKit 语义)。
    let old_view: id = msg![env; recognizer view];
    if old_view != nil && old_view != this {
        () = msg![env; old_view removeGestureRecognizer:recognizer];
    }
    retain(env, recognizer);
    env.objc
        .borrow_mut::<UIViewHostObject>(this)
        .gesture_recognizers
        .push(recognizer);
    ui_gesture_recognizer::attach_to_view(env, recognizer, this);
}

- (())removeGestureRecognizer:(id)recognizer { // UIGestureRecognizer*
    if recognizer == nil {
        return;
    }
    let host = env.objc.borrow_mut::<UIViewHostObject>(this);
    let Some(idx) = host
        .gesture_recognizers
        .iter()
        .position(|&r| r == recognizer)
    else {
        return;
    };
    host.gesture_recognizers.remove(idx);
    ui_gesture_recognizer::detach_from_view(env, recognizer);
    release(env, recognizer);
}

- (id)gestureRecognizers {
    let recognizers = env
        .objc
        .borrow::<UIViewHostObject>(this)
        .gesture_recognizers
        .clone();
    if recognizers.is_empty() {
        // UIKit:没有识别器时返回 nil。
        return nil;
    }
    for &recognizer in &recognizers {
        retain(env, recognizer);
    }
    let array = ns_array::from_vec(env, recognizers);
    autorelease(env, array)
}

// [扫描修 2026-09-15] F8-2:UIView 层的触摸处理 = 手势识别挂点 + UIResponder 默认转发。
// 此前 UIView 没有这些方法,消息直接落到 UIResponder 的转发;现在转发行为不变,只是先让
// 沿途识别器看到触摸。视图链上没有识别器时只多一次父视图遍历。覆盖了这些方法的游戏类
// (如 cocos2d 的 EAGLView)和 UIControl 系不受影响。
- (())touchesBegan:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    // 识别器委托回调与转发都会跑游戏代码,可能把本视图拆下释放,处理期间 retain 住。
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Began);
    forward_touches(env, this, TouchStage::Began, touches, event);
    release(env, this);
}

- (())touchesMoved:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Moved);
    forward_touches(env, this, TouchStage::Moved, touches, event);
    release(env, this);
}

- (())touchesEnded:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    retain(env, this);
    let recognized = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Ended);
    // 识别成功且 cancelsTouchesInView:沿响应链改发 touchesCancelled:(UIKit 语义)。
    // 先转发、后派发 action:action 可能同步拆掉视图树,先派发会让取消送不到 EAGLView,
    // cocos2d 的 CCMenu 会卡在跟踪态。
    let stage = if ui_gesture_recognizer::cancels_touches_in_view(env, &recognized) {
        TouchStage::Cancelled
    } else {
        TouchStage::Ended
    };
    forward_touches(env, this, stage, touches, event);
    ui_gesture_recognizer::fire_recognized(env, recognized);
    release(env, this);
}

- (())touchesCancelled:(id)touches // NSSet* of UITouch*
             withEvent:(id)event { // UIEvent*
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Cancelled);
    forward_touches(env, this, TouchStage::Cancelled, touches, event);
    release(env, this);
}

// Ending a view-editing session

- (bool)endEditing:(bool)force {
    assert!(force);
    let responder: id = env.framework_state.uikit.ui_responder.first_responder;
    let class = msg![env; responder class];
    let ui_text_field_class = env.objc.get_known_class("UITextField", &mut env.mem);
    if responder != nil && env.objc.class_is_subclass_of(class, ui_text_field_class) {
        // we need to check if text field is in the current view hierarchy
        let mut to_find = responder;
        while to_find != nil {
            if to_find == this {
                return msg![env; responder resignFirstResponder];
            }
            to_find = msg![env; to_find superview];
        }
    }
    false
}

// UIResponder implementation
// From the Apple UIView docs regarding [UIResponder nextResponder]:
// "UIView implements this method and returns the UIViewController object that
//  manages it (if it has one) or its superview (if it doesn’t)."
- (id)nextResponder {
    let host_object = env.objc.borrow::<UIViewHostObject>(this);
    if host_object.view_controller != nil {
        host_object.view_controller
    } else {
        host_object.superview
    }
}

// Co-ordinate space conversion

- (CGPoint)convertPoint:(CGPoint)point
               fromView:(id)other { // UIView*
    if other == nil {
        let window: id = msg![env; this window];
        assert!(window != nil);
        return msg![env; this convertPoint:point fromView:window]
    }
    let this_layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    let other_layer = env.objc.borrow::<UIViewHostObject>(other).layer;
    msg![env; this_layer convertPoint:point fromLayer:other_layer]
}
- (CGPoint)convertPoint:(CGPoint)point
                 toView:(id)other { // UIView*
    if other == nil {
        let window: id = msg![env; this window];
        assert!(window != nil);
        return msg![env; this convertPoint:point toView:window]
    }
    let this_layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    let other_layer = env.objc.borrow::<UIViewHostObject>(other).layer;
    msg![env; this_layer convertPoint:point toLayer:other_layer]
}
- (CGRect)convertRect:(CGRect)rect
             fromView:(id)other { // UIView*
    if other == nil {
        let window: id = msg![env; this window];
        assert!(window != nil);
        return msg![env; this convertRect:rect fromView:window]
    }
    let this_layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    let other_layer = env.objc.borrow::<UIViewHostObject>(other).layer;
    msg![env; this_layer convertRect:rect fromLayer:other_layer]
}
- (CGRect)convertRect:(CGRect)rect
               toView:(id)other { // UIView*
    if other == nil {
        let window: id = msg![env; this window];
        assert!(window != nil);
        return msg![env; this convertRect:rect toView:window]
    }
    let this_layer = env.objc.borrow::<UIViewHostObject>(this).layer;
    let other_layer = env.objc.borrow::<UIViewHostObject>(other).layer;
    msg![env; this_layer convertRect:rect toLayer:other_layer]
}

- (())setAutoresizingMask:(NSUInteger)mask {
    todo_objc_setter!(this, mask);
}
- (())setAutoresizesSubviews:(bool)enabled {
    todo_objc_setter!(this, enabled);
}

- (CGSize)sizeThatFits:(CGSize)size {
    // default implementation, subclasses can override
    size
}
- (())sizeToFit {
    log!("TODO: [(UIView *){:?} sizeToFit]", this);
}

- (())setContentScaleFactor:(CGFloat)factor {
    todo_objc_setter!(this, factor);
}
- (CGFloat)contentScaleFactor {
    1.0 // TODO
}

@end

};
