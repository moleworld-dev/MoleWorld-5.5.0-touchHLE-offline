/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The implementation of layer compositing.
//!
//! This is completely original; I don't think Apple document how this works and
//! I haven't attempted to reverse-engineer the details. As such, it probably
//! diverges wildly from what the real iPhone OS does.
#![allow(clippy::zero_ptr)] // alas, as you know, opengl

use super::ca_eagl_layer::find_fullscreen_eagl_layer;
use super::ca_layer::CALayerHostObject;
use crate::frameworks::core_animation::animation;
use crate::frameworks::core_graphics::cg_color::CGColorHostObject;
use crate::frameworks::core_graphics::{cg_bitmap_context, cg_image, CGFloat, CGRect};
use crate::gles::gles11_raw as gles11; // constants only
use crate::gles::gles11_raw::types::*;
use crate::gles::present::{present_frame, FpsCounter};
use crate::gles::GLES; // constants only
use crate::image::Image;
use crate::matrix::Matrix;
use crate::mem::SafeWrite;
use crate::objc::{id, msg, msg_class, nil, release, ObjC};
use crate::Environment;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct State {
    texture_framebuffer: Option<(GLuint, GLuint)>,
    recomposite_next: Option<Instant>,
    fps_counter: Option<FpsCounter>,
    misc_gl_objects: Option<MiscGlObjects>,
    /// [2026-10-06 第十轮 R10-A2] 快路径浮层纹理里可能还有不透明内容的区域(GL 坐标 x0, y0, x1, y1,
    /// 左闭右开、自下而上),即上一次浮层合成画到的范围。下一次读回要连它一起读,旧位置才会被擦成透明。
    overlay_dirty: Option<(u32, u32, u32, u32)>,
    /// [2026-10-06 第十轮 R10-A2] 上一趟浮层合成画完时浮层状态的签名(见 overlay_signature)。没变就不再重画、
    /// 不再读回上传,快路径继续叠加已有的浮层纹理;停止叠加时清空,浮层再出现时必定重画。
    overlay_signature: Option<u64>,
}

/// [2026-10-06 第十轮 R10-A2] 只画「找全屏层时被跳过的小浮层」的一趟合成:targets 是那几个浮层,
/// bbox 记下实际画到的屏幕范围(点坐标 min_x, min_y, max_x, max_y),只读回这一块。
struct OverlayPass {
    targets: Vec<id>,
    bbox: Option<[f32; 4]>,
}

impl OverlayPass {
    /// 把单位四边形经 modelview 变换后的四个角并进 bbox。
    fn include(&mut self, modelview: &Matrix<4>) {
        for (x, y) in [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)] {
            let [px, py, _, w] = modelview.transform([x, y, 0.0, 1.0]);
            let (px, py) = if w != 0.0 && w != 1.0 { (px / w, py / w) } else { (px, py) };
            if !px.is_finite() || !py.is_finite() {
                continue;
            }
            let b = self.bbox.get_or_insert([px, py, px, py]);
            b[0] = b[0].min(px);
            b[1] = b[1].min(py);
            b[2] = b[2].max(px);
            b[3] = b[3].max(py);
        }
    }
}

/// [2026-10-06 第十轮 R10-A2] 快路径浮层的状态签名:浮层本身及其上级链的几何、显隐、不透明度,浮层子树里每层的
/// 几何、显隐、不透明度、底色、圆角、待重绘标志、位图来源与「纹理是否已是最新」,加上画布尺寸。浮层不变时
/// (绝大多数帧)签名相同,整趟浮层合成可以跳过——否则每秒 60 趟合成+读回+上传会把无 JIT 的帧率拖低。
/// 有动画在跑时返回 None(每趟都要重画)。须在 display_layers 之后算(重绘会把「纹理已是最新」清掉)。
fn overlay_signature(objc: &ObjC, targets: &[id], canvas: (u32, u32)) -> Option<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    fn hash_geometry(host: &CALayerHostObject, h: &mut DefaultHasher) {
        let t = host.affine_transform;
        for v in [
            host.bounds.origin.x,
            host.bounds.origin.y,
            host.bounds.size.width,
            host.bounds.size.height,
            host.position.x,
            host.position.y,
            host.anchor_point.x,
            host.anchor_point.y,
            t.a,
            t.b,
            t.c,
            t.d,
            t.tx,
            t.ty,
            host.opacity,
        ] {
            v.to_bits().hash(h);
        }
        host.hidden.hash(h);
    }
    fn is_animating(host: &CALayerHostObject) -> bool {
        !host.animations.is_empty() || !host.anonymous_animations.is_empty()
    }
    fn hash_subtree(objc: &ObjC, layer: id, h: &mut DefaultHasher) -> bool {
        let host = objc.borrow::<CALayerHostObject>(layer);
        if is_animating(host) {
            return false;
        }
        layer.hash(h);
        hash_geometry(host, h);
        if host.hidden {
            return true;
        }
        if let Some(c) = host.background_color {
            for v in [c.r, c.g, c.b, c.a] {
                v.to_bits().hash(h);
            }
        }
        host.corner_radius.to_bits().hash(h);
        host.needs_display.hash(h);
        host.contents.hash(h);
        host.cg_context.is_some().hash(h);
        host.presented_pixels.is_some().hash(h);
        host.gles_texture_is_up_to_date.hash(h);
        host.sublayers.len().hash(h);
        host.sublayers
            .iter()
            .all(|&child| hash_subtree(objc, child, h))
    }
    let mut h = DefaultHasher::new();
    canvas.hash(&mut h);
    for &target in targets {
        if !hash_subtree(objc, target, &mut h) {
            return None;
        }
        let mut ancestor = objc.borrow::<CALayerHostObject>(target).superlayer();
        while ancestor != nil {
            let host = objc.borrow::<CALayerHostObject>(ancestor);
            if is_animating(host) {
                return None;
            }
            ancestor.hash(&mut h);
            hash_geometry(host, &mut h);
            ancestor = host.superlayer();
        }
    }
    Some(h.finish())
}

