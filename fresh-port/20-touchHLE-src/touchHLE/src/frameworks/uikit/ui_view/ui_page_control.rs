/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIPageControl`.
//!
//! [同步上游 0.3.0 2026-10-03] 上游 558eced3 加的是只有两个 TODO setter 的空桩:它是
//! UIControl 子类,会接管落在自己身上的触摸,却既不翻页、也不发 ValueChanged。分叉点没有
//! 这个类,游戏里 [UIPageControl alloc] 得 nil、相关消息全是空操作,所以合并后 VIP 教程页
//! 和漂流瓶教程页(-[FeatureIntroductionView initWithFrame:background:andPicturesArray:]
//! @0x38ef32 建一个 60×20 的页控件,addTarget:self action:@selector(pageChanged:)
//! forControlEvents:ValueChanged;由 -[WrapperManager showVipTutorialsView]@0x2641f2 /
//! showDriftBottleTutorialsView@0x2642ee 弹出)会多出一块吞点击的透明区域。
//!
//! 这里按原版 UIKit 语义补一个最小实现:保存页数、当前页和「只有一页时隐藏」;点在控件
//! 左半边翻到上一页、右半边翻到下一页(夹在 0..页数-1),页码真的变了才发 ValueChanged,
//! 游戏的 pageChanged:@0x38f104 读 currentPage 后调 setCurrentPageIndex: 翻页。
//! 「只有一页时隐藏」生效时不参与命中测试,点击照常落到下面的视图。圆点暂不绘制(TODO)。

use super::ui_control::{send_actions, UIControlEventValueChanged, UIControlHostObject};
use crate::frameworks::core_graphics::{CGPoint, CGRect};
use crate::frameworks::foundation::NSInteger;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_super, objc_classes, ClassExports, NSZonePtr,
};
use crate::Environment;

#[derive(Default)]
struct UIPageControlHostObject {
    superclass: UIControlHostObject,
    number_of_pages: NSInteger,
    current_page: NSInteger,
    hides_for_single_page: bool,
}
impl_HostObject_with_superclass!(UIPageControlHostObject);

/// 把页码夹到 0..页数-1(页数为 0 时为 0)。
fn clamp_page(page: NSInteger, number_of_pages: NSInteger) -> NSInteger {
    page.clamp(0, (number_of_pages - 1).max(0))
}

/// 「只有一页时隐藏」生效:不显示,也不拦截点击。
fn hidden_for_single_page(env: &Environment, this: id) -> bool {
    let host = env.objc.borrow::<UIPageControlHostObject>(this);
    host.hides_for_single_page && host.number_of_pages <= 1
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIPageControl: UIControl

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIPageControlHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (NSInteger)numberOfPages {
    env.objc.borrow::<UIPageControlHostObject>(this).number_of_pages
}
- (())setNumberOfPages:(NSInteger)number_of_pages {
    let host = env.objc.borrow_mut::<UIPageControlHostObject>(this);
    host.number_of_pages = number_of_pages.max(0);
    host.current_page = clamp_page(host.current_page, host.number_of_pages);
}

- (NSInteger)currentPage {
    env.objc.borrow::<UIPageControlHostObject>(this).current_page
}
- (())setCurrentPage:(NSInteger)current_page {
    let host = env.objc.borrow_mut::<UIPageControlHostObject>(this);
    host.current_page = clamp_page(current_page, host.number_of_pages);
}

- (bool)hidesForSinglePage {
    env.objc.borrow::<UIPageControlHostObject>(this).hides_for_single_page
}
- (())setHidesForSinglePage:(bool)hides {
    env.objc.borrow_mut::<UIPageControlHostObject>(this).hides_for_single_page = hides;
}

- (bool)pointInside:(CGPoint)point
          withEvent:(id)event { // UIEvent* (possibly nil)
    if hidden_for_single_page(env, this) {
        return false;
    }
    msg_super![env; this pointInside:point withEvent:event]
}

- (())endTrackingWithTouch:(id)touch // UITouch*
                 withEvent:(id)event { // UIEvent*
    () = msg_super![env; this endTrackingWithTouch:touch withEvent:event];

    // 与 UIControl 的 touchesEnded:withEvent: 一样,只认在控件内抬起的点击。
    let pos: CGPoint = msg![env; touch locationInView:this];
    let inside: bool = msg![env; this pointInside:pos withEvent:event];
    if !inside {
        return;
    }
    let bounds: CGRect = msg![env; this bounds];
    let mid_x = bounds.origin.x + bounds.size.width / 2.0;

    let host = env.objc.borrow_mut::<UIPageControlHostObject>(this);
    let old_page = host.current_page;
    let new_page = clamp_page(
        if pos.x < mid_x { old_page - 1 } else { old_page + 1 },
        host.number_of_pages,
    );
    if new_page == old_page {
        return;
    }
    host.current_page = new_page;
    log_dbg!("[(UIPageControl*){:?}] 点击翻页 {} -> {}", this, old_page, new_page);
    send_actions(env, this, event, UIControlEventValueChanged);
}

@end

};
