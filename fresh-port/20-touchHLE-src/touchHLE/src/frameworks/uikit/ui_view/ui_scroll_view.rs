/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIScrollView`.
//!
//! [扫描修 2026-09-15] F8-2:补拖动生命周期回调与翻页吸附。
//! 根因:原实现只在 touchesMoved: 里改 contentOffset 并发 scrollViewDidScroll:,从不发
//! scrollViewWillBeginDragging: / scrollViewDidEndDragging:willDecelerate: /
//! scrollViewDidEndDecelerating:,也没有 pagingEnabled。游戏的 ATPagingView 只在拖动结束回调里
//! 走 knownToBeIdle → -[FeatureIntroductionView pagingViewDidEndMoving:]@0x38f304,
//! "在最后一页再滑一次"才会 pageViewsDidScrolledToTheEnd 关闭教程;回调缺失 → VIP / 漂流瓶
//! 教程层盖住整个游戏关不掉(软锁)。
//! 取舍:
//! - 起拖阈值 10pt(近似 UIKit 拖动手势的起拖距离):阈值内的抖动不算拖动,让点击识别器有机会
//!   识别;超过阈值的第一次移动发 scrollViewWillBeginDragging:,**即使 offset 被夹住不动也发**,
//!   否则最后一页"再滑一次"关不掉。
//! - 没有速度信息,翻页按"离最近页 + 拖过页长 15% 就朝拖动方向翻一页"近似 UIKit 的轻扫翻页;
//!   页长非正时只夹紧不吸附(防除零)。
//! - 没有惯性动画:需要吸附时按 UIKit 顺序 DidEndDragging:willDecelerate:YES → 直接设到目标
//!   offset + DidScroll → DidEndDecelerating;不需要吸附时只发 DidEndDragging:willDecelerate:NO。
//!   ATPagingView 两条路都会进 knownToBeIdle(它内部按 _scrollViewIsMoving 去重)。
//! - 拖动开始时沿响应链发 touchesCancelled:(UIKit 里拖动手势识别成功会取消命中视图的触摸),
//!   让上层(教程层下面的 cocos2d EAGLView)不再把这次触摸当点击;拖动结束不再转发 touchesEnded:。
//!   没有拖动的触摸(点击)保持原来的 UIResponder 转发。
//! - [复核修 2026-09-15] R2-1:上面几处沿响应链的转发(按下、起拖取消、未拖动的抬起/取消)只对
//!   `forwards_touches` 为真的宿主对象做。UITableView 建宿主对象时把它关掉:表格自己处理点选,
//!   留言板/好友列表的表格是 VC 根视图、直接挂在 openGLView 上(-[ManagerViewController
//!   initMessageView:]@0x19e128),往上转发会让 EAGLView 在同一位置再收到一次点击(穿透到
//!   cocos2d 场景)。跟踪、翻页、识别器处理与派发不受影响;普通 UIScrollView/UITextView 照旧转发。

pub mod ui_text_view;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_gesture_recognizer::{self, TouchStage};
use crate::frameworks::uikit::ui_touch::{UITouchPhaseCancelled, UITouchPhaseEnded};
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_send, msg_super, nil, objc_classes, release,
    retain, todo_objc_setter, ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

type UIScrollViewIndicatorStyle = NSInteger;

/// [扫描修 2026-09-15] F8-2:触摸从起点移动超过这个距离(pt)才算开始拖动。
const DRAG_START_THRESHOLD: CGFloat = 10.0;
/// [扫描修 2026-09-15] F8-2:翻页时拖过页长的这个比例,就朝拖动方向翻一页。
const PAGING_FLICK_RATIO: CGFloat = 0.15;

pub struct UIScrollViewHostObject {
    superclass: super::UIViewHostObject,
    /// UIScrollViewDelegate, weak reference
    delegate: id,
    scroll_enabled: bool,
    content_offset: CGPoint,
    content_size: CGSize,
    /// [扫描修 2026-09-15] F8-2:翻页模式(拖动结束吸附到整页)。
    paging_enabled: bool,
    /// [扫描修 2026-09-15] F8-2:只存值,touchHLE 不画滚动条。
    shows_horizontal_scroll_indicator: bool,
    /// [扫描修 2026-09-15] F8-2:只存值,touchHLE 不画滚动条。
    shows_vertical_scroll_indicator: bool,
    /// [扫描修 2026-09-15] F8-2:本次手势跟踪的触摸(强引用,手势结束/取消时释放)。
    tracked_touch: id,
    /// [扫描修 2026-09-15] F8-2:开始跟踪时触摸在本视图坐标里的位置。
    track_start_location: CGPoint,
    /// [扫描修 2026-09-15] F8-2:开始跟踪时的 contentOffset(换算起点用)。
    track_start_offset: CGPoint,
    /// [扫描修 2026-09-15] F8-2:开始拖动时的 contentOffset(翻页判断方向用)。
    drag_begin_offset: CGPoint,
    /// [扫描修 2026-09-15] F8-2:是否已越过起拖阈值、处于拖动中。
    dragging: bool,
    /// [复核修 2026-09-15] R2-1:触摸是否沿响应链往上转发。默认真(UIResponder 语义);
    /// UITableView 用 [`UIScrollViewHostObject::without_touch_forwarding`] 建成假。
    forwards_touches: bool,
}
impl_HostObject_with_superclass!(UIScrollViewHostObject);
impl UIScrollViewHostObject {
    /// [复核修 2026-09-15] R2-1:不沿响应链转发触摸的宿主对象(UITableView 的 superclass 字段用)。
    pub(super) fn without_touch_forwarding() -> Self {
        UIScrollViewHostObject {
            forwards_touches: false,
            ..Default::default()
        }
    }
}
impl Default for UIScrollViewHostObject {
    fn default() -> Self {
        UIScrollViewHostObject {
            superclass: Default::default(),
            delegate: nil,
            scroll_enabled: true,
            content_offset: CGPoint { x: 0.0, y: 0.0 },
            content_size: CGSize {
                width: 0.0,
                height: 0.0,
            },
            paging_enabled: false,
            shows_horizontal_scroll_indicator: true,
            shows_vertical_scroll_indicator: true,
            tracked_touch: nil,
            track_start_location: CGPoint { x: 0.0, y: 0.0 },
            track_start_offset: CGPoint { x: 0.0, y: 0.0 },
            drag_begin_offset: CGPoint { x: 0.0, y: 0.0 },
            dragging: false,
            forwards_touches: true,
        }
    }
}

/// [复核修 2026-09-15] R2-1:按宿主对象的 `forwards_touches` 决定是否沿响应链转发(UITableView 不转发)。
fn forward_touches_if_enabled(
    env: &mut Environment,
    this: id,
    stage: TouchStage,
    touches: id,
    event: id,
) {
    let forwards = env
        .objc
        .borrow::<UIScrollViewHostObject>(this)
        .forwards_touches;
    if forwards {
        super::forward_touches(env, this, stage, touches, event);
    }
}

/// [复核修 2026-09-15] R2-2:本视图是否还挂在窗口里。不在窗口里时 UITouch locationInView:本视图
/// 会在 CALayer 找公共祖先时 panic("have no common ancestor"),换算坐标前先判一次。
fn is_in_window(env: &mut Environment, this: id) -> bool {
    let window: id = msg![env; this window];
    window != nil
}

/// [复核修 2026-09-15] R2-1 返修:这次抬起会不会被 touchesEnded: 当作拖动结束收尾(跟踪的触摸在
/// `touches` 里且已越过起拖阈值),与 touchesEnded: 里 `was_dragging` 的判定条件相同。
/// UITableView 在调 super touchesEnded: **之前**用它排除点选:表格自己的点选阈值按窗口坐标、
/// 从 touchesBegan: 起算,和这里的起拖判定(本视图坐标、`hypot >= 10` 起拖、跟踪起点可能晚到
/// 第一次移动)对不齐,边界上会出现"已经滚动又点选一行"。只看跟踪中的这个触摸,不看 `isDragging`
/// 全局状态,免得残留的旧跟踪(取消没传到本视图)把后续所有点选都挡掉。纯查询,不跑游戏代码。
pub(super) fn ends_drag_in(env: &mut Environment, this: id, touches: id) -> bool {
    let (tracked, dragging) = {
        let host = env.objc.borrow::<UIScrollViewHostObject>(this);
        (host.tracked_touch, host.dragging)
    };
    dragging && tracked != nil && touches_contain(env, touches, tracked)
}

/// 触摸集合里的第一个触摸(单指假设,与原实现一致);空集合返回 nil。
fn first_touch(env: &mut Environment, touches: id) -> id {
    if touches == nil {
        return nil;
    }
    let touch_arr: id = msg![env; touches allObjects];
    let count: NSUInteger = msg![env; touch_arr count];
    if count == 0 {
        return nil;
    }
    msg![env; touch_arr objectAtIndex:0u32]
}

fn touches_contain(env: &mut Environment, touches: id, touch: id) -> bool {
    if touches == nil || touch == nil {
        return false;
    }
    let touch_arr: id = msg![env; touches allObjects];
    let count: NSUInteger = msg![env; touch_arr count];
    for i in 0..count {
        let t: id = msg![env; touch_arr objectAtIndex:i];
        if t == touch {
            return true;
        }
    }
    false
}

/// 取响应 `sel_name` 的委托。返回的委托已 retain(回调里委托可能把自己释放),调用方用完 release。
fn delegate_responding_to(env: &mut Environment, this: id, sel_name: &str) -> Option<(id, SEL)> {
    let delegate: id = msg![env; this delegate];
    if delegate == nil {
        return None;
    }
    let sel: SEL = env
        .objc
        .register_host_selector(sel_name.to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if !responds {
        return None;
    }
    retain(env, delegate);
    Some((delegate, sel))
}

/// 发单参数(scrollView)的委托回调。
fn notify_delegate(env: &mut Environment, this: id, sel_name: &str) {
    if let Some((delegate, sel)) = delegate_responding_to(env, this, sel_name) {
        () = msg_send(env, (delegate, sel, this));
        release(env, delegate);
    }
}

/// 结束跟踪:释放跟踪的触摸。返回之前是否处于拖动中。
fn stop_tracking(env: &mut Environment, this: id) -> bool {
    let host = env.objc.borrow_mut::<UIScrollViewHostObject>(this);
    let old_touch = std::mem::take(&mut host.tracked_touch);
    let was_dragging = std::mem::replace(&mut host.dragging, false);
    release(env, old_touch);
    was_dragging
}

/// 开始跟踪一次触摸。`start_location` 为本视图坐标。
/// 另一根仍按着的手指正在被跟踪时返回 false(单指假设);跟踪着已抬起的触摸
/// (结束消息没传到这里)时先补发拖动收尾回调再换新触摸。
fn start_tracking(env: &mut Environment, this: id, touch: id, start_location: CGPoint) -> bool {
    let tracked = env
        .objc
        .borrow::<UIScrollViewHostObject>(this)
        .tracked_touch;
    if tracked == touch {
        return true;
    }
    if tracked != nil {
        let phase: NSInteger = msg![env; tracked phase];
        // [复核修 2026-09-15] 取消(滚轮虚拟捏合结束)也算已结束,否则取消消息没传到这里时会一直拒绝新触摸。
        if phase != UITouchPhaseEnded && phase != UITouchPhaseCancelled {
            return false;
        }
        if stop_tracking(env, this) {
            finish_drag(env, this);
        }
    }
    let offset: CGPoint = msg![env; this contentOffset];
    retain(env, touch);
    let host = env.objc.borrow_mut::<UIScrollViewHostObject>(this);
    let old_touch = std::mem::replace(&mut host.tracked_touch, touch);
    host.dragging = false;
    host.track_start_location = start_location;
    host.track_start_offset = offset;
    release(env, old_touch);
    true
}

/// 单轴翻页吸附目标。`begin` 是开始拖动时的 offset。
/// 页长非正/非有限(零页宽)时只夹紧不吸附,防除零。
fn paging_snap(offset: CGFloat, begin: CGFloat, page_len: CGFloat, content_len: CGFloat) -> CGFloat {
    let max_offset = (content_len - page_len).max(0.0);
    if !offset.is_finite() {
        return 0.0;
    }
    if !(page_len.is_finite() && page_len > 0.0) {
        return offset.min(max_offset).max(0.0);
    }
    let begin_page = if begin.is_finite() {
        (begin / page_len).round()
    } else {
        0.0
    };
    let mut page = (offset / page_len).round();
    let moved = offset - begin_page * page_len;
    if page == begin_page && moved.abs() > page_len * PAGING_FLICK_RATIO {
        page = begin_page + moved.signum();
    }
    (page * page_len).min(max_offset).max(0.0)
}

/// 拖动结束收尾:翻页吸附 + 委托回调(见文件头注释的顺序说明)。
fn finish_drag(env: &mut Environment, this: id) {
    let mut snap_target: Option<CGPoint> = None;
    let paging_enabled = env
        .objc
        .borrow::<UIScrollViewHostObject>(this)
        .paging_enabled;
    if paging_enabled {
        let bounds: CGRect = msg![env; this bounds];
        let content_size: CGSize = msg![env; this contentSize];
        let offset: CGPoint = msg![env; this contentOffset];
        let begin = env
            .objc
            .borrow::<UIScrollViewHostObject>(this)
            .drag_begin_offset;
        let target = CGPoint {
            x: paging_snap(offset.x, begin.x, bounds.size.width, content_size.width),
            y: paging_snap(offset.y, begin.y, bounds.size.height, content_size.height),
        };
        log!(
            "[扫描修 2026-09-15] UIScrollView {:?} 翻页拖动结束:offset {:?} -> {:?}(页宽 {} 页高 {})",
            this,
            offset,
            target,
            { bounds.size.width },
            { bounds.size.height }
        );
        if target != offset {
            snap_target = Some(target);
        }
    }

    let will_decelerate = snap_target.is_some();
    if let Some((delegate, sel)) =
        delegate_responding_to(env, this, "scrollViewDidEndDragging:willDecelerate:")
    {
        () = msg_send(env, (delegate, sel, this, will_decelerate));
        release(env, delegate);
    }
    if let Some(target) = snap_target {
        notify_delegate(env, this, "scrollViewWillBeginDecelerating:");
        () = msg![env; this setContentOffset:target];
        notify_delegate(env, this, "scrollViewDidScroll:");
        notify_delegate(env, this, "scrollViewDidEndDecelerating:");
    }
}

/// 移动阶段的滚动逻辑(原 touchesMoved: 的实现 + 起拖判定)。
fn scroll_touches_moved(env: &mut Environment, this: id, touches: id, event: id) {
    let scroll_enabled: bool = msg![env; this scrollEnabled];
    if !scroll_enabled {
        return;
    }
    // [复核修 2026-09-15] R2-2:触摸过程中本视图被摘出窗口(游戏回调里拆视图树)后,
    // 下面的 previousLocationInView:/locationInView:this 会 panic,不在窗口里就不滚动。
    if !is_in_window(env, this) {
        return;
    }

    // Assume single finger touches for now
    let touch = first_touch(env, touches);
    if touch == nil {
        return;
    }

    // 本次手势第一次到这里(按下被子类吞掉了,例如 UITextView 覆盖 touchesBegan: 不调 super):
    // 以移动前的位置作为起点开始跟踪。
    let tracked = env
        .objc
        .borrow::<UIScrollViewHostObject>(this)
        .tracked_touch;
    if tracked != touch {
        let prev: CGPoint = msg![env; touch previousLocationInView:this];
        if !start_tracking(env, this, touch, prev) {
            return;
        }
    }

    let prev_location: CGPoint = msg![env; touch previousLocationInView:this];
    let new_location: CGPoint = msg![env; touch locationInView:this];

    let (dragging, track_start_location, track_start_offset) = {
        let host = env.objc.borrow::<UIScrollViewHostObject>(this);
        (
            host.dragging,
            host.track_start_location,
            host.track_start_offset,
        )
    };

    let (delta_x, delta_y) = if dragging {
        (
            new_location.x - prev_location.x,
            new_location.y - prev_location.y,
        )
    } else {
        // 起点换算到当前 bounds 原点下(开始跟踪后 offset 若被程序改过要补上差值)。
        let offset: CGPoint = msg![env; this contentOffset];
        let start_x = track_start_location.x + (offset.x - track_start_offset.x);
        let start_y = track_start_location.y + (offset.y - track_start_offset.y);
        let dx = new_location.x - start_x;
        let dy = new_location.y - start_y;
        if dx.hypot(dy) < DRAG_START_THRESHOLD {
            return;
        }
        {
            let host = env.objc.borrow_mut::<UIScrollViewHostObject>(this);
            host.dragging = true;
            host.drag_begin_offset = offset;
        }
        // 拖动接管这次触摸:同一触摸上的点击识别器失败,响应链上方收到 touchesCancelled:。
        ui_gesture_recognizer::fail_recognizers_tracking(env, touch);
        forward_touches_if_enabled(env, this, TouchStage::Cancelled, touches, event);
        // 即使接下来 offset 被夹住不动也要发(最后一页"再滑一次"关闭教程靠它)。
        notify_delegate(env, this, "scrollViewWillBeginDragging:");
        // 回调里跟踪被结束了(游戏把触摸取消回来之类),就不再滚动。
        if env
            .objc
            .borrow::<UIScrollViewHostObject>(this)
            .tracked_touch
            != touch
        {
            return;
        }
        // 起拖这一次的位移从起点算,阈值内那一段不丢。
        (dx, dy)
    };

    let bounds: CGRect = msg![env; this bounds];
    let offset: CGPoint = msg![env; this contentOffset];
    let content_size: CGSize = msg![env; this contentSize];

    // Very rudimentary scrolling.
    // We emulate sliding up to scroll down like on the real iPhone.
    let mut new_content_offset: CGPoint = CGPoint { x: offset.x - delta_x, y: offset.y - delta_y };

    // Update content offset within bounds
    new_content_offset.y = new_content_offset.y.min(content_size.height - bounds.size.height).max(0.0);
    new_content_offset.x = new_content_offset.x.min(content_size.width - bounds.size.width).max(0.0);

    // Trigger rerender only if required.
    log_dbg!("content offset: old {:?}, new {:?}", offset, new_content_offset);
    if new_content_offset != offset {
        () = msg![env; this setContentOffset:new_content_offset];
        notify_delegate(env, this, "scrollViewDidScroll:");
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIScrollView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIScrollViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// [扫描修 2026-09-15] F8-2:释放跟踪中的触摸。只取走这一个字段,不能 take 整个宿主对象
// (否则内嵌的 UIViewHostObject 被清成 Default,UIView dealloc 就不再释放 layer/子视图/识别器)。
- (())dealloc {
    let tracked_touch = std::mem::take(
        &mut env.objc.borrow_mut::<UIScrollViewHostObject>(this).tracked_touch,
    );
    release(env, tracked_touch);
    msg_super![env; this dealloc]
}

- (id)delegate {
    env.objc.borrow::<UIScrollViewHostObject>(this).delegate
}
- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).delegate = delegate;
}

- (())setDelaysContentTouches:(id)_delay_content_touches{
    // TODO
}
- (())setBounces:(id)_bounces {
    // TODO
}

- (bool)scrollEnabled {
    env.objc.borrow::<UIScrollViewHostObject>(this).scroll_enabled
}
- (())setScrollEnabled:(bool)scroll_enabled {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).scroll_enabled = scroll_enabled;
}

// [扫描修 2026-09-15] F8-2:翻页开关(-[ATPagingView commonInit]@0x38c376 设为 YES)。
- (bool)isPagingEnabled {
    env.objc.borrow::<UIScrollViewHostObject>(this).paging_enabled
}
- (())setPagingEnabled:(bool)enabled {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).paging_enabled = enabled;
}

// [扫描修 2026-09-15] F8-2:滚动条显隐只存值(touchHLE 不画滚动条),免得落进"不响应选择子"日志。
- (bool)showsHorizontalScrollIndicator {
    env.objc.borrow::<UIScrollViewHostObject>(this).shows_horizontal_scroll_indicator
}
- (())setShowsHorizontalScrollIndicator:(bool)shows {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).shows_horizontal_scroll_indicator = shows;
}
- (bool)showsVerticalScrollIndicator {
    env.objc.borrow::<UIScrollViewHostObject>(this).shows_vertical_scroll_indicator
}
- (())setShowsVerticalScrollIndicator:(bool)shows {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).shows_vertical_scroll_indicator = shows;
}

// [扫描修 2026-09-15] F8-2:拖动状态查询。
- (bool)isTracking {
    env.objc.borrow::<UIScrollViewHostObject>(this).tracked_touch != nil
}
- (bool)isDragging {
    env.objc.borrow::<UIScrollViewHostObject>(this).dragging
}
- (bool)isDecelerating {
    // 没有惯性滚动
    false
}

- (CGPoint)contentOffset {
    env.objc.borrow::<UIScrollViewHostObject>(this).content_offset
}
- (())setContentOffset:(CGPoint)offset {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).content_offset = offset;
    // Bounds origin should be equals to the content offset
    let mut bounds: CGRect = msg![env; this bounds];
    bounds.origin = offset;
    () = msg![env; this setBounds:bounds];
    () = msg![env; this setNeedsDisplay];
}

- (CGSize)contentSize {
    env.objc.borrow::<UIScrollViewHostObject>(this).content_size
}
- (())setContentSize:(CGSize)size {
    env.objc.borrow_mut::<UIScrollViewHostObject>(this).content_size = size;
}

- (())setIndicatorStyle:(UIScrollViewIndicatorStyle)style {
    todo_objc_setter!(this, style);
}

// [扫描修 2026-09-15] F8-2:按下 = 手势识别挂点 + 开始跟踪 + 保持原来的 UIResponder 转发。
- (())touchesBegan:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    // 委托回调与转发都会跑游戏代码,可能把本视图拆下释放,处理期间 retain 住。
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Began);
    let scroll_enabled: bool = msg![env; this scrollEnabled];
    let touch = first_touch(env, touches);
    // [复核修 2026-09-15] R2-2:识别器委托回调可能已把本视图摘出窗口,离窗时不换算坐标、不开始跟踪。
    if scroll_enabled && touch != nil && is_in_window(env, this) {
        let location: CGPoint = msg![env; touch locationInView:this];
        let _ = start_tracking(env, this, touch, location);
    }
    forward_touches_if_enabled(env, this, TouchStage::Began, touches, event);
    release(env, this);
}

- (())touchesMoved:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Moved);
    // 移动消息与原实现一样不往上转发。
    scroll_touches_moved(env, this, touches, event);
    release(env, this);
}

// [扫描修 2026-09-15] F8-2:抬起。拖动中 → 翻页吸附 + 拖动结束回调,不再转发;
// 没拖动(点击)→ 按识别结果转发 touchesEnded:/touchesCancelled:,再派发识别器 action。
- (())touchesEnded:(id)touches // NSSet* of UITouch*
         withEvent:(id)event { // UIEvent*
    retain(env, this);
    let recognized = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Ended);
    let tracked = env.objc.borrow::<UIScrollViewHostObject>(this).tracked_touch;
    let was_dragging = if tracked != nil && touches_contain(env, touches, tracked) {
        stop_tracking(env, this)
    } else {
        false
    };
    if was_dragging {
        finish_drag(env, this);
    } else {
        let stage = if ui_gesture_recognizer::cancels_touches_in_view(env, &recognized) {
            TouchStage::Cancelled
        } else {
            TouchStage::Ended
        };
        forward_touches_if_enabled(env, this, stage, touches, event);
    }
    // 先转发、后派发:action 可能同步拆掉视图树(见 ui_gesture_recognizer.rs 模块注释)。
    ui_gesture_recognizer::fire_recognized(env, recognized);
    release(env, this);
}

// [扫描修 2026-09-15] F8-2:取消。拖动中照样收尾(拖动开始时已经往上发过取消);否则继续往上转发。
- (())touchesCancelled:(id)touches // NSSet* of UITouch*
             withEvent:(id)event { // UIEvent*
    retain(env, this);
    let _ = ui_gesture_recognizer::process_touches(env, touches, TouchStage::Cancelled);
    let tracked = env.objc.borrow::<UIScrollViewHostObject>(this).tracked_touch;
    let was_dragging = if tracked != nil && touches_contain(env, touches, tracked) {
        stop_tracking(env, this)
    } else {
        false
    };
    if was_dragging {
        finish_drag(env, this);
    } else {
        forward_touches_if_enabled(env, this, TouchStage::Cancelled, touches, event);
    }
    release(env, this);
}

@end

};