/// [2026-10-06 第十轮 R10-A2] 本次认出全屏层时被跳过的小浮层(只有 iOS 会跳过)。
/// 设环境变量 MOLE_FASTPATH_OVERLAY=0 可关掉叠加,退回旧行为(被跳过的浮层不画),用于对比帧率或排查。
#[cfg(target_os = "ios")]
fn fastpath_overlays() -> Vec<id> {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("MOLE_FASTPATH_OVERLAY").map_or(true, |v| v != "0")) {
        return Vec::new();
    }
    super::ca_eagl_layer::skipped_overlays()
}
#[cfg(not(target_os = "ios"))]
fn fastpath_overlays() -> Vec<id> {
    Vec::new()
}

/// [2026-10-06 第十轮 R10-A2] 交给游戏上下文:上传浮层画布区域并设定快路径是否叠加(见 eagl)。
#[cfg(target_os = "ios")]
unsafe fn hand_overlay_to_guest(
    env: &mut Environment,
    canvas: (u32, u32),
    upload: Option<((u32, u32, u32, u32), &[u8])>,
    content: (u32, u32, u32, u32),
    active: bool,
) {
    crate::frameworks::opengles::upload_fastpath_overlay_region(env, canvas, upload, content, active);
}
#[cfg(not(target_os = "ios"))]
unsafe fn hand_overlay_to_guest(
    _env: &mut Environment,
    _canvas: (u32, u32),
    _upload: Option<((u32, u32, u32, u32), &[u8])>,
    _content: (u32, u32, u32, u32),
    _active: bool,
) {
}

fn set_fastpath_overlay_inactive(env: &mut Environment) {
    env.framework_state
        .core_animation
        .composition
        .overlay_signature = None;
    #[cfg(target_os = "ios")]
    crate::frameworks::opengles::set_fastpath_overlay_inactive();
}

struct MiscGlObjects {
    /// Texture containing a single rounded corner.
    rounded_corner_texture: GLuint,
    /// [BASIC_SQUARE_POINTS], used as both vertex and texture co-ords for
    /// drawing simple textured quads.
    basic_square_buffer: GLuint,
    /// [FLIPPED_SQUARE_POINTS], used as texture co-ords for some textured
    /// quads.
    flipped_square_buffer: GLuint,
    /// 9-patch rounded corner texture co-ords (always the same).
    rounded_vertex_buffer: GLuint,
    /// 9-patch rounded corner vertex co-ords (varies with ratio of corner
    /// radius to overall rectangle size).
    rounded_tex_coord_buffer: GLuint,
    /// Index buffer for 9-patch (first 6 elements can be used for square).
    index_buffer: GLuint,
}

unsafe fn load_matrix(gles: &mut dyn GLES, matrix: Matrix<4>) {
    gles.LoadMatrixf(matrix.columns().as_ptr() as *const _);
}

