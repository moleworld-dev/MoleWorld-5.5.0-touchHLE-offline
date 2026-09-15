/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITableView`、`UITableViewCell`、`UITableViewController`(最小子集)。
//!
//! [扫描修 2026-09-15] F8-1 / F9-7 / F11-8:这三个类原先不存在,touchHLE 为它们建
//! UnimplementedClass 占位,消息沿父类链命中占位就按 nil 处理 → 游戏自己的
//! MessageViewController / SeekViewController / FriendsViewController(都继承
//! UITableViewController)alloc 得 nil,留言板、寻找好友结果、好友列表只有 cocos2d 外框;
//! 淘米账号菜单的历史账号列表(-[TMAUserIDListView backLayerAddTableWithFrame:] 直接
//! [UITableView alloc] initWithFrame:style:)同样空白。
//!
//! 实现范围以 re.py 核对到的实际调用为准:
//! - 控制器:initWithStyle:(经 initWithStyle:listArray: 的 [super initWithStyle:])、
//!   tableView、view 懒加载(loadView 建全尺寸表格,dataSource/delegate = self)。
//! - 表格:initWithFrame:style:、setDataSource:/setDelegate:、setRowHeight:、
//!   setSeparatorStyle:/setSeparatorColor:/setAllowsSelection:、setScrollEnabled:/setBounces:
//!   (UIScrollView 已有)、reloadData、dequeueReusableCellWithIdentifier:、
//!   indexPathForCell:(TMAUserIDListView deleteBtnClick:)、
//!   deleteRowsAtIndexPaths:withRowAnimation:(好友删除 / 历史账号删除)、
//!   tableView:didSelectRowAtIndexPath:(历史账号选中登录)。
//! - cell:initWithStyle:reuseIdentifier:、contentView(游戏往里 addSubview 并用
//!   viewWithTag: 取回)、textLabel/imageView(TMAUserIDListView)、setSelected:animated:
//!   (MessageCell 覆盖后调 super)。
//!
//! 与 iOS 一致的关键语义:
//! - reloadData 同步问 numberOfSections/numberOfRows/heightForRow 算 contentSize,
//!   cell 则在下一次 layoutSubviews(合成前的布局遍历)里只为**可见行**向 dataSource 要,
//!   滚出可见区的 cell 进复用池,dequeueReusableCellWithIdentifier: 从池里取。
//!   只建可见行还有一个 touchHLE 特有的理由:合成器没有 clipsToBounds/masksToBounds
//!   (composition.rs TODO),全部行都建出来会画到表格外面、盖住游戏画面。
//! - iOS ≤6 普通样式表格会在显示前把 cell 背景色设成表格背景色,再给 delegate 一次
//!   tableView:willDisplayCell:forRowAtIndexPath: 机会;这里照做。
//! - UITableView 自己处理触摸,不再沿响应链转发给下面的 EAGLView(否则点列表会穿透到
//!   游戏场景);点按(未拖动)时走 willSelect → 选中 → didSelect。
//!   [复核修 2026-09-15] R2-1:UIScrollView 补齐四个触摸方法后,msg_super 进去会沿响应链转发;
//!   现在表格的宿主对象建出来就关掉 UIScrollView 的转发(`without_touch_forwarding`,在
//!   allocWithZone: 用的 Default 里设,覆盖所有 init 路径和游戏子类),滚动跟踪/识别器照常。
//!
//! 未做(列出来便于按 does-not-respond 日志迭代):分区头/尾标题与视图、分隔线绘制、
//! 选中高亮、编辑/滑动删除、行动画、索引条、nib 载入 cell。
//!
//! NSIndexPath:Foundation 的 ns_index_path.rs 是空实现(不在本包归属内),而 touchHLE 的
//! 类模板按名字只取第一个、不支持宿主侧 category,无法在这里给 NSIndexPath 补
//! indexPathForRow:inSection:/row/section。所以本文件提供私有子类 `_touchHLE_NSIndexPath`
//! (row/section/length/indexAtPosition:/isEqual:/hash/compare:),表格交给游戏的索引路径
//! 全部用它。游戏自己 [NSIndexPath indexPathForRow:inSection:] 的调用点只在 SDK
//! (SHK/ASI/DM 等),不受影响;若以后补全 ns_index_path.rs,可删掉此子类。

use crate::frameworks::core_graphics::cg_color::CGColorRef;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::to_rust_string;
use crate::frameworks::foundation::{ns_array, NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_view_controller::{view_if_loaded, UIViewControllerHostObject};
use crate::objc::{
    autorelease, id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil,
    objc_classes, release, retain, Class, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;
use std::collections::HashMap;

const UITableViewCellStyleDefault: NSInteger = 0;
const UITableViewCellStyleValue1: NSInteger = 1;
const UITableViewCellStyleValue2: NSInteger = 2;
const UITableViewCellStyleSubtitle: NSInteger = 3;
const UITableViewCellSelectionStyleBlue: NSInteger = 1;
const UITableViewCellSeparatorStyleSingleLine: NSInteger = 1;
const UITableViewScrollPositionTop: NSInteger = 1;
const UITableViewScrollPositionMiddle: NSInteger = 2;
const UITableViewScrollPositionBottom: NSInteger = 3;
const NSNotFound: NSUInteger = 0x7FFF_FFFF;

/// iOS 默认行高。
const DEFAULT_ROW_HEIGHT: CGFloat = 44.0;
/// 手指移动达到这个距离(点)就算拖动,不再当作点选。
/// [复核修 2026-09-15] R2-1 返修:与 UIScrollView 起拖阈值(`hypot >= 10` 起拖)对齐,原注释为"超过"。
const TAP_SLOP: CGFloat = 10.0;
/// 每个复用标识最多缓存的 cell 数。
const MAX_REUSE_PER_IDENTIFIER: usize = 32;
/// 行数上限,防止 dataSource 返回异常大的数把宿主拖死。
const MAX_ROWS: usize = 20_000;

// ---------------------------------------------------------------------------
// _touchHLE_NSIndexPath
// ---------------------------------------------------------------------------

struct TableIndexPathHostObject {
    section: NSInteger,
    row: NSInteger,
}
impl HostObject for TableIndexPathHostObject {}

/// 新建索引路径(+1 引用)。
fn new_index_path_retained(env: &mut Environment, section: NSInteger, row: NSInteger) -> id {
    let class = env
        .objc
        .get_known_class("_touchHLE_NSIndexPath", &mut env.mem);
    let index_path: id = msg![env; class alloc];
    let host = env.objc.borrow_mut::<TableIndexPathHostObject>(index_path);
    host.section = section;
    host.row = row;
    index_path
}

/// 新建索引路径(autoreleased)。
fn new_index_path(env: &mut Environment, section: NSInteger, row: NSInteger) -> id {
    let index_path = new_index_path_retained(env, section, row);
    autorelease(env, index_path)
}

/// 取索引路径的 (section, row);不是本文件的索引路径对象时返回 None。
fn index_path_parts(env: &Environment, index_path: id) -> Option<(NSInteger, NSInteger)> {
    if index_path == nil {
        return None;
    }
    env.objc
        .get_host_object(index_path)
        .and_then(|obj| obj.as_any().downcast_ref::<TableIndexPathHostObject>())
        .map(|p| (p.section, p.row))
}

// ---------------------------------------------------------------------------
// 宿主对象
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct RowGeometry {
    section: NSInteger,
    row: NSInteger,
    y: CGFloat,
    height: CGFloat,
}

pub struct UITableViewHostObject {
    superclass: super::ui_scroll_view::UIScrollViewHostObject,
    style: NSInteger,
    /// UITableViewDataSource,弱引用(UIKit 语义)。delegate 复用 UIScrollView 的字段。
    data_source: id,
    row_height: CGFloat,
    separator_style: NSInteger,
    allows_selection: bool,
    /// 表头/表尾视图:强引用,同时是子视图。
    header_view: id,
    footer_view: id,
    section_count: NSInteger,
    /// 行几何,按 section/row 顺序平铺(y 为内容坐标)。
    rows: Vec<RowGeometry>,
    /// 已实体化的可见行:(rows 下标, cell),cell 强引用,按下标升序。
    visible: Vec<(usize, id)>,
    /// 复用池:reuseIdentifier → cell(强引用)。
    reuse_pool: HashMap<String, Vec<id>>,
    selected: Option<(NSInteger, NSInteger)>,
    /// 数据源变了但还没 reload(初始化 / setDataSource: / 布局期间的 reloadData)。
    needs_reload: bool,
    /// beginUpdates / endUpdates 嵌套深度。
    update_depth: u32,
    /// 本次触摸起点(窗口坐标)与是否已拖动。
    touch_start: Option<CGPoint>,
    touch_dragged: bool,
    /// layoutSubviews 重入保护(布局期间会调 guest 的 cellForRowAtIndexPath:)。
    in_layout: bool,
}
impl_HostObject_with_superclass!(UITableViewHostObject);
impl Default for UITableViewHostObject {
    fn default() -> Self {
        UITableViewHostObject {
            // [复核修 2026-09-15] R2-1:表格不沿响应链转发触摸(见文件头)。
            superclass: super::ui_scroll_view::UIScrollViewHostObject::without_touch_forwarding(),
            style: 0,
            data_source: nil,
            row_height: DEFAULT_ROW_HEIGHT,
            separator_style: UITableViewCellSeparatorStyleSingleLine,
            allows_selection: true,
            header_view: nil,
            footer_view: nil,
            section_count: 0,
            rows: Vec::new(),
            visible: Vec::new(),
            reuse_pool: HashMap::new(),
            selected: None,
            needs_reload: true,
            update_depth: 0,
            touch_start: None,
            touch_dragged: false,
            in_layout: false,
        }
    }
}

pub struct UITableViewCellHostObject {
    superclass: super::UIViewHostObject,
    style: NSInteger,
    /// `NSString*`,强引用(copy)。
    reuse_identifier: id,
    /// 以下子视图都是强引用(同时挂在视图树上)。
    content_view: id,
    text_label: id,
    detail_text_label: id,
    image_view: id,
    accessory_view: id,
    background_view: id,
    selected_background_view: id,
    accessory_type: NSInteger,
    selection_style: NSInteger,
    selected: bool,
    highlighted: bool,
}
impl_HostObject_with_superclass!(UITableViewCellHostObject);
impl Default for UITableViewCellHostObject {
    fn default() -> Self {
        UITableViewCellHostObject {
            superclass: Default::default(),
            style: UITableViewCellStyleDefault,
            reuse_identifier: nil,
            content_view: nil,
            text_label: nil,
            detail_text_label: nil,
            image_view: nil,
            accessory_view: nil,
            background_view: nil,
            selected_background_view: nil,
            accessory_type: 0,
            selection_style: UITableViewCellSelectionStyleBlue,
            selected: false,
            highlighted: false,
        }
    }
}

#[derive(Default)]
struct UITableViewControllerHostObject {
    superclass: UIViewControllerHostObject,
    style: NSInteger,
    clears_selection_on_view_will_appear: bool,
}
impl_HostObject_with_superclass!(UITableViewControllerHostObject);

// ---------------------------------------------------------------------------
// 通用小工具
// ---------------------------------------------------------------------------

fn rect(x: CGFloat, y: CGFloat, width: CGFloat, height: CGFloat) -> CGRect {
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

/// 对象是否响应某选择子;选择子从未注册过(没有任何类实现)时直接 false。
fn responds(env: &mut Environment, object: id, selector_name: &str) -> bool {
    if object == nil {
        return false;
    }
    let Some(sel) = env.objc.lookup_selector(selector_name) else {
        return false;
    };
    msg![env; object respondsToSelector:sel]
}

fn is_kind_of_host_class(env: &mut Environment, object: id, class_name: &str) -> bool {
    if object == nil {
        return false;
    }
    let class: Class = msg![env; object class];
    let target = env.objc.get_known_class(class_name, &mut env.mem);
    env.objc.class_is_subclass_of(class, target)
}

/// UIScrollView 自己(而非 UIView/UIResponder)是否实现了某个触摸方法。
/// 用运行时判断而不是写死:W3 可能给 UIScrollView 补 touchesBegan:/touchesEnded:
/// (比如翻页、惯性),有就交给它;没有就不调 super,免得 UIResponder 默认实现把
/// 触摸沿响应链转发给下面的 EAGLView(点列表穿透到游戏场景)。
/// [复核修 2026-09-15] R2-1:UIScrollView 现在四个方法都实现了,这里恒为真;它内部的
/// 转发由宿主对象的 forwards_touches 关掉(见 UITableViewHostObject::default)。
fn scroll_view_implements(env: &mut Environment, selector_name: &str) -> bool {
    let Some(sel) = env.objc.lookup_selector(selector_name) else {
        return false;
    };
    let scroll_view_class = env.objc.get_known_class("UIScrollView", &mut env.mem);
    let view_class = env.objc.get_known_class("UIView", &mut env.mem);
    env.objc
        .class_overrides_method_of_superclass(scroll_view_class, sel, view_class)
}

fn set_frame_if_some(env: &mut Environment, view: id, frame: CGRect) {
    if view != nil {
        () = msg![env; view setFrame:frame];
    }
}

/// iOS 的 viewWithTag: 是深度优先递归搜索(含自身)。touchHLE 的 UIView 版本只看
/// 直接子视图,而游戏把控件加在 contentView 里、再对 cell 调 viewWithTag:
/// (SeekViewController configureCell:forIndexPath:@0x1a3108 等),所以 cell 上覆盖成递归。
/// 纯宿主侧遍历,不发消息。
fn find_view_with_tag(env: &Environment, view: id, tag: NSInteger, depth: u32) -> id {
    const MAX_DEPTH: u32 = 64;
    let host = env.objc.borrow::<super::UIViewHostObject>(view);
    if host.tag == tag {
        return view;
    }
    if depth >= MAX_DEPTH {
        return nil;
    }
    for &subview in &host.subviews {
        let found = find_view_with_tag(env, subview, tag, depth + 1);
        if found != nil {
            return found;
        }
    }
    nil
}

// ---------------------------------------------------------------------------
// UITableView:几何、实体化、复用、选中
// ---------------------------------------------------------------------------

fn row_frame(row: RowGeometry, width: CGFloat) -> CGRect {
    rect(0.0, row.y, width, row.height)
}

/// cell 离开可见区:摘下视图树,有复用标识就进复用池,否则 autorelease。
/// 用 autorelease 而不是 release:reloadData 可能由 cell 里按钮的触摸回调触发
/// (MessageViewController deleteBtnTouched:@0x1a8e4a),当场释放会让仍在分发中的按钮悬空;
/// 交给当前自动释放池,等本次事件处理结束再释放。
fn recycle_cell(env: &mut Environment, table: id, cell: id) {
    () = msg![env; cell removeFromSuperview];
    if is_kind_of_host_class(env, cell, "UITableViewCell") {
        let identifier = env.objc.borrow::<UITableViewCellHostObject>(cell).reuse_identifier;
        if identifier != nil {
            let key = to_rust_string(env, identifier).to_string();
            let pool = &mut env.objc.borrow_mut::<UITableViewHostObject>(table).reuse_pool;
            let list = pool.entry(key).or_default();
            if list.len() < MAX_REUSE_PER_IDENTIFIER {
                list.push(cell); // 转移本表持有的引用
                return;
            }
        }
    }
    autorelease(env, cell);
}

fn recycle_all_visible(env: &mut Environment, table: id) {
    let visible = std::mem::take(&mut env.objc.borrow_mut::<UITableViewHostObject>(table).visible);
    for (_, cell) in visible {
        recycle_cell(env, table, cell);
    }
}

/// 向 dataSource/delegate 询问分区数、行数、行高,重建行几何并更新 contentSize。
fn rebuild_geometry(env: &mut Environment, table: id) {
    let (data_source, row_height, header, footer) = {
        let host = env.objc.borrow::<UITableViewHostObject>(table);
        (host.data_source, host.row_height, host.header_view, host.footer_view)
    };
    let delegate: id = msg![env; table delegate];
    let bounds: CGRect = msg![env; table bounds];
    let width = bounds.size.width;

    let mut y: CGFloat = 0.0;
    if header != nil {
        let frame: CGRect = msg![env; header frame];
        let header_height = frame.size.height;
        () = msg![env; header setFrame:(rect(0.0, 0.0, width, header_height))];
        y += header_height;
    }

    let mut rows: Vec<RowGeometry> = Vec::new();
    let mut section_count: NSInteger = 0;
    if data_source != nil {
        section_count = if responds(env, data_source, "numberOfSectionsInTableView:") {
            msg![env; data_source numberOfSectionsInTableView:table]
        } else {
            1
        };
        section_count = section_count.max(0);
        let has_rows = responds(env, data_source, "tableView:numberOfRowsInSection:");
        let has_height = responds(env, delegate, "tableView:heightForRowAtIndexPath:");
        'sections: for section in 0..section_count {
            let count: NSInteger = if has_rows {
                msg![env; data_source tableView:table numberOfRowsInSection:section]
            } else {
                0
            };
            for row in 0..count.max(0) {
                if rows.len() >= MAX_ROWS {
                    log!("Warning: UITableView {:?} has more than {} rows, truncating", table, MAX_ROWS);
                    break 'sections;
                }
                let height = if has_height {
                    let index_path = new_index_path_retained(env, section, row);
                    let h: CGFloat = msg![env; delegate tableView:table heightForRowAtIndexPath:index_path];
                    release(env, index_path);
                    if h.is_finite() && h >= 0.0 { h } else { row_height }
                } else {
                    row_height
                };
                rows.push(RowGeometry { section, row, y, height });
                y += height;
            }
        }
    }

    if footer != nil {
        let frame: CGRect = msg![env; footer frame];
        let footer_height = frame.size.height;
        () = msg![env; footer setFrame:(rect(0.0, y, width, footer_height))];
        y += footer_height;
    }

    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(table);
        host.rows = rows;
        host.section_count = section_count;
    }
    let content_size = CGSize { width, height: y };
    () = msg![env; table setContentSize:content_size];

    // 删行后内容变短时把滚动位置夹回有效范围,避免停在空白处。
    let offset: CGPoint = msg![env; table contentOffset];
    let max_y = (y - bounds.size.height).max(0.0);
    if offset.y > max_y {
        let clamped = CGPoint { x: offset.x, y: max_y };
        () = msg![env; table setContentOffset:clamped];
    }
}

/// reloadData 的主体:回收全部可见 cell、清选中、重建几何;cell 留到下一次布局再要。
fn reload_data(env: &mut Environment, table: id, schedule_layout: bool) {
    recycle_all_visible(env, table);
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(table);
        host.selected = None;
        host.needs_reload = false;
    }
    rebuild_geometry(env, table);
    if schedule_layout {
        () = msg![env; table setNeedsLayout];
    }
}

/// 只为可见区里的行实体化 cell,并回收离开可见区的 cell。仅在 layoutSubviews 里调用。
fn tile_visible_rows(env: &mut Environment, table: id) {
    let bounds: CGRect = msg![env; table bounds];
    let top = bounds.origin.y;
    let bottom = top + bounds.size.height;
    let width = bounds.size.width;

    let (data_source, selected, wanted) = {
        let host = env.objc.borrow::<UITableViewHostObject>(table);
        let wanted: Vec<usize> = host
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.height > 0.0 && r.y < bottom && r.y + r.height > top)
            .map(|(i, _)| i)
            .collect();
        (host.data_source, host.selected, wanted)
    };

    // 回收离开可见区的 cell;留下的 cell 按当前宽度校正 frame。
    let old_visible = std::mem::take(&mut env.objc.borrow_mut::<UITableViewHostObject>(table).visible);
    let mut kept: Vec<(usize, id)> = Vec::with_capacity(wanted.len());
    for (index, cell) in old_visible {
        if wanted.binary_search(&index).is_ok() {
            kept.push((index, cell));
        } else {
            recycle_cell(env, table, cell);
        }
    }
    for &(index, cell) in &kept {
        let Some(row) = env.objc.borrow::<UITableViewHostObject>(table).rows.get(index).copied() else {
            continue;
        };
        let frame = row_frame(row, width);
        let old: CGRect = msg![env; cell frame];
        if old.size.width != frame.size.width
            || old.size.height != frame.size.height
            || old.origin.y != frame.origin.y
        {
            () = msg![env; cell setFrame:frame];
            if is_kind_of_host_class(env, cell, "UITableViewCell") {
                layout_cell(env, cell);
            }
        }
    }

    // msg! 对未注册的选择子会 expect panic("Unknown selector"),dataSource 没实现这个必需方法时
    // 选择子可能从未注册,所以先用 responds 判一次。
    let has_cell_for_row = responds(env, data_source, "tableView:cellForRowAtIndexPath:");
    if data_source != nil && !has_cell_for_row && !wanted.is_empty() {
        log!(
            "Warning: UITableView {:?} dataSource {:?} does not implement tableView:cellForRowAtIndexPath:",
            table, data_source
        );
    }
    if has_cell_for_row {
        let delegate: id = msg![env; table delegate];
        let will_display = responds(env, delegate, "tableView:willDisplayCell:forRowAtIndexPath:");
        let table_layer: id = msg![env; table layer];
        let table_background: CGColorRef = msg![env; table_layer backgroundColor];
        for &index in &wanted {
            if kept.iter().any(|&(i, _)| i == index) {
                continue;
            }
            let Some(row) = env.objc.borrow::<UITableViewHostObject>(table).rows.get(index).copied() else {
                continue;
            };
            let index_path = new_index_path_retained(env, row.section, row.row);
            let cell: id = msg![env; data_source tableView:table cellForRowAtIndexPath:index_path];
            if cell == nil {
                // iOS 在这里抛异常;宽容处理,跳过这一行。
                log!(
                    "Warning: {:?} tableView:cellForRowAtIndexPath: returned nil for section {} row {}",
                    data_source, row.section, row.row
                );
                release(env, index_path);
                continue;
            }
            retain(env, cell);
            () = msg![env; cell setFrame:(row_frame(row, width))];
            let is_cell = is_kind_of_host_class(env, cell, "UITableViewCell");
            if table_background != nil {
                let color: id = msg_class![env; UIColor colorWithCGColor:table_background];
                () = msg![env; cell setBackgroundColor:color];
            }
            if is_cell {
                layout_cell(env, cell);
            }
            () = msg![env; table addSubview:cell];
            if is_cell && selected == Some((row.section, row.row)) {
                () = msg![env; cell setSelected:true animated:false];
            }
            if will_display {
                () = msg![env; delegate tableView:table willDisplayCell:cell forRowAtIndexPath:index_path];
            }
            release(env, index_path);
            kept.push((index, cell));
        }
    }

    kept.sort_by_key(|&(i, _)| i);
    env.objc.borrow_mut::<UITableViewHostObject>(table).visible = kept;
}

fn visible_cell_for(env: &Environment, table: id, section: NSInteger, row: NSInteger) -> id {
    let host = env.objc.borrow::<UITableViewHostObject>(table);
    host.visible
        .iter()
        .find(|&&(i, _)| {
            host.rows
                .get(i)
                .map_or(false, |r| r.section == section && r.row == row)
        })
        .map_or(nil, |&(_, cell)| cell)
}

fn set_cell_selected(env: &mut Environment, cell: id, selected: bool, animated: bool) {
    if cell != nil && is_kind_of_host_class(env, cell, "UITableViewCell") {
        () = msg![env; cell setSelected:selected animated:animated];
    }
}

/// 切换选中行(None = 取消选中),同步可见 cell 的 selected 状态。
fn select_row(env: &mut Environment, table: id, target: Option<(NSInteger, NSInteger)>, animated: bool) {
    let previous = env.objc.borrow::<UITableViewHostObject>(table).selected;
    if previous == target {
        return;
    }
    if let Some((section, row)) = previous {
        let cell = visible_cell_for(env, table, section, row);
        set_cell_selected(env, cell, false, animated);
    }
    env.objc.borrow_mut::<UITableViewHostObject>(table).selected = target;
    if let Some((section, row)) = target {
        let cell = visible_cell_for(env, table, section, row);
        set_cell_selected(env, cell, true, animated);
    }
}

fn row_at_content_y(env: &Environment, table: id, y: CGFloat) -> Option<(NSInteger, NSInteger)> {
    env.objc
        .borrow::<UITableViewHostObject>(table)
        .rows
        .iter()
        .find(|r| y >= r.y && y < r.y + r.height)
        .map(|r| (r.section, r.row))
}

/// 点按选中:willSelect(可改目标或返回 nil 取消)→ 选中 → didSelect。
fn handle_tap(env: &mut Environment, table: id, point_in_table: CGPoint) {
    let y = point_in_table.y;
    let Some((mut section, mut row)) = row_at_content_y(env, table, y) else {
        return;
    };
    let delegate: id = msg![env; table delegate];
    // 回调里可能把表格从视图树摘掉并释放(-[TMAUserIDListView tableView:didSelectRowAtIndexPath:]
    // 选中后 removeFromSuperview@0x4e3b28),先 retain,最后 autorelease,保证本次触摸分发
    // 结束前表格仍存活。
    retain(env, table);
    if responds(env, delegate, "tableView:willSelectRowAtIndexPath:") {
        let index_path = new_index_path_retained(env, section, row);
        let result: id = msg![env; delegate tableView:table willSelectRowAtIndexPath:index_path];
        let redirected = index_path_parts(env, result);
        release(env, index_path);
        if result == nil {
            autorelease(env, table);
            return;
        }
        if let Some((s, r)) = redirected {
            section = s;
            row = r;
        }
    }
    select_row(env, table, Some((section, row)), false);
    if responds(env, delegate, "tableView:didSelectRowAtIndexPath:") {
        let index_path = new_index_path_retained(env, section, row);
        () = msg![env; delegate tableView:table didSelectRowAtIndexPath:index_path];
        release(env, index_path);
    }
    autorelease(env, table);
}

// ---------------------------------------------------------------------------
// UITableViewCell:子视图懒创建与布局
// ---------------------------------------------------------------------------

fn ensure_content_view(env: &mut Environment, cell: id) -> id {
    let existing = env.objc.borrow::<UITableViewCellHostObject>(cell).content_view;
    if existing != nil {
        return existing;
    }
    let bounds: CGRect = msg![env; cell bounds];
    let content_frame = rect(0.0, 0.0, bounds.size.width, bounds.size.height);
    let view: id = msg_class![env; UIView alloc];
    let view: id = msg![env; view initWithFrame:content_frame];
    // 透明、不透明度 NO:让 cell/表格底色透出来,也避免合成器对"不透明无背景"层关混合出黑块。
    () = msg![env; view setOpaque:false];
    env.objc.borrow_mut::<UITableViewCellHostObject>(cell).content_view = view; // 持有 alloc 的 +1
    () = msg![env; cell addSubview:view];
    view
}

fn make_color(env: &mut Environment, r: CGFloat, g: CGFloat, b: CGFloat) -> id {
    msg_class![env; UIColor colorWithRed:r green:g blue:b alpha:(1.0 as CGFloat)]
}

/// textLabel(detail = false)/ detailTextLabel(detail = true)懒创建。
/// iOS:Default 样式的 detailTextLabel 恒为 nil。字体/颜色取 iOS 6 各样式的默认值。
fn ensure_label(env: &mut Environment, cell: id, detail: bool) -> id {
    let (existing, style) = {
        let host = env.objc.borrow::<UITableViewCellHostObject>(cell);
        (if detail { host.detail_text_label } else { host.text_label }, host.style)
    };
    if existing != nil {
        return existing;
    }
    if detail && style == UITableViewCellStyleDefault {
        return nil;
    }
    let content_view = ensure_content_view(env, cell);
    let label: id = msg_class![env; UILabel alloc];
    let label: id = msg![env; label initWithFrame:(rect(0.0, 0.0, 0.0, 0.0))];
    let clear: id = msg_class![env; UIColor clearColor];
    () = msg![env; label setBackgroundColor:clear];
    let (bold, size, color, right_aligned): (bool, CGFloat, (CGFloat, CGFloat, CGFloat), bool) =
        match (detail, style) {
            (false, UITableViewCellStyleDefault) => (true, 20.0, (0.0, 0.0, 0.0), false),
            (false, UITableViewCellStyleSubtitle) => (true, 18.0, (0.0, 0.0, 0.0), false),
            (false, UITableViewCellStyleValue2) => (true, 12.0, (0.32, 0.4, 0.57), true),
            (false, _) => (true, 17.0, (0.0, 0.0, 0.0), false),
            (true, UITableViewCellStyleValue1) => (false, 17.0, (0.22, 0.33, 0.53), true),
            (true, UITableViewCellStyleValue2) => (true, 15.0, (0.0, 0.0, 0.0), false),
            (true, _) => (false, 14.0, (0.5, 0.5, 0.5), false),
        };
    let font: id = if bold {
        msg_class![env; UIFont boldSystemFontOfSize:size]
    } else {
        msg_class![env; UIFont systemFontOfSize:size]
    };
    () = msg![env; label setFont:font];
    let text_color = make_color(env, color.0, color.1, color.2);
    () = msg![env; label setTextColor:text_color];
    if right_aligned {
        () = msg![env; label setTextAlignment:(2 as NSInteger)];
    }
    {
        let host = env.objc.borrow_mut::<UITableViewCellHostObject>(cell);
        if detail {
            host.detail_text_label = label; // 持有 alloc 的 +1
        } else {
            host.text_label = label;
        }
    }
    () = msg![env; content_view addSubview:label];
    () = msg![env; cell setNeedsLayout];
    label
}

fn ensure_image_view(env: &mut Environment, cell: id) -> id {
    let existing = env.objc.borrow::<UITableViewCellHostObject>(cell).image_view;
    if existing != nil {
        return existing;
    }
    let content_view = ensure_content_view(env, cell);
    let image_view: id = msg_class![env; UIImageView alloc];
    let image_view: id = msg![env; image_view initWithFrame:(rect(0.0, 0.0, 0.0, 0.0))];
    env.objc.borrow_mut::<UITableViewCellHostObject>(cell).image_view = image_view; // 持有 +1
    () = msg![env; content_view addSubview:image_view];
    () = msg![env; cell setNeedsLayout];
    image_view
}

/// 按 iOS 6 默认样式摆放 contentView / imageView / textLabel / detailTextLabel / accessoryView。
/// touchHLE 没有 autoresizing,所以 cell 尺寸变化后必须显式调用(表格实体化时调、
/// cell 自己 layoutSubviews 时也调)。
fn layout_cell(env: &mut Environment, cell: id) {
    let bounds: CGRect = msg![env; cell bounds];
    let width = bounds.size.width;
    let height = bounds.size.height;
    let (style, content_view, text_label, detail_label, image_view, accessory_view, accessory_type, background_view, selected_background_view) = {
        let h = env.objc.borrow::<UITableViewCellHostObject>(cell);
        (
            h.style,
            h.content_view,
            h.text_label,
            h.detail_text_label,
            h.image_view,
            h.accessory_view,
            h.accessory_type,
            h.background_view,
            h.selected_background_view,
        )
    };
    let full = rect(0.0, 0.0, width, height);
    set_frame_if_some(env, background_view, full);
    set_frame_if_some(env, selected_background_view, full);

    let mut content_width = width;
    if accessory_view != nil {
        let frame: CGRect = msg![env; accessory_view frame];
        let accessory_width = frame.size.width;
        let accessory_height = frame.size.height;
        let accessory_frame = rect(
            width - accessory_width - 10.0,
            ((height - accessory_height) / 2.0).floor(),
            accessory_width,
            accessory_height,
        );
        () = msg![env; accessory_view setFrame:accessory_frame];
        content_width = (width - accessory_width - 20.0).max(0.0);
    } else if accessory_type != 0 {
        // 系统附件(箭头/对勾)不绘制,只按 iOS 预留宽度。
        content_width = (width - 30.0).max(0.0);
    }
    set_frame_if_some(env, content_view, rect(0.0, 0.0, content_width, height));

    let mut x: CGFloat = 10.0;
    if image_view != nil {
        let image: id = msg![env; image_view image];
        if image != nil {
            let size: CGSize = msg![env; image size];
            let image_width = size.width;
            let image_height = size.height;
            let image_frame = rect(10.0, ((height - image_height) / 2.0).floor(), image_width, image_height);
            () = msg![env; image_view setFrame:image_frame];
            x = 10.0 + image_width + 10.0;
        }
    }
    let text_width = (content_width - x - 10.0).max(0.0);
    match style {
        UITableViewCellStyleSubtitle => {
            set_frame_if_some(env, text_label, rect(x, (height * 0.08).floor(), text_width, (height * 0.5).floor()));
            set_frame_if_some(env, detail_label, rect(x, (height * 0.55).floor(), text_width, (height * 0.38).floor()));
        }
        UITableViewCellStyleValue1 => {
            set_frame_if_some(env, text_label, rect(x, 0.0, text_width * 0.6, height));
            set_frame_if_some(env, detail_label, rect(x + text_width * 0.4, 0.0, text_width * 0.6, height));
        }
        UITableViewCellStyleValue2 => {
            set_frame_if_some(env, text_label, rect(x, 0.0, text_width * 0.3, height));
            set_frame_if_some(env, detail_label, rect(x + text_width * 0.3 + 6.0, 0.0, (text_width * 0.7 - 6.0).max(0.0), height));
        }
        _ => {
            set_frame_if_some(env, text_label, rect(x, 0.0, text_width, height));
        }
    }
}

/// 设置表头(header = true)或表尾视图:替换旧视图、挂到表格上、下次布局重排。
fn set_header_or_footer(env: &mut Environment, table: id, view: id, header: bool) {
    let old = {
        let host = env.objc.borrow::<UITableViewHostObject>(table);
        if header { host.header_view } else { host.footer_view }
    };
    if old == view {
        return;
    }
    if old != nil {
        () = msg![env; old removeFromSuperview];
        release(env, old);
    }
    retain(env, view);
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(table);
        if header {
            host.header_view = view;
        } else {
            host.footer_view = view;
        }
        host.needs_reload = true;
    }
    if view != nil {
        () = msg![env; table addSubview:view];
    }
    () = msg![env; table setNeedsLayout];
}

fn find_row(env: &Environment, table: id, section: NSInteger, row: NSInteger) -> Option<RowGeometry> {
    env.objc
        .borrow::<UITableViewHostObject>(table)
        .rows
        .iter()
        .find(|r| r.section == section && r.row == row)
        .copied()
}

// ---------------------------------------------------------------------------
// 类定义
// ---------------------------------------------------------------------------

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// [扫描修 2026-09-15] 表格交给游戏的索引路径(见文件头 NSIndexPath 说明)。
@implementation _touchHLE_NSIndexPath: NSIndexPath

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(TableIndexPathHostObject { section: 0, row: 0 });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (NSInteger)section {
    env.objc.borrow::<TableIndexPathHostObject>(this).section
}
- (NSInteger)row {
    env.objc.borrow::<TableIndexPathHostObject>(this).row
}
- (NSInteger)item {
    env.objc.borrow::<TableIndexPathHostObject>(this).row
}
- (NSUInteger)length {
    2
}
- (NSUInteger)indexAtPosition:(NSUInteger)position {
    let host = env.objc.borrow::<TableIndexPathHostObject>(this);
    match position {
        0 => host.section as NSUInteger,
        1 => host.row as NSUInteger,
        _ => NSNotFound,
    }
}
// 索引路径不可变,copy 直接 retain。
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}
- (bool)isEqual:(id)other {
    if other == this {
        return true;
    }
    let mine = index_path_parts(env, this);
    mine.is_some() && index_path_parts(env, other) == mine
}
- (NSUInteger)hash {
    let host = env.objc.borrow::<TableIndexPathHostObject>(this);
    (host.section as NSUInteger).wrapping_shl(20) ^ (host.row as NSUInteger)
}
// NSComparisonResult:-1 升序 / 0 相等 / 1 降序。
- (NSInteger)compare:(id)other {
    let mine = index_path_parts(env, this).unwrap_or((0, 0));
    let Some(theirs) = index_path_parts(env, other) else {
        return 1;
    };
    match mine.cmp(&theirs) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

@end

// [扫描修 2026-09-15] F8-1 / F9-7 / F11-8:最小 UITableView(见文件头)。
@implementation UITableView: UIScrollView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame
              style:(NSInteger)style {
    let this: id = msg_super![env; this initWithFrame:frame];
    if this == nil {
        return nil;
    }
    env.objc.borrow_mut::<UITableViewHostObject>(this).style = style;
    // iOS ≤6 表格默认白底;游戏的列表都会在之后显式设背景色(TMAUserIDListView 设 clearColor,
    // ManagerViewController initMessageView: 等设自定义色)。
    let white: id = msg_class![env; UIColor whiteColor];
    () = msg![env; this setBackgroundColor:white];
    () = msg![env; this setNeedsLayout];
    this
}
- (id)initWithFrame:(CGRect)frame {
    msg![env; this initWithFrame:frame style:(0 as NSInteger)]
}

- (())dealloc {
    let (visible, pool, header, footer) = {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        (
            std::mem::take(&mut host.visible),
            std::mem::take(&mut host.reuse_pool),
            std::mem::replace(&mut host.header_view, nil),
            std::mem::replace(&mut host.footer_view, nil),
        )
    };
    // 可见 cell / 表头表尾同时是子视图,这里只释放本表额外持有的那一份,
    // 子视图那一份由 UIView dealloc 释放。
    for (_, cell) in visible {
        release(env, cell);
    }
    for (_, cells) in pool {
        for cell in cells {
            release(env, cell);
        }
    }
    release(env, header);
    release(env, footer);
    msg_super![env; this dealloc]
}

- (NSInteger)style {
    env.objc.borrow::<UITableViewHostObject>(this).style
}

- (id)dataSource {
    env.objc.borrow::<UITableViewHostObject>(this).data_source
}
- (())setDataSource:(id)data_source { // 弱引用
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        host.data_source = data_source;
        host.needs_reload = true;
    }
    () = msg![env; this setNeedsLayout];
}

- (CGFloat)rowHeight {
    env.objc.borrow::<UITableViewHostObject>(this).row_height
}
- (())setRowHeight:(CGFloat)height {
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        host.row_height = height;
        host.needs_reload = true;
    }
    () = msg![env; this setNeedsLayout];
}

// 分隔线不绘制,只记录样式。
- (NSInteger)separatorStyle {
    env.objc.borrow::<UITableViewHostObject>(this).separator_style
}
- (())setSeparatorStyle:(NSInteger)style {
    env.objc.borrow_mut::<UITableViewHostObject>(this).separator_style = style;
}
- (id)separatorColor {
    nil
}
- (())setSeparatorColor:(id)_color {
    log_dbg!("[(UITableView*){:?} setSeparatorColor:] ignored (separators are not drawn)", this);
}

- (bool)allowsSelection {
    env.objc.borrow::<UITableViewHostObject>(this).allows_selection
}
- (())setAllowsSelection:(bool)allows {
    env.objc.borrow_mut::<UITableViewHostObject>(this).allows_selection = allows;
}
- (())setAllowsSelectionDuringEditing:(bool)_allows {
}
- (bool)isEditing {
    false
}
- (())setEditing:(bool)_editing {
}
- (())setEditing:(bool)_editing
        animated:(bool)_animated {
}
- (())setSectionHeaderHeight:(CGFloat)_height {
}
- (())setSectionFooterHeight:(CGFloat)_height {
}
- (id)backgroundView {
    nil
}
- (())setBackgroundView:(id)_view {
    log_dbg!("[(UITableView*){:?} setBackgroundView:] ignored", this);
}

- (id)tableHeaderView {
    env.objc.borrow::<UITableViewHostObject>(this).header_view
}
- (())setTableHeaderView:(id)view {
    set_header_or_footer(env, this, view, true);
}
- (id)tableFooterView {
    env.objc.borrow::<UITableViewHostObject>(this).footer_view
}
- (())setTableFooterView:(id)view {
    set_header_or_footer(env, this, view, false);
}

// iOS:同步重算行数/行高与 contentSize,cell 在下一次布局时按可见行重新向 dataSource 要。
- (())reloadData {
    let (in_layout, updating) = {
        let host = env.objc.borrow::<UITableViewHostObject>(this);
        (host.in_layout, host.update_depth > 0)
    };
    if in_layout || updating {
        // 布局期间(guest 的 cellForRowAtIndexPath: 里)或 beginUpdates 之内:推迟。
        env.objc.borrow_mut::<UITableViewHostObject>(this).needs_reload = true;
        if in_layout {
            () = msg![env; this setNeedsLayout];
        }
        return;
    }
    reload_data(env, this, true);
}
- (())beginUpdates {
    env.objc.borrow_mut::<UITableViewHostObject>(this).update_depth += 1;
}
- (())endUpdates {
    let depth = {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        host.update_depth = host.update_depth.saturating_sub(1);
        host.update_depth
    };
    if depth == 0 {
        () = msg![env; this reloadData];
    }
}
// 行/分区增删改一律退化为整表 reload(不做行动画)。
// FriendsViewController tableView:commitEditingStyle:forRowAtIndexPath:@0x19c78c、
// TMAUserIDListView tableView:accessoryButtonTappedForRowWithIndexPath:@0x4e2cb4 用到。
- (())insertRowsAtIndexPaths:(id)_index_paths
            withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}
- (())deleteRowsAtIndexPaths:(id)_index_paths
            withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}
- (())reloadRowsAtIndexPaths:(id)_index_paths
            withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}
- (())insertSections:(id)_sections
    withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}
- (())deleteSections:(id)_sections
    withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}
- (())reloadSections:(id)_sections
    withRowAnimation:(NSInteger)_animation {
    () = msg![env; this reloadData];
}

- (NSInteger)numberOfSections {
    env.objc.borrow::<UITableViewHostObject>(this).section_count
}
- (NSInteger)numberOfRowsInSection:(NSInteger)section {
    env.objc
        .borrow::<UITableViewHostObject>(this)
        .rows
        .iter()
        .filter(|r| r.section == section)
        .count() as NSInteger
}

- (id)dequeueReusableCellWithIdentifier:(id)identifier {
    if identifier == nil {
        return nil;
    }
    let key = to_rust_string(env, identifier).to_string();
    let cell = env
        .objc
        .borrow_mut::<UITableViewHostObject>(this)
        .reuse_pool
        .get_mut(&key)
        .and_then(|list| list.pop());
    let Some(cell) = cell else {
        return nil;
    };
    () = msg![env; cell prepareForReuse];
    autorelease(env, cell) // 池里持有的那份转成 autoreleased 返回
}

- (id)cellForRowAtIndexPath:(id)index_path {
    let Some((section, row)) = index_path_parts(env, index_path) else {
        return nil;
    };
    visible_cell_for(env, this, section, row)
}
- (id)indexPathForCell:(id)cell {
    let found = {
        let host = env.objc.borrow::<UITableViewHostObject>(this);
        host.visible
            .iter()
            .find(|&&(_, c)| c == cell)
            .and_then(|&(i, _)| host.rows.get(i))
            .map(|r| (r.section, r.row))
    };
    match found {
        Some((section, row)) => new_index_path(env, section, row),
        None => nil,
    }
}
- (id)indexPathForRowAtPoint:(CGPoint)point {
    let y = point.y;
    let found = row_at_content_y(env, this, y);
    match found {
        Some((section, row)) => new_index_path(env, section, row),
        None => nil,
    }
}
- (id)visibleCells {
    let cells: Vec<id> = env
        .objc
        .borrow::<UITableViewHostObject>(this)
        .visible
        .iter()
        .map(|&(_, cell)| cell)
        .collect();
    for &cell in &cells {
        retain(env, cell);
    }
    let array = ns_array::from_vec(env, cells);
    autorelease(env, array)
}
- (id)indexPathsForVisibleRows {
    let parts: Vec<(NSInteger, NSInteger)> = {
        let host = env.objc.borrow::<UITableViewHostObject>(this);
        host.visible
            .iter()
            .filter_map(|&(i, _)| host.rows.get(i))
            .map(|r| (r.section, r.row))
            .collect()
    };
    let mut paths: Vec<id> = Vec::with_capacity(parts.len());
    for (section, row) in parts {
        paths.push(new_index_path_retained(env, section, row));
    }
    let array = ns_array::from_vec(env, paths);
    autorelease(env, array)
}
- (CGRect)rectForRowAtIndexPath:(id)index_path {
    let bounds: CGRect = msg![env; this bounds];
    let width = bounds.size.width;
    let geometry = match index_path_parts(env, index_path) {
        Some((section, row)) => find_row(env, this, section, row),
        None => None,
    };
    match geometry {
        Some(g) => row_frame(g, width),
        None => rect(0.0, 0.0, 0.0, 0.0),
    }
}

- (id)indexPathForSelectedRow {
    let selected = env.objc.borrow::<UITableViewHostObject>(this).selected;
    match selected {
        Some((section, row)) => new_index_path(env, section, row),
        None => nil,
    }
}
- (())selectRowAtIndexPath:(id)index_path
                  animated:(bool)animated
            scrollPosition:(NSInteger)position {
    let target = index_path_parts(env, index_path);
    select_row(env, this, target, animated);
    if target.is_some() && position != 0 {
        () = msg![env; this scrollToRowAtIndexPath:index_path atScrollPosition:position animated:animated];
    }
}
- (())deselectRowAtIndexPath:(id)index_path
                    animated:(bool)animated {
    let target = index_path_parts(env, index_path);
    let selected = env.objc.borrow::<UITableViewHostObject>(this).selected;
    if target.is_some() && target == selected {
        select_row(env, this, None, animated);
    }
}
- (())scrollToRowAtIndexPath:(id)index_path
            atScrollPosition:(NSInteger)position
                    animated:(bool)_animated {
    let geometry = match index_path_parts(env, index_path) {
        Some((section, row)) => find_row(env, this, section, row),
        None => None,
    };
    let Some(g) = geometry else {
        return;
    };
    let bounds: CGRect = msg![env; this bounds];
    let content_size: CGSize = msg![env; this contentSize];
    let visible_height = bounds.size.height;
    let current_y = bounds.origin.y;
    let target_y = match position {
        UITableViewScrollPositionTop => g.y,
        UITableViewScrollPositionMiddle => g.y + g.height / 2.0 - visible_height / 2.0,
        UITableViewScrollPositionBottom => g.y + g.height - visible_height,
        _ => {
            // None:已完全可见就不动,否则滚到最近的一边。
            if g.y < current_y {
                g.y
            } else if g.y + g.height > current_y + visible_height {
                g.y + g.height - visible_height
            } else {
                current_y
            }
        }
    };
    let max_y = (content_size.height - visible_height).max(0.0);
    let offset = CGPoint { x: bounds.origin.x, y: target_y.min(max_y).max(0.0) };
    () = msg![env; this setContentOffset:offset];
}

// 合成前布局遍历里调用:首次或推迟的 reload,然后按可见区实体化 cell。
- (())layoutSubviews {
    () = msg_super![env; this layoutSubviews];
    if env.objc.borrow::<UITableViewHostObject>(this).in_layout {
        return;
    }
    env.objc.borrow_mut::<UITableViewHostObject>(this).in_layout = true;
    // 布局遍历不在 guest 事件回调里,自建一个池收纳本轮 autorelease 的索引路径与回收的 cell。
    let pool: id = msg_class![env; NSAutoreleasePool new];
    if env.objc.borrow::<UITableViewHostObject>(this).needs_reload {
        reload_data(env, this, false);
    }
    tile_visible_rows(env, this);
    env.objc.borrow_mut::<UITableViewHostObject>(this).in_layout = false;
    release(env, pool);
}

// 尺寸或滚动位置(UIScrollView setContentOffset: 会改 bounds.origin)变化后重新铺可见行。
- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    () = msg![env; this setNeedsLayout];
}
- (())setBounds:(CGRect)bounds {
    () = msg_super![env; this setBounds:bounds];
    () = msg![env; this setNeedsLayout];
}

// 触摸:记录起点 → 拖动交给 UIScrollView 滚动 → 抬起时未拖动则点选行。
- (())touchesBegan:(id)touches
         withEvent:(id)event {
    let touch: id = msg![env; touches anyObject];
    let location: CGPoint = msg![env; touch locationInView:nil];
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        host.touch_start = Some(location);
        host.touch_dragged = false;
    }
    if scroll_view_implements(env, "touchesBegan:withEvent:") {
        () = msg_super![env; this touchesBegan:touches withEvent:event];
    }
}
- (())touchesMoved:(id)touches
         withEvent:(id)event {
    let touch: id = msg![env; touches anyObject];
    let location: CGPoint = msg![env; touch locationInView:nil];
    {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        if let Some(start) = host.touch_start {
            let dx = location.x - start.x;
            let dy = location.y - start.y;
            // [复核修 2026-09-15] R2-1 返修:原为 `>`。触点坐标取整(window.rs),位移正好 10pt
            // ((0,10)/(6,8)…)时 UIScrollView 已起拖(`hypot < 10` 才不算),这里却还当点按;
            // 改成 `>=` 与起拖阈值对齐。可滚动时最终以 touchesEnded: 里的 ends_drag_in 为准。
            if dx * dx + dy * dy >= TAP_SLOP * TAP_SLOP {
                host.touch_dragged = true;
            }
        }
    }
    if scroll_view_implements(env, "touchesMoved:withEvent:") {
        () = msg_super![env; this touchesMoved:touches withEvent:event];
    }
}
// [复核修 2026-09-15] R2-2:原先先 msg_super、后换算坐标,且期间没 retain 表格也不查窗口。
// super(UIScrollView touchesEnded:)里会跑游戏代码(识别器 action、拖动结束委托回调;修 R2-1
// 之前还会往上转发给 -[TMAUserIDListView touchesEnded:withEvent:]@0x4e3b30,触点不在表格
// frame 内就 removeFromSuperview@0x4e3bd2),表格被连带摘出窗口后再 locationInView:this 会在
// ca_layer.rs transform_for_conversion panic;游戏代码释放了表格最后一个持有者时,这里还会用
// 悬垂的 this。改为:先 retain 表格、在 super 之前算好表格坐标(此时不在窗口就不算),
// super 返回后表格已离窗就不再点选,最后 release。
- (())touchesEnded:(id)touches
         withEvent:(id)event {
    retain(env, this);
    let touch: id = msg![env; touches anyObject];
    let (start, dragged, allows_selection) = {
        let host = env.objc.borrow_mut::<UITableViewHostObject>(this);
        (host.touch_start.take(), host.touch_dragged, host.allows_selection)
    };
    // [复核修 2026-09-15] R2-1 返修:super 这次会按拖动结束收尾(已发 scrollViewWillBeginDragging:、
    // 内容滚过)就不再点选。必须在 msg_super 之前查:super 里 stop_tracking 会清掉拖动状态。
    // 放在 && 链末尾,不是点选候选时不多发消息。
    let tap_candidate = !dragged
        && start.is_some()
        && allows_selection
        && touch != nil
        && !super::ui_scroll_view::ends_drag_in(env, this, touches);
    let point_in_table: Option<CGPoint> = if tap_candidate {
        let window: id = msg![env; this window];
        if window != nil {
            let point: CGPoint = msg![env; touch locationInView:this];
            Some(point)
        } else {
            None
        }
    } else {
        None
    };
    if scroll_view_implements(env, "touchesEnded:withEvent:") {
        () = msg_super![env; this touchesEnded:touches withEvent:event];
    }
    if let Some(point_in_table) = point_in_table {
        let window: id = msg![env; this window];
        if window != nil {
            handle_tap(env, this, point_in_table);
        } else {
            log_dbg!(
                "[复核修 2026-09-15] UITableView {:?} 在 touchesEnded: 处理中离开窗口,跳过点选",
                this
            );
        }
    }
    release(env, this);
}
- (())touchesCancelled:(id)touches
             withEvent:(id)event {
    env.objc.borrow_mut::<UITableViewHostObject>(this).touch_start = None;
    if scroll_view_implements(env, "touchesCancelled:withEvent:") {
        () = msg_super![env; this touchesCancelled:touches withEvent:event];
    }
}

@end

// [扫描修 2026-09-15] F8-1 / F9-7 / F11-8:最小 UITableViewCell。
@implementation UITableViewCell: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewCellHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithStyle:(NSInteger)style
    reuseIdentifier:(id)reuse_identifier {
    // iOS:默认 320×44,表格实体化时再按行几何设 frame。
    let this: id = msg_super![env; this initWithFrame:(rect(0.0, 0.0, 320.0, DEFAULT_ROW_HEIGHT))];
    if this == nil {
        return nil;
    }
    let identifier: id = if reuse_identifier != nil {
        msg![env; reuse_identifier copy]
    } else {
        nil
    };
    {
        let host = env.objc.borrow_mut::<UITableViewCellHostObject>(this);
        host.style = style;
        host.reuse_identifier = identifier;
    }
    ensure_content_view(env, this);
    this
}
// iOS 2.x 的旧初始化方法。
- (id)initWithFrame:(CGRect)frame
    reuseIdentifier:(id)reuse_identifier {
    let this: id = msg![env; this initWithStyle:UITableViewCellStyleDefault reuseIdentifier:reuse_identifier];
    if this != nil {
        () = msg![env; this setFrame:frame];
    }
    this
}

- (())dealloc {
    let owned = {
        let host = env.objc.borrow_mut::<UITableViewCellHostObject>(this);
        [
            std::mem::replace(&mut host.reuse_identifier, nil),
            std::mem::replace(&mut host.content_view, nil),
            std::mem::replace(&mut host.text_label, nil),
            std::mem::replace(&mut host.detail_text_label, nil),
            std::mem::replace(&mut host.image_view, nil),
            std::mem::replace(&mut host.accessory_view, nil),
            std::mem::replace(&mut host.background_view, nil),
            std::mem::replace(&mut host.selected_background_view, nil),
        ]
    };
    // 只释放本 cell 额外持有的那一份;子视图那一份由 UIView dealloc 释放。
    // 注意不能对整个宿主对象 mem::take:那会把内嵌的 UIViewHostObject 清成默认值,
    // UIView dealloc 就不再释放 layer/子视图。
    for &object in owned.iter() {
        release(env, object);
    }
    msg_super![env; this dealloc]
}

- (id)reuseIdentifier {
    env.objc.borrow::<UITableViewCellHostObject>(this).reuse_identifier
}
- (id)contentView {
    ensure_content_view(env, this)
}
- (id)textLabel {
    ensure_label(env, this, false)
}
- (id)detailTextLabel {
    ensure_label(env, this, true)
}
- (id)imageView {
    ensure_image_view(env, this)
}

- (NSInteger)accessoryType {
    env.objc.borrow::<UITableViewCellHostObject>(this).accessory_type
}
- (())setAccessoryType:(NSInteger)accessory_type {
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).accessory_type = accessory_type;
    () = msg![env; this setNeedsLayout];
}
- (())setEditingAccessoryType:(NSInteger)_accessory_type {
}
- (id)accessoryView {
    env.objc.borrow::<UITableViewCellHostObject>(this).accessory_view
}
- (())setAccessoryView:(id)view {
    let old = env.objc.borrow::<UITableViewCellHostObject>(this).accessory_view;
    if old == view {
        return;
    }
    if old != nil {
        () = msg![env; old removeFromSuperview];
        release(env, old);
    }
    retain(env, view);
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).accessory_view = view;
    if view != nil {
        () = msg![env; this addSubview:view];
    }
    () = msg![env; this setNeedsLayout];
}

- (NSInteger)selectionStyle {
    env.objc.borrow::<UITableViewCellHostObject>(this).selection_style
}
- (())setSelectionStyle:(NSInteger)style {
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).selection_style = style;
}

// 只记录状态,不绘制选中/高亮底色。MessageCell setSelected:animated:@0x1ae658 覆盖后调 super。
- (bool)isSelected {
    env.objc.borrow::<UITableViewCellHostObject>(this).selected
}
- (())setSelected:(bool)selected {
    () = msg![env; this setSelected:selected animated:false];
}
- (())setSelected:(bool)selected
         animated:(bool)_animated {
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).selected = selected;
}
- (bool)isHighlighted {
    env.objc.borrow::<UITableViewCellHostObject>(this).highlighted
}
- (())setHighlighted:(bool)highlighted {
    () = msg![env; this setHighlighted:highlighted animated:false];
}
- (())setHighlighted:(bool)highlighted
            animated:(bool)_animated {
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).highlighted = highlighted;
}

- (id)backgroundView {
    env.objc.borrow::<UITableViewCellHostObject>(this).background_view
}
- (())setBackgroundView:(id)view {
    let old = env.objc.borrow::<UITableViewCellHostObject>(this).background_view;
    if old == view {
        return;
    }
    if old != nil {
        () = msg![env; old removeFromSuperview];
        release(env, old);
    }
    retain(env, view);
    env.objc.borrow_mut::<UITableViewCellHostObject>(this).background_view = view;
    if view != nil {
        // 背景视图在最底层。
        () = msg![env; this insertSubview:view atIndex:(0 as NSInteger)];
    }
    () = msg![env; this setNeedsLayout];
}
- (id)selectedBackgroundView {
    env.objc.borrow::<UITableViewCellHostObject>(this).selected_background_view
}
- (())setSelectedBackgroundView:(id)view {
    // 不显示选中态,只持有。
    retain(env, view);
    let old = std::mem::replace(
        &mut env.objc.borrow_mut::<UITableViewCellHostObject>(this).selected_background_view,
        view,
    );
    release(env, old);
}

- (())prepareForReuse {
}

- (())layoutSubviews {
    () = msg_super![env; this layoutSubviews];
    layout_cell(env, this);
}

// 见 find_view_with_tag:iOS 语义是递归搜索。
- (id)viewWithTag:(NSInteger)tag {
    find_view_with_tag(env, this, tag, 0)
}

- (())setEditing:(bool)_editing
        animated:(bool)_animated {
}
- (())setIndentationLevel:(NSInteger)_level {
}
- (())setIndentationWidth:(CGFloat)_width {
}
- (())setShowsReorderControl:(bool)_shows {
}
- (())setShouldIndentWhileEditing:(bool)_indent {
}

// iOS 2.x 旧接口:转给 textLabel / imageView。
- (id)text {
    let label = env.objc.borrow::<UITableViewCellHostObject>(this).text_label;
    if label == nil {
        return nil;
    }
    msg![env; label text]
}
- (())setText:(id)text {
    let label = ensure_label(env, this, false);
    () = msg![env; label setText:text];
}
- (())setFont:(id)font {
    let label = ensure_label(env, this, false);
    () = msg![env; label setFont:font];
}
- (())setTextColor:(id)color {
    let label = ensure_label(env, this, false);
    () = msg![env; label setTextColor:color];
}
- (id)image {
    let image_view = env.objc.borrow::<UITableViewCellHostObject>(this).image_view;
    if image_view == nil {
        return nil;
    }
    msg![env; image_view image]
}
- (())setImage:(id)image {
    let image_view = ensure_image_view(env, this);
    () = msg![env; image_view setImage:image];
    () = msg![env; this setNeedsLayout];
}

@end

// [扫描修 2026-09-15] F8-1 / F9-7 / F11-8:最小 UITableViewController。
@implementation UITableViewController: UIViewController

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITableViewControllerHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// 游戏的 -[MessageViewController initWithStyle:listArray:]@0x1a6714 等走 [super initWithStyle:]。
- (id)initWithStyle:(NSInteger)style {
    {
        let host = env.objc.borrow_mut::<UITableViewControllerHostObject>(this);
        host.style = style;
        host.clears_selection_on_view_will_appear = true;
    }
    msg![env; this initWithNibName:nil bundle:nil]
}

// 控制器先于表格释放时清掉表格里指向自己的弱引用(iOS 同样这么做),
// 否则表格下一次布局会向野指针要数据。
- (())dealloc {
    let view = view_if_loaded(env, this);
    if view != nil && is_kind_of_host_class(env, view, "UITableView") {
        let data_source = env.objc.borrow::<UITableViewHostObject>(view).data_source;
        if data_source == this {
            env.objc.borrow_mut::<UITableViewHostObject>(view).data_source = nil;
        }
        let delegate: id = msg![env; view delegate];
        if delegate == this {
            () = msg![env; view setDelegate:nil];
        }
    }
    msg_super![env; this dealloc]
}

// iOS:建一个全尺寸表格作为根视图,dataSource/delegate 设为自己。
// 不走 nib:游戏的三个列表控制器都没有 nib;UIViewController 的 nib 路径在找不到视图时会 assert。
- (())loadView {
    let style = env.objc.borrow::<UITableViewControllerHostObject>(this).style;
    let screen: id = msg_class![env; UIScreen mainScreen];
    let frame: CGRect = msg![env; screen applicationFrame];
    let table: id = msg_class![env; UITableView alloc];
    let table: id = msg![env; table initWithFrame:frame style:style];
    () = msg![env; table setDataSource:this];
    () = msg![env; table setDelegate:this];
    () = msg![env; this setView:table];
    release(env, table);
}

- (id)tableView {
    let view: id = msg![env; this view];
    if is_kind_of_host_class(env, view, "UITableView") {
        view
    } else {
        nil
    }
}
- (())setTableView:(id)table_view {
    () = msg![env; this setView:table_view];
}

- (bool)clearsSelectionOnViewWillAppear {
    env.objc.borrow::<UITableViewControllerHostObject>(this).clears_selection_on_view_will_appear
}
- (())setClearsSelectionOnViewWillAppear:(bool)clears {
    env.objc.borrow_mut::<UITableViewControllerHostObject>(this).clears_selection_on_view_will_appear = clears;
}

- (())viewWillAppear:(bool)animated {
    () = msg_super![env; this viewWillAppear:animated];
    let clears = env.objc.borrow::<UITableViewControllerHostObject>(this).clears_selection_on_view_will_appear;
    let view = view_if_loaded(env, this);
    if clears && view != nil && is_kind_of_host_class(env, view, "UITableView") {
        select_row(env, view, None, animated);
    }
}

@end

};
