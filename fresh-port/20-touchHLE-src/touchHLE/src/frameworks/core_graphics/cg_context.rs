/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CGContext.h`

use super::cg_affine_transform::CGAffineTransform;
use super::cg_image::CGImageRef;
use super::{cg_bitmap_context, cg_color, CGFloat, CGPoint, CGRect};
use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::core_foundation::{CFRelease, CFRetain, CFTypeRef};
use crate::frameworks::core_graphics::cg_bitmap_context::{
    CGBitmapContextDrawer, CGBitmapContextGetHeight, CGBitmapContextGetWidth,
};
use crate::frameworks::core_graphics::cg_color::CGColorRef;
use crate::frameworks::core_graphics::cg_font::{
    CGFontHostObject, CGFontRef, CGFontRelease, CGFontRetain, CGGlyph,
};
use crate::frameworks::core_graphics::cg_geometry::CGPointZero;
use crate::frameworks::uikit;
use crate::mem::{ConstPtr, GuestUSize};
use crate::objc::{objc_classes, ClassExports, HostObject};
use crate::Environment;
use std::cell::RefCell;
use std::collections::HashMap;
use std::f32::consts::PI;

type CGInterpolationQuality = i32;

type CGTextDrawingMode = i32;
const kCGTextFill: CGTextDrawingMode = 0;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// CGContext seems to be a CFType-based type, but in our implementation those
// are just Objective-C types, so we need a class for it, but its name is not
// visible anywhere.
@implementation _touchHLE_CGContext: NSObject

- (())dealloc {
    let host_obj = env.objc.borrow::<CGContextHostObject>(this);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    if bitmap_data.data_is_owned {
        env.mem.free(bitmap_data.data);
    }
    CGFontRelease(env, host_obj.font);

    // [深扫修 2026-09-11] #23(b):清掉该上下文的路径/描边侧表条目,
    // 防止地址复用后新上下文继承旧路径。
    PATH_EXTRAS.with(|map| {
        map.borrow_mut().remove(&this);
    });

    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};

// TODO: keep more states saved once they are implemented
type ContextState = (
    (CGFloat, CGFloat, CGFloat, CGFloat),
    CGAffineTransform,
    CGFontRef,
    CGFloat,
);

pub(super) struct CGContextHostObject {
    pub(super) subclass: CGContextSubclass,
    pub(super) rgb_fill_color: (CGFloat, CGFloat, CGFloat, CGFloat),
    pub(super) font: CGFontRef,
    pub(super) font_size: CGFloat,
    /// Current transform.
    pub(super) transform: CGAffineTransform,
    pub(super) state_stack: Vec<ContextState>,
}
impl HostObject for CGContextHostObject {}

pub(super) enum CGContextSubclass {
    CGBitmapContext(cg_bitmap_context::CGBitmapContextData),
}

pub type CGContextRef = CFTypeRef;

pub fn CGContextRelease(env: &mut Environment, c: CGContextRef) {
    if !c.is_null() {
        CFRelease(env, c);
    }
}
pub fn CGContextRetain(env: &mut Environment, c: CGContextRef) -> CGContextRef {
    if !c.is_null() {
        CFRetain(env, c)
    } else {
        c
    }
}

fn CGContextSetFillColorWithColor(env: &mut Environment, context: CGContextRef, color: CGColorRef) {
    let (r, g, b, a) = cg_color::to_rgba(&env.objc, color);
    CGContextSetRGBFillColor(env, context, r, g, b, a)
}

pub fn CGContextSetRGBFillColor(
    env: &mut Environment,
    context: CGContextRef,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) {
    let color = (red, green, blue, alpha);
    env.objc
        .borrow_mut::<CGContextHostObject>(context)
        .rgb_fill_color = color;
}

fn CGContextSetGrayFillColor(
    env: &mut Environment,
    context: CGContextRef,
    gray: CGFloat,
    alpha: CGFloat,
) {
    let color = (gray, gray, gray, alpha);
    env.objc
        .borrow_mut::<CGContextHostObject>(context)
        .rgb_fill_color = color;
}

pub fn CGContextFillRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    cg_bitmap_context::fill_rect(env, context, rect, /* clear: */ false);
}

pub fn CGContextClearRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    cg_bitmap_context::fill_rect(env, context, rect, /* clear: */ true);
}

fn CGContextClipToRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    if rect.origin == CGPointZero
        && rect.size.height == CGBitmapContextGetHeight(env, context) as f32
        && rect.size.width == CGBitmapContextGetWidth(env, context) as f32
    {
        assert!(env
            .objc
            .borrow_mut::<CGContextHostObject>(context)
            .transform
            .is_identity());
        // All good, clipping is not needed!
        return;
    }
    todo!();
}

pub fn CGContextConcatCTM(
    env: &mut Environment,
    context: CGContextRef,
    transform: CGAffineTransform,
) {
    log_dbg!("CGContextConcatCTM({:?})", transform);
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    host_obj.transform = transform.concat(host_obj.transform);
}
pub fn CGContextGetCTM(env: &mut Environment, context: CGContextRef) -> CGAffineTransform {
    let res = env.objc.borrow::<CGContextHostObject>(context).transform;
    log_dbg!("CGContextGetCTM() => {:?}", res);
    res
}
pub fn CGContextRotateCTM(env: &mut Environment, context: CGContextRef, angle: CGFloat) {
    log_dbg!("CGContextRotateCTM({:?})", angle);
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    host_obj.transform = host_obj.transform.rotate(angle);
}
pub fn CGContextScaleCTM(env: &mut Environment, context: CGContextRef, x: CGFloat, y: CGFloat) {
    log_dbg!("CGContextScaleCTM({:?})", (x, y));
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    host_obj.transform = host_obj.transform.scale(x, y);
}
pub fn CGContextTranslateCTM(
    env: &mut Environment,
    context: CGContextRef,
    tx: CGFloat,
    ty: CGFloat,
) {
    log_dbg!("CGContextTranslateCTM({:?})", (tx, ty));
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    host_obj.transform = host_obj.transform.translate(tx, ty);
}

pub fn CGContextDrawImage(
    env: &mut Environment,
    context: CGContextRef,
    rect: CGRect,
    image: CGImageRef,
) {
    cg_bitmap_context::draw_image(env, context, rect, image);
}

// [深扫修 2026-09-11] 改为 pub,供 UIActivityIndicatorView 的 drawRect: 旋转绘制使用。
pub fn CGContextSaveGState(env: &mut Environment, context: CGContextRef) {
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    host_obj.state_stack.push((
        host_obj.rgb_fill_color,
        host_obj.transform,
        host_obj.font,
        host_obj.font_size,
    ));
    CGFontRetain(env, env.objc.borrow::<CGContextHostObject>(context).font);
    // [深扫修 2026-09-11] #23(b):描边颜色/线宽属于图形状态,一并入栈
    // (路径本身按 CG 语义不属于图形状态,不保存)。
    with_path_extras(context, |extras| {
        let stroke = extras.stroke;
        extras.stroke_stack.push(stroke);
    });
}

// [深扫修 2026-09-11] 改为 pub,理由同上。
pub fn CGContextRestoreGState(env: &mut Environment, context: CGContextRef) {
    // [深扫修 2026-09-11] #23(b):恢复描边状态(栈空时容忍,不 panic)。
    with_path_extras(context, |extras| {
        if let Some(stroke) = extras.stroke_stack.pop() {
            extras.stroke = stroke;
        }
    });

    // We need to release _old_ font, there are 2 cases:
    // - font hasn't been set between save/restore -> this release corresponds
    // the font retain from save
    // - font has been set between save/restore -> we need to release old font
    // retained on the set
    CGFontRelease(env, env.objc.borrow::<CGContextHostObject>(context).font);
    let host_obj = env.objc.borrow_mut::<CGContextHostObject>(context);
    let state = host_obj.state_stack.pop().unwrap();
    host_obj.rgb_fill_color = state.0;
    host_obj.transform = state.1;
    host_obj.font = state.2;
    host_obj.font_size = state.3;
}

fn CGContextSetInterpolationQuality(
    _env: &mut Environment,
    context: CGContextRef,
    quality: CGInterpolationQuality,
) {
    log!(
        "TODO: CGContextSetInterpolationQuality({:?}, {:?})",
        context,
        quality
    );
}
fn CGContextSetAllowsAntialiasing(_env: &mut Environment, context: CGContextRef, allow: bool) {
    log!(
        "TODO: CGContextSetAllowsAntialiasing({:?}, {})",
        context,
        allow
    );
}

fn CGContextSetFont(env: &mut Environment, context: CGContextRef, font: CGFontRef) {
    CGFontRetain(env, font);
    let old_font = env.objc.borrow_mut::<CGContextHostObject>(context).font;
    CGFontRelease(env, old_font);
    env.objc.borrow_mut::<CGContextHostObject>(context).font = font;
}

fn CGContextSetFontSize(env: &mut Environment, context: CGContextRef, size: CGFloat) {
    env.objc
        .borrow_mut::<CGContextHostObject>(context)
        .font_size = size;
}

fn CGContextSetTextDrawingMode(
    _env: &mut Environment,
    _context: CGContextRef,
    mode: CGTextDrawingMode,
) {
    assert_eq!(mode, kCGTextFill); // TODO: support other modes
}

fn CGContextShowGlyphsAtPoint(
    env: &mut Environment,
    context: CGContextRef,
    x: CGFloat,
    y: CGFloat,
    glyphs: ConstPtr<CGGlyph>,
    count: GuestUSize,
) {
    let mut glyph_ids = Vec::new();
    for i in 0..count {
        let glyph_id = env.mem.read(glyphs + i);
        glyph_ids.push(rusttype::GlyphId(glyph_id));
    }

    let font = env.objc.borrow::<CGContextHostObject>(context).font;
    let font_size = env.objc.borrow::<CGContextHostObject>(context).font_size;

    let font = &env.objc.borrow::<CGFontHostObject>(font).font;

    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
    let fill_color = drawer.rgb_fill_color();

    font.draw_glyphs(font_size, glyph_ids, (x, y), |raster_glyph| {
        uikit::ui_font::draw_font_glyph(
            &mut drawer,
            raster_glyph,
            fill_color,
            /* clip_x: */ None,
            /* clip_y: */ None,
        )
    });
}

// ============================================================================
// [深扫修 2026-09-11] #23(b):CoreGraphics 路径 API。
//
// 根因:CGContextBeginPath/MoveToPoint/AddArc/ClosePath/FillPath 等此前都没导出,
// dyld 把它们链接到"返回 0 的空桩",MBProgressHUD -fillRoundedRect:inContext:
// (@0x12cc70)画的半透明圆角底框什么都不出。
//
// 实现:最小但语义正确的软件光栅化,复用现有 CGBitmapContextDrawer
// (与 CGContextFillRect 同一套变换/坐标/预乘/伽马/混合逻辑,保证画出来的
// 坐标系与 FillRect 一致):
// - 路径点在"加入路径时"就用当时的 CTM 变换到设备空间(Quartz 语义)。
// - 圆弧用折线近似,分段数按设备空间半径保证弦高误差 ≤ 0.25px。
// - 填充 = 扫描线多边形填充,取像素中心采样,支持非零环绕(FillPath)与
//   奇偶(EOFillPath)规则,开放子路径隐式闭合;凸多边形/圆角矩形完全正确。
// - 描边 = 每条线段扩成同向矩形后按非零规则并集填充(内部拐点两端各延长
//   半个线宽补接缝,近似 square/miter join),无抗锯齿。
// 路径与描边状态放在按上下文指针索引的侧表里(上下文 dealloc 时清除),
// 这样不必改动 CGContextHostObject 的构造处。
// 所有新导出函数都容忍 NULL 上下文(CG 原版对 NULL 也是打日志后返回)。
// ============================================================================

type CGPathDrawingMode = i32;
const kCGPathFill: CGPathDrawingMode = 0;
const kCGPathEOFill: CGPathDrawingMode = 1;
const kCGPathStroke: CGPathDrawingMode = 2;
const kCGPathFillStroke: CGPathDrawingMode = 3;
const kCGPathEOFillStroke: CGPathDrawingMode = 4;

type RgbaColor = (CGFloat, CGFloat, CGFloat, CGFloat);

/// 一条子路径,点已在设备空间。
struct SubPath {
    points: Vec<CGPoint>,
    closed: bool,
}

#[derive(Copy, Clone)]
struct StrokeState {
    rgb_stroke_color: RgbaColor,
    line_width: CGFloat,
}
impl Default for StrokeState {
    fn default() -> Self {
        // CG 默认描边色为不透明黑色,线宽 1。
        StrokeState {
            rgb_stroke_color: (0.0, 0.0, 0.0, 1.0),
            line_width: 1.0,
        }
    }
}

#[derive(Default)]
struct PathExtras {
    subpaths: Vec<SubPath>,
    stroke: StrokeState,
    stroke_stack: Vec<StrokeState>,
}

thread_local! {
    static PATH_EXTRAS: RefCell<HashMap<CGContextRef, PathExtras>> = RefCell::new(HashMap::new());
}

fn with_path_extras<R>(context: CGContextRef, f: impl FnOnce(&mut PathExtras) -> R) -> R {
    PATH_EXTRAS.with(|map| f(map.borrow_mut().entry(context).or_default()))
}

fn current_transform(env: &Environment, context: CGContextRef) -> CGAffineTransform {
    env.objc.borrow::<CGContextHostObject>(context).transform
}

/// CTM 的平均缩放系数(用于把用户空间长度换算成设备像素)。
fn device_scale(transform: CGAffineTransform) -> CGFloat {
    let det = transform.a * transform.d - transform.b * transform.c;
    let scale = det.abs().sqrt();
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// 圆弧折线分段数:弦高误差 ≤ 0.25 设备像素,限制在 [1, 1024]。
fn arc_segment_count(radius_device: CGFloat, sweep: CGFloat) -> usize {
    let radius = radius_device.abs().max(0.5);
    let step = 2.0 * (1.0 - (0.25 / radius).min(1.0)).acos();
    let step = if step.is_finite() && step > 1e-3 { step } else { 1e-3 };
    let count = (sweep.abs() / step).ceil();
    if count.is_finite() {
        (count as usize).clamp(1, 1024)
    } else {
        1
    }
}

fn path_current_point(extras: &PathExtras) -> Option<CGPoint> {
    let subpath = extras.subpaths.last()?;
    if subpath.closed {
        subpath.points.first().copied()
    } else {
        subpath.points.last().copied()
    }
}

fn path_move_to(extras: &mut PathExtras, point: CGPoint) {
    // 连续 MoveTo 只替换起点,不留下单点子路径。
    if let Some(subpath) = extras.subpaths.last_mut() {
        if !subpath.closed && subpath.points.len() == 1 {
            subpath.points[0] = point;
            return;
        }
    }
    extras.subpaths.push(SubPath {
        points: vec![point],
        closed: false,
    });
}

/// 返回 false 表示没有当前点(CG 原版此时报错并忽略)。
fn path_line_to(extras: &mut PathExtras, point: CGPoint) -> bool {
    let Some(subpath) = extras.subpaths.last_mut() else {
        return false;
    };
    if subpath.closed {
        // 闭合后继续画线:从闭合子路径的起点开新子路径。
        let start = subpath.points.first().copied().unwrap_or(point);
        extras.subpaths.push(SubPath {
            points: vec![start, point],
            closed: false,
        });
    } else {
        subpath.points.push(point);
    }
    true
}

/// 在用户空间按圆心/半径/起始角/扫过角生成折线并加入路径;
/// 有当前点时先连一条直线到圆弧起点(CGContextAddArc 语义)。
fn path_add_arc(
    extras: &mut PathExtras,
    transform: CGAffineTransform,
    center: CGPoint,
    radius: CGFloat,
    start_angle: CGFloat,
    sweep: CGFloat,
) {
    let point_at = |angle: CGFloat| {
        transform.apply_to_point(CGPoint {
            x: center.x + radius * angle.cos(),
            y: center.y + radius * angle.sin(),
        })
    };
    let first = point_at(start_angle);
    if path_current_point(extras).is_some() {
        path_line_to(extras, first);
    } else {
        path_move_to(extras, first);
    }
    let count = arc_segment_count(radius * device_scale(transform), sweep);
    for i in 1..=count {
        let angle = start_angle + sweep * (i as CGFloat / count as CGFloat);
        path_line_to(extras, point_at(angle));
    }
}

fn rect_device_points(transform: CGAffineTransform, rect: CGRect) -> Vec<CGPoint> {
    let CGRect { origin, size } = rect;
    [
        CGPoint { x: origin.x, y: origin.y },
        CGPoint { x: origin.x + size.width, y: origin.y },
        CGPoint { x: origin.x + size.width, y: origin.y + size.height },
        CGPoint { x: origin.x, y: origin.y + size.height },
    ]
    .into_iter()
    .map(|point| transform.apply_to_point(point))
    .collect()
}

fn ellipse_device_points(transform: CGAffineTransform, rect: CGRect) -> Vec<CGPoint> {
    let (rx, ry) = (rect.size.width / 2.0, rect.size.height / 2.0);
    let center = CGPoint {
        x: rect.origin.x + rx,
        y: rect.origin.y + ry,
    };
    let count = arc_segment_count(rx.abs().max(ry.abs()) * device_scale(transform), 2.0 * PI).max(8);
    (0..count)
        .map(|i| {
            let angle = 2.0 * PI * (i as CGFloat / count as CGFloat);
            transform.apply_to_point(CGPoint {
                x: center.x + rx * angle.cos(),
                y: center.y + ry * angle.sin(),
            })
        })
        .collect()
}

/// 扫描线填充设备空间多边形集合。`color_override` 为 None 时用当前填充色,
/// 否则用给定颜色(描边时传描边色)。颜色的预乘/伽马转换沿用
/// CGBitmapContextDrawer::rgb_fill_color(),与 FillRect 完全一致。
fn fill_device_polygons(
    env: &mut Environment,
    context: CGContextRef,
    polygons: &[Vec<CGPoint>],
    even_odd: bool,
    color_override: Option<RgbaColor>,
) {
    // (x0, y0, x1, y1, 方向)
    let mut edges: Vec<(CGFloat, CGFloat, CGFloat, CGFloat, i32)> = Vec::new();
    let mut y_min = CGFloat::INFINITY;
    let mut y_max = CGFloat::NEG_INFINITY;
    for polygon in polygons {
        if polygon.len() < 3
            || polygon
                .iter()
                .any(|point| !point.x.is_finite() || !point.y.is_finite())
        {
            continue;
        }
        for i in 0..polygon.len() {
            let p0 = polygon[i];
            let p1 = polygon[(i + 1) % polygon.len()];
            if p0.y == p1.y {
                continue; // 水平边对扫描线无贡献
            }
            let direction = if p1.y > p0.y { 1 } else { -1 };
            edges.push((p0.x, p0.y, p1.x, p1.y, direction));
            y_min = y_min.min(p0.y.min(p1.y));
            y_max = y_max.max(p0.y.max(p1.y));
        }
    }
    if edges.is_empty() {
        return;
    }

    // 取颜色:临时把填充色换成覆盖色,借 drawer 做同样的颜色转换后立即还原。
    let original_fill_color = env.objc.borrow::<CGContextHostObject>(context).rgb_fill_color;
    if let Some(color) = color_override {
        env.objc
            .borrow_mut::<CGContextHostObject>(context)
            .rgb_fill_color = color;
    }
    let color = {
        let drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
        drawer.rgb_fill_color()
    };
    if color_override.is_some() {
        env.objc
            .borrow_mut::<CGContextHostObject>(context)
            .rgb_fill_color = original_fill_color;
    }

    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
    let width = drawer.width().min(i32::MAX as GuestUSize) as i32;
    let height = drawer.height().min(i32::MAX as GuestUSize) as i32;

    // 采样点 = 像素中心 (col + 0.5, row + 0.5)。
    let row_start = (y_min - 0.5).ceil().max(0.0) as i32;
    let row_end = ((y_max - 0.5).ceil() as i32).min(height);
    let mut crossings: Vec<(CGFloat, i32)> = Vec::new();
    for row in row_start..row_end {
        let sample_y = row as CGFloat + 0.5;
        crossings.clear();
        for &(x0, y0, x1, y1, direction) in &edges {
            let (low, high) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
            if sample_y < low || sample_y >= high {
                continue;
            }
            let x = x0 + (sample_y - y0) * (x1 - x0) / (y1 - y0);
            crossings.push((x, direction));
        }
        if crossings.len() < 2 {
            continue;
        }
        crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut winding = 0i32;
        for i in 0..crossings.len() - 1 {
            winding += if even_odd { 1 } else { crossings[i].1 };
            let inside = if even_odd {
                winding % 2 != 0
            } else {
                winding != 0
            };
            if !inside {
                continue;
            }
            let col_start = (crossings[i].0 - 0.5).ceil().max(0.0) as i32;
            let col_end = ((crossings[i + 1].0 - 0.5).ceil() as i32).min(width);
            for col in col_start..col_end {
                drawer.put_pixel((col, row), color, /* blend: */ true);
            }
        }
    }
}

/// 把子路径描边转换成一组同向矩形(设备空间)。所有矩形由同一个规范矩形
/// 旋转平移得到,环绕方向一致,因此按非零规则填充即为并集(重叠处不会抵消)。
fn stroke_outline_polygons(subpaths: &[SubPath], half_width: CGFloat) -> Vec<Vec<CGPoint>> {
    let mut polygons = Vec::new();
    for subpath in subpaths {
        let n = subpath.points.len();
        if n < 2 {
            continue;
        }
        let segment_count = if subpath.closed { n } else { n - 1 };
        for i in 0..segment_count {
            let p0 = subpath.points[i];
            let p1 = subpath.points[(i + 1) % n];
            let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
            let length = (dx * dx + dy * dy).sqrt();
            if !(length > 1e-6) {
                continue;
            }
            let (ux, uy) = (dx / length, dy / length);
            let extend_start = if subpath.closed || i > 0 { half_width } else { 0.0 };
            let extend_end = if subpath.closed || i + 1 < segment_count {
                half_width
            } else {
                0.0
            };
            let a = CGPoint {
                x: p0.x - ux * extend_start,
                y: p0.y - uy * extend_start,
            };
            let b = CGPoint {
                x: p1.x + ux * extend_end,
                y: p1.y + uy * extend_end,
            };
            let (nx, ny) = (-uy * half_width, ux * half_width);
            polygons.push(vec![
                CGPoint { x: a.x + nx, y: a.y + ny },
                CGPoint { x: b.x + nx, y: b.y + ny },
                CGPoint { x: b.x - nx, y: b.y - ny },
                CGPoint { x: a.x - nx, y: a.y - ny },
            ]);
        }
    }
    polygons
}

fn fill_subpaths(env: &mut Environment, context: CGContextRef, subpaths: &[SubPath], even_odd: bool) {
    let polygons: Vec<Vec<CGPoint>> = subpaths
        .iter()
        .filter(|subpath| subpath.points.len() >= 3)
        .map(|subpath| subpath.points.clone())
        .collect();
    fill_device_polygons(env, context, &polygons, even_odd, None);
}

fn stroke_subpaths(env: &mut Environment, context: CGContextRef, subpaths: &[SubPath]) {
    let stroke = with_path_extras(context, |extras| extras.stroke);
    // 线宽换算到设备像素;至少 1px,保证细线在无抗锯齿采样下仍可见。
    let width = (stroke.line_width * device_scale(current_transform(env, context))).max(1.0);
    let polygons = stroke_outline_polygons(subpaths, width / 2.0);
    fill_device_polygons(env, context, &polygons, false, Some(stroke.rgb_stroke_color));
}

fn take_subpaths(context: CGContextRef) -> Vec<SubPath> {
    with_path_extras(context, |extras| std::mem::take(&mut extras.subpaths))
}

fn CGContextBeginPath(_env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    with_path_extras(context, |extras| extras.subpaths.clear());
}

// [审查修 2026-09-13] E21:补 CGContextClip / CGContextEOClip(最小版)。
// 根因:这两个函数没导出,dyld 把它们链接到空桩。路径 API 补上之后,当前路径常驻
// PATH_EXTRAS 侧表,空桩不清路径,于是裁剪用的轮廓会被后面的 StrokePath/FillPath
// 一起画出来。例:AtomCustomAdView 的圆角裁剪框被描边;YMLProgressView 的凹槽和
// 进度矩形按非零规则合并填充。在复用的 CALayer 上下文里,这些路径还会跨 drawRect: 累积。
// 修法:按 CG 语义,确定裁剪区后把当前路径重置为空,这里只做清路径这一步。
// 取舍:暂不做真正裁剪(需要给光栅化加裁剪蒙版,本轮不做),裁剪区外的内容照画,
// 这部分和修复前空桩时一样。TODO 日志两个函数共用一处 log_once!,整个进程只打一次,
// 避免每次 drawRect: 刷屏。
fn clip_discard_current_path(context: CGContextRef) {
    log_once!("TODO: CGContextClip/CGContextEOClip: clipping is not implemented, only the current path is cleared");
    let _ = take_subpaths(context);
}

fn CGContextClip(_env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    clip_discard_current_path(context);
}

fn CGContextEOClip(_env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    clip_discard_current_path(context);
}

fn CGContextMoveToPoint(env: &mut Environment, context: CGContextRef, x: CGFloat, y: CGFloat) {
    if context.is_null() {
        return;
    }
    let point = current_transform(env, context).apply_to_point(CGPoint { x, y });
    with_path_extras(context, |extras| path_move_to(extras, point));
}

fn CGContextAddLineToPoint(env: &mut Environment, context: CGContextRef, x: CGFloat, y: CGFloat) {
    if context.is_null() {
        return;
    }
    let point = current_transform(env, context).apply_to_point(CGPoint { x, y });
    if !with_path_extras(context, |extras| path_line_to(extras, point)) {
        log_dbg!("CGContextAddLineToPoint({:?}): no current point, ignored", context);
    }
}

fn CGContextAddArc(
    env: &mut Environment,
    context: CGContextRef,
    x: CGFloat,
    y: CGFloat,
    radius: CGFloat,
    start_angle: CGFloat,
    end_angle: CGFloat,
    clockwise: i32,
) {
    if context.is_null() {
        return;
    }
    // CG 语义:clockwise == 0 表示角度递增方向,== 1 表示角度递减方向
    // (纯数学定义,与上下文是否翻转无关)。扫过角规范到 [0, 2π] / [-2π, 0]。
    // 例:MBProgressHUD 右上角 AddArc(3π/2 → 0, clockwise=0) → 扫过 +π/2。
    let two_pi = 2.0 * PI;
    let mut sweep = end_angle - start_angle;
    if clockwise == 0 {
        if sweep < 0.0 {
            sweep = sweep.rem_euclid(two_pi);
        } else if sweep > two_pi {
            sweep = two_pi;
        }
    } else if sweep > 0.0 {
        let remainder = sweep.rem_euclid(two_pi);
        sweep = if remainder == 0.0 { 0.0 } else { remainder - two_pi };
    } else if sweep < -two_pi {
        sweep = -two_pi;
    }
    let transform = current_transform(env, context);
    with_path_extras(context, |extras| {
        path_add_arc(extras, transform, CGPoint { x, y }, radius, start_angle, sweep)
    });
}

fn CGContextAddArcToPoint(
    env: &mut Environment,
    context: CGContextRef,
    x1: CGFloat,
    y1: CGFloat,
    x2: CGFloat,
    y2: CGFloat,
    radius: CGFloat,
) {
    if context.is_null() {
        return;
    }
    let transform = current_transform(env, context);
    let inverse = transform.invert();
    let p1 = CGPoint { x: x1, y: y1 };
    let p2 = CGPoint { x: x2, y: y2 };
    with_path_extras(context, |extras| {
        let Some(current_device) = path_current_point(extras) else {
            log_dbg!("CGContextAddArcToPoint: no current point, ignored");
            return;
        };
        // 切线圆弧在用户空间计算:当前点反变换回用户空间。
        let p0 = inverse.apply_to_point(current_device);
        let (v1x, v1y) = (p0.x - p1.x, p0.y - p1.y);
        let (v2x, v2y) = (p2.x - p1.x, p2.y - p1.y);
        let len1 = (v1x * v1x + v1y * v1y).sqrt();
        let len2 = (v2x * v2x + v2y * v2y).sqrt();
        if !(len1 > 1e-6 && len2 > 1e-6 && radius > 0.0) {
            path_line_to(extras, transform.apply_to_point(p1));
            return;
        }
        let (u1x, u1y) = (v1x / len1, v1y / len1);
        let (u2x, u2y) = (v2x / len2, v2y / len2);
        let theta = (u1x * u2x + u1y * u2y).clamp(-1.0, 1.0).acos();
        if !(theta > 1e-4 && PI - theta > 1e-4) {
            // 三点共线:退化为直线到 (x1, y1)。
            path_line_to(extras, transform.apply_to_point(p1));
            return;
        }
        let tangent_distance = radius / (theta / 2.0).tan();
        let center_distance = radius / (theta / 2.0).sin();
        let t1 = CGPoint {
            x: p1.x + u1x * tangent_distance,
            y: p1.y + u1y * tangent_distance,
        };
        let t2 = CGPoint {
            x: p1.x + u2x * tangent_distance,
            y: p1.y + u2y * tangent_distance,
        };
        let (bx, by) = (u1x + u2x, u1y + u2y);
        let bisector_length = (bx * bx + by * by).sqrt();
        let center = CGPoint {
            x: p1.x + bx / bisector_length * center_distance,
            y: p1.y + by / bisector_length * center_distance,
        };
        let a1 = (t1.y - center.y).atan2(t1.x - center.x);
        let a2 = (t2.y - center.y).atan2(t2.x - center.x);
        let mut sweep = a2 - a1;
        if sweep > PI {
            sweep -= 2.0 * PI;
        } else if sweep < -PI {
            sweep += 2.0 * PI;
        }
        path_line_to(extras, transform.apply_to_point(t1));
        path_add_arc(extras, transform, center, radius, a1, sweep);
    });
}

fn CGContextAddRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    if context.is_null() {
        return;
    }
    let points = rect_device_points(current_transform(env, context), rect);
    with_path_extras(context, |extras| {
        extras.subpaths.push(SubPath {
            points,
            closed: true,
        })
    });
}

fn CGContextClosePath(_env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    with_path_extras(context, |extras| {
        if let Some(subpath) = extras.subpaths.last_mut() {
            if !subpath.points.is_empty() {
                subpath.closed = true;
            }
        }
    });
}

fn CGContextFillPath(env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    // 绘制后清空当前路径(CG 语义)。
    let subpaths = take_subpaths(context);
    fill_subpaths(env, context, &subpaths, /* even_odd: */ false);
}

fn CGContextEOFillPath(env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    let subpaths = take_subpaths(context);
    fill_subpaths(env, context, &subpaths, /* even_odd: */ true);
}

fn CGContextStrokePath(env: &mut Environment, context: CGContextRef) {
    if context.is_null() {
        return;
    }
    let subpaths = take_subpaths(context);
    stroke_subpaths(env, context, &subpaths);
}

fn CGContextDrawPath(env: &mut Environment, context: CGContextRef, mode: CGPathDrawingMode) {
    if context.is_null() {
        return;
    }
    let subpaths = take_subpaths(context);
    match mode {
        kCGPathFill => fill_subpaths(env, context, &subpaths, false),
        kCGPathEOFill => fill_subpaths(env, context, &subpaths, true),
        kCGPathStroke => stroke_subpaths(env, context, &subpaths),
        kCGPathFillStroke => {
            fill_subpaths(env, context, &subpaths, false);
            stroke_subpaths(env, context, &subpaths);
        }
        kCGPathEOFillStroke => {
            fill_subpaths(env, context, &subpaths, true);
            stroke_subpaths(env, context, &subpaths);
        }
        _ => {
            log!("TODO: CGContextDrawPath unknown mode {}", mode);
        }
    }
}

fn CGContextSetLineWidth(_env: &mut Environment, context: CGContextRef, width: CGFloat) {
    if context.is_null() {
        return;
    }
    with_path_extras(context, |extras| extras.stroke.line_width = width);
}

// [审查修 2026-09-13] E22:改为 pub,供 -[UIColor set] / -[UIColor setStroke] 设置描边色
// (本函数已容忍 NULL 上下文,外部直接调用安全)。
pub fn CGContextSetRGBStrokeColor(
    _env: &mut Environment,
    context: CGContextRef,
    red: CGFloat,
    green: CGFloat,
    blue: CGFloat,
    alpha: CGFloat,
) {
    if context.is_null() {
        return;
    }
    with_path_extras(context, |extras| {
        extras.stroke.rgb_stroke_color = (red, green, blue, alpha)
    });
}

fn CGContextSetGrayStrokeColor(
    _env: &mut Environment,
    context: CGContextRef,
    gray: CGFloat,
    alpha: CGFloat,
) {
    if context.is_null() {
        return;
    }
    with_path_extras(context, |extras| {
        extras.stroke.rgb_stroke_color = (gray, gray, gray, alpha)
    });
}

fn CGContextSetStrokeColorWithColor(env: &mut Environment, context: CGContextRef, color: CGColorRef) {
    if context.is_null() || color.is_null() {
        return;
    }
    let rgba = cg_color::to_rgba(&env.objc, color);
    with_path_extras(context, |extras| extras.stroke.rgb_stroke_color = rgba);
}

fn CGContextStrokeRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    if context.is_null() {
        return;
    }
    // 不影响当前路径。
    let subpaths = [SubPath {
        points: rect_device_points(current_transform(env, context), rect),
        closed: true,
    }];
    stroke_subpaths(env, context, &subpaths);
}

fn CGContextFillEllipseInRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    if context.is_null() {
        return;
    }
    let polygons = [ellipse_device_points(current_transform(env, context), rect)];
    fill_device_polygons(env, context, &polygons, false, None);
}

fn CGContextStrokeEllipseInRect(env: &mut Environment, context: CGContextRef, rect: CGRect) {
    if context.is_null() {
        return;
    }
    let subpaths = [SubPath {
        points: ellipse_device_points(current_transform(env, context), rect),
        closed: true,
    }];
    stroke_subpaths(env, context, &subpaths);
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CGContextRetain(_)),
    export_c_func!(CGContextRelease(_)),
    export_c_func!(CGContextSetFillColorWithColor(_, _)),
    export_c_func!(CGContextSetRGBFillColor(_, _, _, _, _)),
    export_c_func!(CGContextSetGrayFillColor(_, _, _)),
    export_c_func!(CGContextFillRect(_, _)),
    export_c_func!(CGContextClearRect(_, _)),
    export_c_func!(CGContextClipToRect(_, _)),
    export_c_func!(CGContextConcatCTM(_, _)),
    export_c_func!(CGContextGetCTM(_)),
    export_c_func!(CGContextRotateCTM(_, _)),
    export_c_func!(CGContextScaleCTM(_, _, _)),
    export_c_func!(CGContextTranslateCTM(_, _, _)),
    export_c_func!(CGContextDrawImage(_, _, _)),
    export_c_func!(CGContextSaveGState(_)),
    export_c_func!(CGContextRestoreGState(_)),
    export_c_func!(CGContextSetInterpolationQuality(_, _)),
    export_c_func!(CGContextSetAllowsAntialiasing(_, _)),
    export_c_func!(CGContextSetFont(_, _)),
    export_c_func!(CGContextSetFontSize(_, _)),
    export_c_func!(CGContextSetTextDrawingMode(_, _)),
    export_c_func!(CGContextShowGlyphsAtPoint(_, _, _, _, _)),
    // [深扫修 2026-09-11] #23(b):路径 API
    export_c_func!(CGContextBeginPath(_)),
    // [审查修 2026-09-13] E21:裁剪(最小版:只清当前路径,不做真正裁剪)
    export_c_func!(CGContextClip(_)),
    export_c_func!(CGContextEOClip(_)),
    export_c_func!(CGContextMoveToPoint(_, _, _)),
    export_c_func!(CGContextAddLineToPoint(_, _, _)),
    export_c_func!(CGContextAddArc(_, _, _, _, _, _, _)),
    export_c_func!(CGContextAddArcToPoint(_, _, _, _, _, _)),
    export_c_func!(CGContextAddRect(_, _)),
    export_c_func!(CGContextClosePath(_)),
    export_c_func!(CGContextFillPath(_)),
    export_c_func!(CGContextEOFillPath(_)),
    export_c_func!(CGContextStrokePath(_)),
    export_c_func!(CGContextDrawPath(_, _)),
    export_c_func!(CGContextSetLineWidth(_, _)),
    export_c_func!(CGContextSetRGBStrokeColor(_, _, _, _, _)),
    export_c_func!(CGContextSetGrayStrokeColor(_, _, _)),
    export_c_func!(CGContextSetStrokeColorWithColor(_, _)),
    export_c_func!(CGContextStrokeRect(_, _)),
    export_c_func!(CGContextFillEllipseInRect(_, _)),
    export_c_func!(CGContextStrokeEllipseInRect(_, _)),
];