/// For use by `NSRunLoop`: call this 60 times per second. Composites the app's
/// visible layers (i.e. UI) and presents it to the screen. Does nothing if
/// composition isn't in use or it's too soon (the latter check is skipped if
/// `force` is set to [true]).
///
/// Returns the time a recomposite is due, if any.
pub fn recomposite_if_necessary(env: &mut Environment, force: bool) -> Option<Instant> {
    // [MoleWorld iOS] No GL while truly backgrounded (iOS kills GPU-in-background).
    if env.window.as_ref().map_or(false, |w| w.is_backgrounded()) {
        return None;
    }
    // [深扫修 2026-09-11] #23(a):合成前先布局 setNeedsLayout 打过脏标记的视图
    // (MBProgressHUD 的底框宽高/指示器居中都在 layoutSubviews 里算),必须在
    // display_layers(drawRect:)之前。放在函数最前面:① 早于下面对 windows 的
    // 克隆,避免 guest 的 layoutSubviews 改动窗口列表后用到过期列表;② 早于全屏
    // EAGL 快路径判断,因为布局可能增删子视图从而改变能否走快路径。
    // 本调用由 NSRunLoop 发起,不在游戏 drawScene 帧栈内;无脏视图时只是一次计数判断。
    crate::frameworks::uikit::ui_view::layout_dirty_views_before_composition(env);

    let mut animation_state = animation::State::default();
    let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
    if !windows.iter().any(|&window| !msg![env; window isHidden]) {
        log_dbg!("No visible windows, skipping composition");
        return None;
    }

    // [2026-10-06 第十轮 R10-A2] iOS 认全屏层时会跳过未聚焦的小浮层(无 JIT 必须留在快路径保帧率),原先这些浮层
    // 整段不画:公告板正文(UITextView)、好友村搜索框都看不见,与原版不符。现在快路径下照样走到这里,但只把这几个
    // 浮层画进一张透明画布、只读回画到的那一块,交给游戏上下文,快路径出帧时叠加上去(eagl::draw_overlay_texture)。
    // 整个过程在运行循环里做,不在游戏 drawScene 帧栈上。桌面/安卓不跳过浮层,行为不变。
    let mut overlay_pass: Option<OverlayPass> = if find_fullscreen_eagl_layer(env) != nil {
        let targets = fastpath_overlays();
        if targets.is_empty() {
            set_fastpath_overlay_inactive(env);
            // No composition done, EAGLContext will present directly.
            log_dbg!("Using CAEAGLLayer fast path, skipping composition");
            return None;
        }
        Some(OverlayPass {
            targets,
            bbox: None,
        })
    } else {
        set_fastpath_overlay_inactive(env);
        None
    };

    if env.options.print_fps && overlay_pass.is_none() {
        env.framework_state
            .core_animation
            .composition
            .fps_counter
            .get_or_insert_with(FpsCounter::start)
            .count_frame(format_args!("Core Animation compositor"));
    }

    let now = Instant::now();
    let interval = 1.0 / 60.0; // 60Hz
    let new_recomposite_next = if let Some(recomposite_next) = env
        .framework_state
        .core_animation
        .composition
        .recomposite_next
    {
        if !force && recomposite_next > now {
            log_dbg!("Not recompositing yet, wait {:?}", recomposite_next - now);
            return Some(recomposite_next);
        }

        // See NSTimer implementation for a discussion of what this does.
        let overdue_by = now.duration_since(recomposite_next);
        log_dbg!("Recompositing, overdue by {:?}", overdue_by);
        // TODO: Use `.div_duration_f64()` once that is stabilized.
        let advance_by = (overdue_by.as_secs_f64() / interval).max(1.0).ceil();
        assert!(advance_by == (advance_by as u32) as f64);
        let advance_by = advance_by as u32;
        if advance_by > 1 {
            log_dbg!("Warning: compositor is lagging. It is overdue by {}s and has missed {} interval(s)!", overdue_by.as_secs_f64(), advance_by - 1);
        }
        let advance_by = Duration::from_secs_f64(interval)
            .checked_mul(advance_by)
            .unwrap();
        Some(recomposite_next.checked_add(advance_by).unwrap())
    } else {
        Some(now.checked_add(Duration::from_secs_f64(interval)).unwrap())
    };
    env.framework_state
        .core_animation
        .composition
        .recomposite_next = new_recomposite_next;

    let window_layers: Vec<id> = windows
        .into_iter()
        .map(|window| {
            let layer: id = msg![env; window layer];
            // Ensure layer bitmaps are up to date.
            // [2026-10-06 第十轮 R10-A2] 浮层合成只画浮层,只需更新浮层子树的位图(见下)。整棵树都 display
            // 会把游戏自身每帧标脏的层(例如全屏 EAGL 视图)也重绘一遍,快路径原本从不做这件事,
            // 实测好友村帧率从 24.5 掉到 15。
            if overlay_pass.is_none() {
                display_layers(env, layer);
            }
            layer
        })
        .collect();
    if let Some(pass) = &overlay_pass {
        for target in pass.targets.clone() {
            display_layers(env, target);
        }
    }

    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };
    let scale_hack: u32 = env.options.scale_hack.get();
    let fb_width = screen_bounds.size.width as u32 * scale_hack;
    let fb_height = screen_bounds.size.height as u32 * scale_hack;

    // [2026-10-06 第十轮 R10-A2] 浮层没有任何变化:快路径继续叠加已有的浮层纹理,这一趟什么都不做。
    if let Some(pass) = &overlay_pass {
        let signature = overlay_signature(&env.objc, &pass.targets, (fb_width, fb_height));
        if signature.is_some()
            && signature
                == env
                    .framework_state
                    .core_animation
                    .composition
                    .overlay_signature
        {
            return new_recomposite_next;
        }
    }
    let present_frame_args = (
        env.window().viewport(),
        env.window().rotation_matrix(),
        env.window().virtual_cursor_visible_at(),
        env.window().drawable_size(), // [MoleWorld] full_size,供 --ambient-fill
    );
    // [MoleWorld iOS] 窗口真实默认 framebuffer(桌面/安卓=0),传给 present_frame 绑定;
    // 以及 viewRenderbuffer,swap 前绑回 GL_RENDERBUFFER。
    let window_default_fbo = env.window().default_framebuffer();
    // [补完 2026-09-15] window_default_rbo 只在下方 #[cfg(target_os = "ios")] 的 BindRenderbufferOES 用到,
    // 桌面/安卓构建报 unused_variables。只在非 iOS 放宽该 lint,取值与调用照旧(不把这行门控掉,
    // 免得 default_renderbuffer() 在其它调用点也门控后变成 dead_code),各平台行为不变。
    #[cfg_attr(not(target_os = "ios"), allow(unused_variables))]
    let window_default_rbo = env.window().default_renderbuffer();

    // TODO: draw status bar if it's not hidden

    // Initial state for layer tree traversal (see composite_layer_recursive)
    let cumulative_transform = Matrix::<4>::identity();
    let opacity = 1.0;

    let window = env.window.as_mut().unwrap();
    let mut gles = window.make_internal_gl_ctx_current();

    // Set up GL objects needed for render-to-texture. We could draw directly
    // to the screen instead, but this way we can reuse the code for scaling and
    // rotating the screen and drawing the virtual cursor.
    let texture = if let Some((texture, framebuffer)) = env
        .framework_state
        .core_animation
        .composition
        .texture_framebuffer
    {
        unsafe {
            gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, framebuffer);
        };
        texture
    } else {
        let mut texture = 0;
        let mut framebuffer = 0;
        unsafe {
            gles.GenTextures(1, &mut texture);
            gles.BindTexture(gles11::TEXTURE_2D, texture);
            gles.TexImage2D(
                gles11::TEXTURE_2D,
                0,
                gles11::RGBA as _,
                fb_width as _,
                fb_height as _,
                0,
                gles11::RGBA,
                gles11::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gles.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MIN_FILTER,
                gles11::LINEAR as _,
            );
            gles.TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_MAG_FILTER,
                gles11::LINEAR as _,
            );
            // [MoleWorld iOS] This compositor render-target texture is NPOT
            // (screen-sized). iOS native GLES1 only allows NPOT textures with
            // CLAMP_TO_EDGE wrap + non-mipmap filter; with the default GL_REPEAT
            // the texture is INCOMPLETE, so when present_frame samples it the
            // draw silently behaves as if texturing were disabled → a solid
            // white quad (no glError). Desktop GL2 tolerates NPOT+REPEAT, which
            // is why this only broke on the device. CLAMP is also semantically
            // correct here (we sample [0,1] exactly).
            // [MoleWorld] CLAMP only on iOS (native GLES1 needs it for NPOT completeness).
            // On Mac present_frame rotates texcoords via the TEXTURE matrix, sending them
            // outside [0,1] where the default REPEAT wraps correctly but CLAMP_TO_EDGE
            // smears the frame into vertical bands (the "撕裂" regression). Mac keeps REPEAT.
            #[cfg(target_os = "ios")]
            {
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_S,
                    gles11::CLAMP_TO_EDGE as _,
                );
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_T,
                    gles11::CLAMP_TO_EDGE as _,
                );
            }

            gles.GenFramebuffersOES(1, &mut framebuffer);
            gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, framebuffer);
            gles.FramebufferTexture2DOES(
                gles11::FRAMEBUFFER_OES,
                gles11::COLOR_ATTACHMENT0_OES,
                gles11::TEXTURE_2D,
                texture,
                0,
            );
            assert_eq!(gles.GetError(), 0);
            assert_eq!(
                gles.CheckFramebufferStatusOES(gles11::FRAMEBUFFER_OES),
                gles11::FRAMEBUFFER_COMPLETE_OES
            );
        }
        env.framework_state
            .core_animation
            .composition
            .texture_framebuffer = Some((texture, framebuffer));
        texture
    };

    // Set up various other GL objects that will be reused on every frame.
    let misc_gl_objects = env
        .framework_state
        .core_animation
        .composition
        .misc_gl_objects
        .get_or_insert_with(|| {
            let dimension = 512usize; // way larger than any reasonable corner
            let mut image = Image::from_pixel_vec(
                vec![255u8; dimension * dimension * 4],
                (dimension as _, dimension as _),
            );
            image.round_corners(dimension as _, /* four_corners: */ false, /* add_sheen: */ false);

            let mut rounded_corner_texture = 0;
            unsafe {
                gles.GenTextures(1, &mut rounded_corner_texture);
                gles.BindTexture(gles11::TEXTURE_2D, rounded_corner_texture);
                // GENERATE_MIPMAP must be set before the texture upload.
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::GENERATE_MIPMAP,
                    gles11::TRUE as _,
                );
                upload_rgba8_pixels(gles.as_mut(), image.pixels(), (dimension as _, dimension as _));
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_MIN_FILTER,
                    gles11::LINEAR_MIPMAP_LINEAR as _,
                );
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_S,
                    gles11::CLAMP_TO_EDGE as _,
                );
                gles.TexParameteri(
                    gles11::TEXTURE_2D,
                    gles11::TEXTURE_WRAP_T,
                    gles11::CLAMP_TO_EDGE as _,
                );
            }

            let [basic_square_buffer, flipped_square_buffer, rounded_vertex_buffer, rounded_tex_coord_buffer, index_buffer] = unsafe {
                let mut array_buffers = [0; 5];
                gles.GenBuffers(5, array_buffers.as_mut_ptr());
                array_buffers
            };
            unsafe {
                gles.BindBuffer(gles11::ARRAY_BUFFER, basic_square_buffer);
                upload_slice(gles.as_mut(), gles11::ARRAY_BUFFER, &BASIC_SQUARE_POINTS, gles11::STATIC_DRAW);
                gles.BindBuffer(gles11::ARRAY_BUFFER, flipped_square_buffer);
                upload_slice(gles.as_mut(), gles11::ARRAY_BUFFER, &FLIPPED_SQUARE_POINTS, gles11::STATIC_DRAW);
                gles.BindBuffer(gles11::ARRAY_BUFFER, rounded_vertex_buffer);
                upload_slice(gles.as_mut(), gles11::ARRAY_BUFFER, &[0f32; FLOATS_PER_9PATCH], gles11::DYNAMIC_DRAW);
                gles.BindBuffer(gles11::ARRAY_BUFFER, rounded_tex_coord_buffer);
                upload_slice(
                    gles.as_mut(),
                    gles11::ARRAY_BUFFER,
                    &make_9patch_coords([0.0, 1.0, 1.0, 0.0], [0.0, 1.0, 1.0, 0.0]),
                    gles11::STATIC_DRAW,
                );
                // Prevent accidental subsequent use.
                gles.BindBuffer(gles11::ARRAY_BUFFER, 0);

                gles.BindBuffer(gles11::ELEMENT_ARRAY_BUFFER, index_buffer);
                upload_slice(gles.as_mut(), gles11::ELEMENT_ARRAY_BUFFER, &make_9patch_indices(), gles11::STATIC_DRAW);
                // Prevent accidental subsequent use.
                gles.BindBuffer(gles11::ELEMENT_ARRAY_BUFFER, 0);
            }

            MiscGlObjects {
                rounded_corner_texture,
                basic_square_buffer,
                flipped_square_buffer,
                rounded_vertex_buffer,
                rounded_tex_coord_buffer,
                index_buffer,
            }
        });

    // Clear the framebuffer and set up state to prepare for rendering
    unsafe {
        gles.Viewport(0, 0, fb_width as _, fb_height as _);
        // 浮层画布底色全透明(叠加到游戏画面上),完整合成照旧黑底。
        if overlay_pass.is_some() {
            gles.ClearColor(0.0, 0.0, 0.0, 0.0);
        } else {
            gles.ClearColor(0.0, 0.0, 0.0, 1.0);
        }
        gles.Clear(gles11::COLOR_BUFFER_BIT);
        gles.Color4f(1.0, 1.0, 1.0, 1.0);

        gles.MatrixMode(gles11::PROJECTION);
        // Scale down screen-space to normalized device co-ordinates, shift the
        // origin to be at the top-left rather than the center, and flip the
        // Y axis (OpenGL's points up, Core Animation's points down).
        // Using the projection matrix for this is more convenient than adding
        // an extra multiply to composite_layer_recursive.
        load_matrix(
            gles.as_mut(),
            Matrix::from(&Matrix::scale_2d(
                2.0 / screen_bounds.size.width,
                -2.0 / screen_bounds.size.height,
            ))
            .multiply(&Matrix::translate_3d(-1.0, 1.0, 0.0)),
        );
        gles.MatrixMode(gles11::MODELVIEW);
        gles.LoadIdentity();

        // One index buffer to rule them all
        gles.BindBuffer(gles11::ELEMENT_ARRAY_BUFFER, misc_gl_objects.index_buffer);
    }
    std::mem::drop(gles);

    // Assumes the windows in the list are ordered back-to-front.
    // TODO: this may not be correct once we support windowLevel.
    for root_layer in window_layers {
        // Here's where the actual drawing happens
        unsafe {
            composite_layer_recursive(
                env,
                &mut animation_state,
                root_layer,
                cumulative_transform,
                opacity,
                &mut overlay_pass,
                false,
            );
        }
    }

    // Re-borrow
    let window = env.window.as_mut().unwrap();
    let mut gles = window.make_internal_gl_ctx_current();

    // Clean up some GL state
    unsafe {
        gles.Viewport(0, 0, fb_width as _, fb_height as _);
        gles.Color4f(1.0, 1.0, 1.0, 1.0);
        gles.Disable(gles11::BLEND);
        gles.MatrixMode(gles11::PROJECTION);
        gles.LoadIdentity();
        gles.MatrixMode(gles11::MODELVIEW);
        gles.LoadIdentity();
        gles.BindBuffer(gles11::ARRAY_BUFFER, 0);
        gles.BindBuffer(gles11::ELEMENT_ARRAY_BUFFER, 0);
        assert_eq!(gles.GetError(), 0);
    }

    if let Some(pass) = overlay_pass {
        // [2026-10-06 第十轮 R10-A2] 浮层合成:读回「这次画到的范围 ∪ 上次画到的范围」(后者擦掉旧位置),
        // 交给游戏上下文上传进浮层纹理。什么都没画(浮层全隐藏)就停止叠加,脏区留到下次一并擦。
        let scale = scale_hack as f32;
        let current = pass.bbox.and_then(|[min_x, min_y, max_x, max_y]| {
            let x0 = ((min_x * scale).floor() - 1.0).clamp(0.0, fb_width as f32) as u32;
            let x1 = ((max_x * scale).ceil() + 1.0).clamp(0.0, fb_width as f32) as u32;
            let y0 = ((min_y * scale).floor() - 1.0).clamp(0.0, fb_height as f32) as u32;
            let y1 = ((max_y * scale).ceil() + 1.0).clamp(0.0, fb_height as f32) as u32;
            // 画布 y 向下,GL 行自下而上。
            (x1 > x0 && y1 > y0).then_some((x0, fb_height - y1, x1, fb_height - y0))
        });
        let composition_state = &mut env.framework_state.core_animation.composition;
        let readback = current.map(|cur| {
            let region = match composition_state.overlay_dirty {
                Some((dx0, dy0, dx1, dy1)) => (
                    cur.0.min(dx0.min(fb_width)),
                    cur.1.min(dy0.min(fb_height)),
                    cur.2.max(dx1.min(fb_width)),
                    cur.3.max(dy1.min(fb_height)),
                ),
                None => cur,
            };
            composition_state.overlay_dirty = Some(cur);
            let (x0, y0, x1, y1) = region;
            let (width, height) = (x1 - x0, y1 - y0);
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            unsafe {
                gles.PixelStorei(gles11::PACK_ALIGNMENT, 4);
                gles.ReadPixels(
                    x0 as _,
                    y0 as _,
                    width as _,
                    height as _,
                    gles11::RGBA,
                    gles11::UNSIGNED_BYTE,
                    pixels.as_mut_ptr() as *mut _,
                );
            }
            ((x0, y0, width, height), pixels)
        });
        std::mem::drop(gles);
        let signature = overlay_signature(&env.objc, &pass.targets, (fb_width, fb_height));
        env.framework_state
            .core_animation
            .composition
            .overlay_signature = signature;
        unsafe {
            hand_overlay_to_guest(
                env,
                (fb_width, fb_height),
                readback
                    .as_ref()
                    .map(|(region, pixels)| (*region, pixels.as_slice())),
                current.unwrap_or((0, 0, 0, 0)),
                readback.is_some(),
            );
        }
    } else {
        // [2026-10-06] iOS:合成画布改在【游戏 EAGL 上下文的视图】里呈现。SDL 在 iOS 上给每个 GL 上下文各配一个视图、
        // 设为当前时挂到窗口上;在内部上下文里换帧的画面,下一帧游戏切回自己的上下文就被换掉,弹框等覆盖层永远看不到。
        // 内部上下文与游戏上下文不共享纹理,所以读回画布像素交给游戏上下文上传后呈现,
        // 见 eagl::present_composited_pixels_in_guest_view。
        #[cfg(target_os = "ios")]
        let presented_in_guest_view = {
            // 画布仍绑在合成 FBO 上:读回整张画布像素(RGBA,自下而上的行序与纹理一致)。
            let mut pixels = vec![0u8; (fb_width * fb_height * 4) as usize];
            unsafe {
                gles.PixelStorei(gles11::PACK_ALIGNMENT, 4);
                gles.ReadPixels(
                    0,
                    0,
                    fb_width as _,
                    fb_height as _,
                    gles11::RGBA,
                    gles11::UNSIGNED_BYTE,
                    pixels.as_mut_ptr() as *mut _,
                );
            }
            std::mem::drop(gles);
            unsafe {
                crate::frameworks::opengles::present_composited_pixels_in_guest_view(
                    env,
                    &pixels,
                    fb_width,
                    fb_height,
                    present_frame_args.1,
                )
            }
        };
        #[cfg(not(target_os = "ios"))]
        let presented_in_guest_view = {
            std::mem::drop(gles);
            false
        };
        if !presented_in_guest_view {
            let window = env.window.as_mut().unwrap();
            let mut gles = window.make_internal_gl_ctx_current();
            // Present our rendered frame (bound to TEXTURE_2D). present_frame binds the
            // window's default framebuffer (0 on desktop/Android, the CAEAGLLayer FBO on
            // iOS) before drawing, so we no longer hardcode-bind framebuffer 0 here.
            unsafe {
                gles.BindTexture(gles11::TEXTURE_2D, texture);
                present_frame(
                    gles.as_mut(),
                    present_frame_args.0,
                    present_frame_args.3, // full_size
                    present_frame_args.1,
                    present_frame_args.2,
                    window_default_fbo,
                );
                // [MoleWorld iOS] swap 前把 viewRenderbuffer 绑回 GL_RENDERBUFFER(SDL presentRenderbuffer
                // 契约:呈现当前绑定的 renderbuffer;present_frame 期间可能绑了别的)。
                #[cfg(target_os = "ios")]
                gles.BindRenderbufferOES(gles11::RENDERBUFFER_OES, window_default_rbo);
            }
            std::mem::drop(gles);
            window.swap_window();
        }
    }

    // [同步上游 0.3.0 2026-10-03] 动画委托回调外包一个自动释放池。UIView 旧式动画改走
    // 上游 CATransaction 实现后,这里第一次真正回调 animationDidStart:/animationDidStop:finished:
    // (中转委托 _touchHLE_UIView_AnimationDelegate 会 numberWithBool:,游戏回调如 MBProgressHUD
    // animationFinished:finished:context: → done 也会 autorelease)。本函数由 NSRunLoop 直接调用,
    // 外面没有池,不包的话这些对象都落进 main() 最外层永不排空的池,每次 HUD 隐藏都漏一点。
    // 写法与 ui_application.rs 等处宿主回调一致。
    let pool: id = msg_class![env; NSAutoreleasePool new];
    animation_state.update_started_and_finished_animations(env);
    release(env, pool);

    new_recomposite_next
}

