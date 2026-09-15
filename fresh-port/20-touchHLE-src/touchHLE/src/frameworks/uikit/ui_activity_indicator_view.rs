/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIActivityIndicatorView`.

use super::ui_graphics::UIGraphicsGetCurrentContext;
use crate::frameworks::core_graphics::cg_context::{
    CGContextFillRect, CGContextRef, CGContextRestoreGState, CGContextRotateCTM,
    CGContextSaveGState, CGContextSetRGBFillColor, CGContextTranslateCTM,
};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{NSInteger, NSTimeInterval};
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes, release,
    ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

type UIActivityIndicatorViewStyle = NSInteger;
const UIActivityIndicatorViewStyleWhiteLarge: UIActivityIndicatorViewStyle = 0;
const UIActivityIndicatorViewStyleWhite: UIActivityIndicatorViewStyle = 1;
const UIActivityIndicatorViewStyleGray: UIActivityIndicatorViewStyle = 2;

/// 辐条数(与 iOS 原版一致:12 根,每步 30°)。
const SPOKES: u32 = 12;
/// [扫描修 2026-09-15] F12-7:步进间隔。iOS 原版约 1 秒转一圈 = 每 1/12 秒走一步。
const SPIN_INTERVAL: NSTimeInterval = 1.0 / 12.0;

pub struct UIActivityIndicatorViewHostObject {
    superclass: super::ui_view::UIViewHostObject,
    animating: bool,
    /// [深扫修 2026-09-11] #23(c):指示器样式,决定固有尺寸与颜色。
    style: UIActivityIndicatorViewStyle,
    /// [深扫修 2026-09-11] #23(c):iOS 默认 YES —— 未在转动时隐藏自身。
    hides_when_stopped: bool,
    /// [扫描修 2026-09-15] F12-7:驱动转圈的重复 NSTimer(强引用);未转动时为 nil。
    spin_timer: id,
    /// [扫描修 2026-09-15] F12-7:定时器的 target(强引用)。NSTimer 会强持有 target,
    /// 若直接拿本视图当 target 会形成"视图 ↔ 定时器"循环引用,MBProgressHUD 释放
    /// 指示器后它永远不 dealloc、定时器永远在跑。所以用一个只弱持有本视图的小对象中转,
    /// 本视图 dealloc / stopAnimating 时 invalidate 并切断弱引用。
    spin_ticker: id,
    /// [扫描修 2026-09-15] F12-7:当前高亮相位(0..SPOKES)。
    spin_phase: u32,
}
impl_HostObject_with_superclass!(UIActivityIndicatorViewHostObject);

/// [扫描修 2026-09-15] F12-7:定时器中转对象,弱持有指示器视图。
struct SpinTickerHostObject {
    view: id,
}
impl HostObject for SpinTickerHostObject {}

/// [深扫修 2026-09-11] #23(c):各样式的固有边长(与 iOS 一致:WhiteLarge 37×37,
/// White/Gray 20×20)。
fn intrinsic_side(style: UIActivityIndicatorViewStyle) -> CGFloat {
    if style == UIActivityIndicatorViewStyleWhiteLarge {
        37.0
    } else {
        20.0
    }
}

/// [深扫修 2026-09-11] #23(c):初始化后的共同设置。
/// - 不透明设为 NO:否则合成器对"不透明且无背景色"的层关闭混合,透明像素会
///   变成黑方块(iOS 上该控件本身也是非不透明的)。
/// - 边界变化时重绘,并立即标记需要绘制(否则 drawRect: 永远不会被调用)。
/// - hidesWhenStopped(默认 YES)且未转动 → 隐藏,与 iOS 行为一致。
fn setup_after_init(env: &mut Environment, this: id) {
    () = msg![env; this setOpaque:false];
    let layer: id = msg![env; this layer];
    () = msg![env; layer setNeedsDisplayOnBoundsChange:true];
    () = msg![env; this setNeedsDisplay];
    let host_obj = env.objc.borrow::<UIActivityIndicatorViewHostObject>(this);
    if host_obj.hides_when_stopped && !host_obj.animating {
        () = msg![env; this setHidden:true];
    }
}

/// [深扫修 2026-09-11] #23(c):绘制 12 根辐条的菊花(按固有尺寸居中,
/// 不随 frame 拉伸,与 iOS 一致)。
///
/// [扫描修 2026-09-15] F12-7:加 `phase` 参数,整组辐条按相位整体旋转 phase×30°,
/// 由定时器每 1/12 秒推进一步 = iOS 原样的离散步进转圈。
/// 取舍(与复核 value_note 的差异):没有用 setTransform: 转整个视图。touchHLE 的
/// CALayer -frame 会把 affineTransform 算进包围盒(ca_layer.rs frame 走
/// superlayer_to_layer_transform),而 MBProgressHUD 在 layoutSubviews 里按指示器
/// 尺寸算底框与居中位置,视图一转 frame 就在 37↔50 之间抖,HUD 会跟着跳。
/// 重绘 37×37 的小位图每秒 12 次开销可以忽略,也不分配 ObjC 对象。
fn draw_spinner(
    env: &mut Environment,
    context: CGContextRef,
    center: CGPoint,
    side: CGFloat,
    rgb: (CGFloat, CGFloat, CGFloat),
    phase: u32,
) {
    let inner_radius = side * 0.22;
    let outer_radius = side * 0.48;
    let thickness = (side * 0.085).max(1.5);
    let step = -2.0 * std::f32::consts::PI / SPOKES as CGFloat;

    CGContextSaveGState(env, context);
    CGContextTranslateCTM(env, context, center.x, center.y);
    // 头部辐条沿"与拖尾相反"的方向前进:拖尾在 +step 方向,所以每相位转 -step。
    CGContextRotateCTM(env, context, -step * (phase % SPOKES) as CGFloat);
    for i in 0..SPOKES {
        // 头部辐条最亮,依次变淡。
        let alpha = (1.0 - i as CGFloat * 0.07).max(0.2);
        CGContextSetRGBFillColor(env, context, rgb.0, rgb.1, rgb.2, alpha);
        CGContextFillRect(
            env,
            context,
            CGRect {
                origin: CGPoint {
                    x: inner_radius,
                    y: -thickness / 2.0,
                },
                size: CGSize {
                    width: outer_radius - inner_radius,
                    height: thickness,
                },
            },
        );
        CGContextRotateCTM(env, context, step);
    }
    CGContextRestoreGState(env, context);
}

/// [扫描修 2026-09-15] F12-7:开始转圈(已在转则不重复建定时器)。
fn start_spin_timer(env: &mut Environment, this: id) {
    if env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).spin_timer != nil {
        return;
    }
    let ticker_class = env
        .objc
        .get_known_class("_touchHLE_UIActivityIndicatorTicker", &mut env.mem);
    let ticker: id = msg![env; ticker_class alloc]; // +1,由本视图持有
    env.objc.borrow_mut::<SpinTickerHostObject>(ticker).view = this;
    let sel = env
        .objc
        .register_host_selector("_touchHLE_spinStep:".to_string(), &mut env.mem);
    // scheduledTimer… 返回 autoreleased,run loop 持有一份;这里再 retain 一份以便 invalidate。
    let timer: id = msg_class![env; NSTimer scheduledTimerWithTimeInterval:SPIN_INTERVAL
                                                                    target:ticker
                                                                  selector:sel
                                                                  userInfo:nil
                                                                   repeats:true];
    let timer: id = msg![env; timer retain];
    let host = env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this);
    host.spin_timer = timer;
    host.spin_ticker = ticker;
}

/// [扫描修 2026-09-15] F12-7:停止转圈:invalidate 定时器、切断中转对象的弱引用、相位归零。
fn stop_spin_timer(env: &mut Environment, this: id) {
    let (timer, ticker) = {
        let host = env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this);
        host.spin_phase = 0;
        (
            std::mem::replace(&mut host.spin_timer, nil),
            std::mem::replace(&mut host.spin_ticker, nil),
        )
    };
    if timer != nil {
        () = msg![env; timer invalidate];
        release(env, timer);
    }
    if ticker != nil {
        env.objc.borrow_mut::<SpinTickerHostObject>(ticker).view = nil;
        release(env, ticker);
    }
}

/// [扫描修 2026-09-15] F12-7:定时器每步:推进相位并请求重绘。
/// 已隐藏或已从视图树摘下时跳过,省掉无意义的位图绘制(定时器照常保留,重新挂上后继续转)。
fn spin_step(env: &mut Environment, view: id) {
    let superview: id = msg![env; view superview];
    if superview == nil {
        return;
    }
    let hidden: bool = msg![env; view isHidden];
    if hidden {
        return;
    }
    {
        let host = env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(view);
        host.spin_phase = (host.spin_phase + 1) % SPOKES;
    }
    () = msg![env; view setNeedsDisplay];
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIActivityIndicatorView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIActivityIndicatorViewHostObject {
        superclass: Default::default(),
        animating: false,
        // iOS: initWithFrame: 创建的默认样式是 White。
        style: UIActivityIndicatorViewStyleWhite,
        hides_when_stopped: true,
        spin_timer: nil,
        spin_ticker: nil,
        spin_phase: 0,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// [深扫修 2026-09-11] #23(c):覆盖 initWithFrame:,补齐控件的初始化设置。
- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    if this != nil {
        setup_after_init(env, this);
    }
    this
}

// [深扫修 2026-09-11] #23(c):按样式给出固有尺寸。
// 根因:此前只是 `[this init]`,frame 为 0×0 且什么都不画;MBProgressHUD
// (-updateIndicators@0x12b254 用 WhiteLarge)在 layoutSubviews 里按指示器
// 宽高 + 2×margin 算底框大小,0×0 时底框只剩 2×margin、里面还是空的。
- (id)initWithActivityIndicatorStyle:(UIActivityIndicatorViewStyle)style {
    // 先写样式,initWithFrame: 里的 setNeedsDisplay 之后的绘制会用到它。
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).style = style;
    let side = intrinsic_side(style);
    let frame = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize { width: side, height: side },
    };
    msg![env; this initWithFrame:frame]
}

// [扫描修 2026-09-15] F12-7:释放前停掉定时器并切断中转对象的弱引用,避免野指针重绘。
- (())dealloc {
    stop_spin_timer(env, this);
    msg_super![env; this dealloc]
}

- (UIActivityIndicatorViewStyle)activityIndicatorViewStyle {
    env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).style
}
- (())setActivityIndicatorViewStyle:(UIActivityIndicatorViewStyle)style {
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).style = style;
    () = msg![env; this setNeedsDisplay];
}

- (CGSize)sizeThatFits:(CGSize)_size {
    let side = intrinsic_side(env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).style);
    CGSize { width: side, height: side }
}

// [扫描修 2026-09-15] F12-7:真正转起来(原先只置标志并打 TODO 日志)。
- (())startAnimating {
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).animating = true;
    // [深扫修 2026-09-11] #23(c):hidesWhenStopped 语义 —— 开始转动时显示。
    if env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped {
        () = msg![env; this setHidden:false];
    }
    start_spin_timer(env, this);
}
- (())stopAnimating {
    env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this).animating = false;
    stop_spin_timer(env, this);
    () = msg![env; this setNeedsDisplay];
    // [深扫修 2026-09-11] #23(c):hidesWhenStopped 语义 —— 停止时隐藏。
    if env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped {
        () = msg![env; this setHidden:true];
    }
}

- (bool)isAnimating {
    env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).animating
}

- (bool)hidesWhenStopped {
    env.objc.borrow::<UIActivityIndicatorViewHostObject>(this).hides_when_stopped
}
- (())setHidesWhenStopped:(bool)hides {
    // [深扫修 2026-09-11] #23(c):此前是空实现。未转动时隐藏状态跟随该属性。
    let host_obj = env.objc.borrow_mut::<UIActivityIndicatorViewHostObject>(this);
    host_obj.hides_when_stopped = hides;
    if !host_obj.animating {
        () = msg![env; this setHidden:hides];
    }
}

// [深扫修 2026-09-11] #23(c):静态绘制(见 draw_spinner);[扫描修 2026-09-15] 按相位绘制。
- (())drawRect:(CGRect)_rect {
    let context = UIGraphicsGetCurrentContext(env);
    if context.is_null() {
        return;
    }
    let (style, phase) = {
        let host = env.objc.borrow::<UIActivityIndicatorViewHostObject>(this);
        (host.style, host.spin_phase)
    };
    let bounds: CGRect = msg![env; this bounds];
    let center = CGPoint {
        x: bounds.origin.x + bounds.size.width / 2.0,
        y: bounds.origin.y + bounds.size.height / 2.0,
    };
    let rgb = if style == UIActivityIndicatorViewStyleGray {
        (0.5, 0.5, 0.5)
    } else {
        (1.0, 1.0, 1.0)
    };
    draw_spinner(env, context, center, intrinsic_side(style), rgb, phase);
}

@end

// [扫描修 2026-09-15] F12-7:NSTimer 的 target 中转对象(见 spin_ticker 字段说明)。
@implementation _touchHLE_UIActivityIndicatorTicker: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(SpinTickerHostObject { view: nil });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())_touchHLE_spinStep:(id)_timer {
    let view = env.objc.borrow::<SpinTickerHostObject>(this).view;
    if view != nil {
        spin_step(env, view);
    }
}

@end

};