/// Call `displayIfNeeded` on all relevant layers in the tree, so their bitmaps
/// are up to date before compositing.
fn display_layers(env: &mut Environment, root_layer: id) {
    // Tell layers to redraw themselves if needed.

    fn traverse(objc: &ObjC, layer: id, layers_needing_display: &mut Vec<id>) {
        let host_obj = objc.borrow::<CALayerHostObject>(layer);
        if host_obj.hidden {
            return;
        }
        if host_obj.needs_display {
            layers_needing_display.push(layer);
        }
        for &layer in &host_obj.sublayers {
            traverse(objc, layer, layers_needing_display);
        }
    }

    let mut layers_needing_display = Vec::new();
    traverse(&env.objc, root_layer, &mut layers_needing_display);

    for layer in layers_needing_display {
        () = msg![env; layer displayIfNeeded];
    }
}

/// Traverses the layer tree and draws each layer.
unsafe fn composite_layer_recursive(
    env: &mut Environment,
    animation_state: &mut animation::State,
    layer: id,
    cumulative_transform: Matrix<4>,
    opacity: CGFloat,
    // [2026-10-06 第十轮 R10-A2] Some = 浮层合成:只画 targets 里的浮层及其子层,其余层只往下走、不画。
    overlay: &mut Option<OverlayPass>,
    // 浮层合成时:祖先里已有目标浮层(本层要画)。
    inside_overlay: bool,
) {
    // TODO: this can't handle zPosition among other things, but it is not
    //       supported yet :)
    // TODO: back-to-front drawing is not efficient, could we use front-to-back?

    // This is both acting as the presentationLayer and the private render layer
    // It might need to be reworked in the future into a guest presentationLayer
    let host_obj = animation_state.create_presentation_layer(env, layer);

    if host_obj.hidden {
        return;
    }

    let window = env.window.as_mut().unwrap();
    let mut gles = window.make_internal_gl_ctx_current();

    let draw_self = match overlay {
        None => true,
        Some(pass) => inside_overlay || pass.targets.contains(&layer),
    };

    let opacity = opacity * host_obj.opacity;
    let (cumulative_transform, modelview) = {
        let CALayerHostObject { bounds, .. } = host_obj;

        // Update the transform to match this layer's co-ordinate space.
        let cumulative_transform =
            <Matrix<4> as From<_>>::from(host_obj.superlayer_to_layer_transform())
                .multiply(&cumulative_transform);

        // Reposition and scale the unit quad (see ARRAY_BUFFER binding)
        // so it will have the right size in this layer's co-ordinate space.
        gles.MatrixMode(gles11::MODELVIEW);
        let modelview =
            Matrix::<4>::from(&Matrix::scale_2d(bounds.size.width, bounds.size.height))
                .multiply(&Matrix::translate_3d(bounds.origin.x, bounds.origin.y, 0.0))
                .multiply(&cumulative_transform);
        load_matrix(gles.as_mut(), modelview);

        (cumulative_transform, modelview)
    };

    // Draw background color, if any
    let have_background = if let (true, Some(background_color)) = (draw_self, host_obj.background_color) {
        let misc = env
            .framework_state
            .core_animation
            .composition
            .misc_gl_objects
            .as_ref()
            .unwrap();

        let CGColorHostObject { r, g, b, a, .. } = background_color;
        gles.Color4f(
            r * a * opacity,
            g * a * opacity,
            b * a * opacity,
            a * opacity,
        );
        gles.Enable(gles11::BLEND);
        gles.BlendFunc(gles11::ONE, gles11::ONE_MINUS_SRC_ALPHA);

        let radius = host_obj.corner_radius;
        if radius == 0.0 {
            gles.Disable(gles11::TEXTURE_2D);
            gles.DisableClientState(gles11::TEXTURE_COORD_ARRAY);

            gles.EnableClientState(gles11::VERTEX_ARRAY);
            gles.BindBuffer(gles11::ARRAY_BUFFER, misc.basic_square_buffer);
            gles.VertexPointer(2, gles11::FLOAT, 0, 0 as *const GLvoid);

            gles.DrawElements(
                gles11::TRIANGLES,
                SQUARE_INDICES.len() as _,
                gles11::UNSIGNED_BYTE,
                0 as *const GLvoid,
            );
        } else {
            gles.Enable(gles11::TEXTURE_2D);
            gles.BindTexture(gles11::TEXTURE_2D, misc.rounded_corner_texture);
            gles.EnableClientState(gles11::TEXTURE_COORD_ARRAY);
            gles.BindBuffer(gles11::ARRAY_BUFFER, misc.rounded_tex_coord_buffer);
            gles.TexCoordPointer(2, gles11::FLOAT, 0, 0 as *const GLvoid);

            gles.EnableClientState(gles11::VERTEX_ARRAY);
            gles.BindBuffer(gles11::ARRAY_BUFFER, misc.rounded_vertex_buffer);
            upload_slice(
                gles.as_mut(),
                gles11::ARRAY_BUFFER,
                &make_9patch_coords(
                    [
                        0.0,
                        (radius / host_obj.bounds.size.width).min(0.5),
                        (1.0 - radius / host_obj.bounds.size.width).max(0.5),
                        1.0,
                    ],
                    [
                        0.0,
                        (radius / host_obj.bounds.size.height).min(0.5),
                        (1.0 - radius / host_obj.bounds.size.height).max(0.5),
                        1.0,
                    ],
                ),
                gles11::DYNAMIC_DRAW,
            );
            gles.VertexPointer(2, gles11::FLOAT, 0, 0 as *const GLvoid);

            gles.DrawElements(
                gles11::TRIANGLES,
                INDICES_PER_9PATCH as _,
                gles11::UNSIGNED_BYTE,
                0 as *const GLvoid,
            );
        };

        true
    } else {
        false
    };

    let need_texture = draw_self
        && (host_obj.presented_pixels.is_some()
            || host_obj.contents != nil
            || host_obj.cg_context.is_some());
    let need_update = need_texture && !host_obj.gles_texture_is_up_to_date;

    if need_texture {
        if let Some(texture) = host_obj.gles_texture {
            gles.BindTexture(gles11::TEXTURE_2D, texture);
        } else {
            assert!(!host_obj.gles_texture_is_up_to_date);
            let mut texture = 0;
            gles.GenTextures(1, &mut texture);
            gles.BindTexture(gles11::TEXTURE_2D, texture);
            // Update original layer texture
            env.objc.borrow_mut::<CALayerHostObject>(layer).gles_texture = Some(texture);
        }
    }

    // Update original layer texture with CAEAGLLayer pixels (slow path), if any
    if need_update {
        let original_host_obj = env.objc.borrow_mut::<CALayerHostObject>(layer);
        if let Some((ref mut pixels, width, height)) = original_host_obj.presented_pixels {
            // The pixels are always RGBA, but if the layer is opaque then the
            // alpha channel is meant to be ignored. glTexImage2D() has no
            // option to ignore it, so let's manually set them to 255.
            if original_host_obj.opaque {
                let mut i = 3;
                while i < pixels.len() {
                    pixels[i] = 255;
                    i += 4;
                }
            }

            upload_rgba8_pixels(gles.as_mut(), pixels, (width, height));
        }
    }

    // Update texture with CGImageRef or CGContextRef pixels, if any
    if need_update {
        if host_obj.contents != nil {
            let image = cg_image::borrow_image(&env.objc, host_obj.contents);

            // No special handling for opacity is needed here: the alpha channel
            // on an image is meaningful and won't be ignored.
            upload_rgba8_pixels(gles.as_mut(), image.pixels(), image.dimensions());
        } else if let Some(cg_context) = host_obj.cg_context {
            // Make sure this is in sync with the code in ca_layer.rs that
            // sets up the context!
            let (width, height, data) = cg_bitmap_context::get_data(&env.objc, cg_context);
            let size = width * height * 4;
            let pixels = env.mem.bytes_at(data.cast(), size);
            upload_rgba8_pixels(gles.as_mut(), pixels, (width, height));
        }
    }

    if need_update {
        // Update original layer field
        env.objc
            .borrow_mut::<CALayerHostObject>(layer)
            .gles_texture_is_up_to_date = true;
    }

    // Draw texture, if any
    if need_texture {
        let misc = env
            .framework_state
            .core_animation
            .composition
            .misc_gl_objects
            .as_ref()
            .unwrap();

        gles.Color4f(opacity, opacity, opacity, opacity);
        if opacity == 1.0 && host_obj.opaque && !have_background {
            gles.Disable(gles11::BLEND);
        } else {
            gles.Enable(gles11::BLEND);
            gles.BlendFunc(gles11::ONE, gles11::ONE_MINUS_SRC_ALPHA);
        }

        gles.EnableClientState(gles11::VERTEX_ARRAY);
        gles.BindBuffer(gles11::ARRAY_BUFFER, misc.basic_square_buffer);
        gles.VertexPointer(2, gles11::FLOAT, 0, 0 as *const GLvoid);

        gles.EnableClientState(gles11::TEXTURE_COORD_ARRAY);
        // Normal images will have top-to-bottom row order, but OpenGL ES
        // expects bottom-to-top, so flip the UVs in that case.
        gles.BindBuffer(
            gles11::ARRAY_BUFFER,
            if host_obj.contents != nil {
                misc.basic_square_buffer
            } else {
                misc.flipped_square_buffer
            },
        );
        gles.TexCoordPointer(2, gles11::FLOAT, 0, 0 as *const GLvoid);
        gles.Enable(gles11::TEXTURE_2D);
        gles.DrawElements(
            gles11::TRIANGLES,
            SQUARE_INDICES.len() as _,
            gles11::UNSIGNED_BYTE,
            0 as *const GLvoid,
        );
    }
    std::mem::drop(gles);

    if have_background || need_texture {
        if let Some(pass) = overlay.as_mut() {
            pass.include(&modelview);
        }
    }

    // avoid holding mutable borrow while recursing
    let original_host_obj = env.objc.borrow_mut::<CALayerHostObject>(layer);
    for &child_layer in &original_host_obj.sublayers.clone() {
        // TODO: clipping/masksToBounds support
        composite_layer_recursive(
            env,
            animation_state,
            child_layer,
            cumulative_transform,
            opacity,
            overlay,
            draw_self,
        )
    }
}

const FLOATS_PER_POINT: usize = 2;
const BASIC_SQUARE_POINTS: [f32; 4 * FLOATS_PER_POINT] = [0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 0.0];
const SQUARE_INDICES: [u8; 6] = [0, 1, 2, 2, 1, 3];
const FLIPPED_SQUARE_POINTS: [f32; 4 * FLOATS_PER_POINT] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0];
const FLOATS_PER_9PATCH: usize = BASIC_SQUARE_POINTS.len() * 3 * 3;
const INDICES_PER_9PATCH: usize = SQUARE_INDICES.len() * 3 * 3;

fn make_9patch_coords(x_edges: [f32; 4], y_edges: [f32; 4]) -> [f32; FLOATS_PER_9PATCH] {
    let mut out_points = [0.0; FLOATS_PER_9PATCH];
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for (i, out_points_chunk) in out_points
        .chunks_exact_mut(BASIC_SQUARE_POINTS.len())
        .enumerate()
    {
        let (x, y) = (i % 3, i / 3);

        for (dst_xy, src_xy) in out_points_chunk
            .chunks_exact_mut(2)
            .zip(BASIC_SQUARE_POINTS.chunks_exact(2))
        {
            let (x1, x2) = (x_edges[x], x_edges[x + 1]);
            let (y1, y2) = (y_edges[y], y_edges[y + 1]);
            dst_xy[0] = x1 + src_xy[0] * (x2 - x1);
            dst_xy[1] = y1 + src_xy[1] * (y2 - y1);
        }
    }
    out_points
}

fn make_9patch_indices() -> [u8; INDICES_PER_9PATCH] {
    let mut out_indices = [0; SQUARE_INDICES.len() * 3 * 3];
    #[allow(clippy::chunks_exact_to_as_chunks)]
    for (i, out_indices_chunk) in out_indices
        .chunks_exact_mut(SQUARE_INDICES.len())
        .enumerate()
    {
        for (out_index, in_index) in out_indices_chunk
            .iter_mut()
            .zip(SQUARE_INDICES.iter().copied())
        {
            *out_index = in_index + i as u8 * (BASIC_SQUARE_POINTS.len() / FLOATS_PER_POINT) as u8;
        }
    }
    out_indices
}

unsafe fn upload_slice<T: SafeWrite>(
    gles: &mut dyn GLES,
    target: GLenum,
    data: &[T],
    usage: GLenum,
) {
    gles.BufferData(
        target,
        std::mem::size_of_val(data) as _,
        data.as_ptr() as *const _,
        usage,
    )
}

unsafe fn upload_rgba8_pixels(gles: &mut dyn GLES, pixels: &[u8], dimensions: (u32, u32)) {
    // [MoleWorld iOS · 诊断] 标记"合成器整屏重传"这条 TexImage2D 来源。用于区分 [NPOT-FIX] 里
    // 有多少来自 host 合成器(应 ≈60Hz 的零头)vs guest 自己 build 发起(主体)。合成器受 60Hz 门控,
    // 一帧内不可能上千次,所以若 [NPOT-FIX] 一帧上千而 [COMP-UP] 很少,证明主体是 guest。
    // [同步 2026-09-24] 只在 iOS 打:这是 iOS 真机排查用的探针,桌面慢合成路径(输入框/弹框)上会刷屏,
    // main 没有这行日志,门控后桌面输出与 main 一致。
    #[cfg(target_os = "ios")]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COMP_UP_N: AtomicU64 = AtomicU64::new(0);
        let n = COMP_UP_N.fetch_add(1, Ordering::Relaxed);
        if n & 0x3f == 0 {
            echo!("[COMP-UP] n={} {}x{}", n, dimensions.0, dimensions.1);
        }
    }
    gles.TexImage2D(
        gles11::TEXTURE_2D,
        0,
        gles11::RGBA as _,
        dimensions.0 as _,
        dimensions.1 as _,
        0,
        gles11::RGBA,
        gles11::UNSIGNED_BYTE,
        pixels.as_ptr() as *const _,
    );
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_MIN_FILTER,
        gles11::LINEAR as _,
    );
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_MAG_FILTER,
        gles11::LINEAR as _,
    );
    // [MoleWorld iOS] Layer/content pixels are NPOT (screen-sized). iOS native GLES1
    // needs CLAMP_TO_EDGE for NPOT completeness; Mac keeps the default REPEAT (see the
    // render-target note above — CLAMP + the Mac present rotation = torn bands).
    #[cfg(target_os = "ios")]
    {
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_WRAP_S,
            gles11::CLAMP_TO_EDGE as _,
        );
        gles.TexParameteri(
            gles11::TEXTURE_2D,
            gles11::TEXTURE_WRAP_T,
            gles11::CLAMP_TO_EDGE as _,
        );
    }
}
