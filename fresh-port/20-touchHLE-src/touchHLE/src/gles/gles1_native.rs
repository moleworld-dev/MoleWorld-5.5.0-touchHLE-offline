/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Passthrough for a native OpenGL ES 1.1 driver.
//!
//! Unlike for the GLES1-on-GL2 driver, there's almost no validation of
//! arguments here, because we assume the driver is complete and the app uses it
//! correctly. The exception is where we expect an extension could be used that
//! the driver might not support (e.g. vendor-specific texture compression).
//! In such cases, we should reject vendor-specific things unless we've made
//! sure we can emulate them on all host platforms for touchHLE.

use super::gles11_raw as gles11;
use super::gles11_raw::types::*;
use super::gles_generic::GLES;
use super::util::{try_decode_pvrtc, PalettedTextureFormat};
use super::GLESContext;
use crate::window::{GLContext, GLVersion, Window};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ffi::CStr;
use std::marker::PhantomData;

// [MoleWorld iOS · P0 修复] 原生 OpenGL ES 1.1 NPOT 纹理完整性兼容层。
// 依据 Khronos ES1.1 §3.7.10 + Apple GL_APPLE_texture_2D_limited_npot:iOS GLES1 上 NPOT
// (非2的幂)纹理只有在 wrap=CLAMP_TO_EDGE 且 min filter 非 mipmap 时才"完整",否则不完整,
// 采样恒返回 (0,0,0,1)/被视为纹理禁用。桌面 GLES1-on-GL2 跑在桌面 GL2.1(核心支持 NPOT)
// 故无此问题——这正是"同一份解释器/数据、只真机卡好友村"的根因:好友村/头像的 NPOT 纹理在真机
// 不完整 → 渲染层画不出 → 反复重画+精灵累积 → 单帧 drawScene 永不结束 → 卡死。本兼容层只改
// NPOT 纹理(POT 零影响,主村正常),把它们强制成 limited-NPOT 合法配置。
thread_local! {
    static NPOT_TEX: RefCell<HashSet<GLuint>> = RefCell::new(HashSet::new());
    static ACTIVE_UNIT: Cell<usize> = const { Cell::new(0) };
    static BOUND_2D: RefCell<[GLuint; 8]> = const { RefCell::new([0u32; 8]) };
    /// [MoleWorld iOS · 性能] BOUND_2D 是否可信。上下文切换(make_current 真正切换时)置 false:
    /// 合成器与游戏是两个 GL 上下文却共用这份线程本地跟踪,不复位就会把"另一个上下文里已绑定的同名
    /// 纹理"误判为冗余而跳过绑定 → 画错纹理。
    static BIND_CACHE_VALID: Cell<bool> = const { Cell::new(false) };
    static NPOT_LOG_N: Cell<u64> = const { Cell::new(0) };
}
fn tex_is_npot(w: GLsizei, h: GLsizei) -> bool {
    let (w, h) = (w as u32, h as u32);
    w > 0 && h > 0 && ((w & (w - 1)) != 0 || (h & (h - 1)) != 0)
}
fn cur_bound_2d() -> GLuint {
    let u = ACTIVE_UNIT.with(|c| c.get()).min(7);
    BOUND_2D.with(|b| b.borrow()[u])
}
fn is_mipmap_min_filter(p: GLint) -> bool {
    matches!(
        p as GLenum,
        gles11::NEAREST_MIPMAP_NEAREST
            | gles11::LINEAR_MIPMAP_NEAREST
            | gles11::NEAREST_MIPMAP_LINEAR
            | gles11::LINEAR_MIPMAP_LINEAR
    )
}

pub struct GLES1NativeContext {
    gl_ctx: GLContext,
    is_loaded: bool,
}
impl GLESContext for GLES1NativeContext {
    fn description() -> &'static str {
        "Native OpenGL ES 1.1"
    }

    fn new(window: &mut Window) -> Result<Self, String> {
        Ok(Self {
            gl_ctx: window.create_gl_context(GLVersion::GLES11)?,
            is_loaded: false,
        })
    }

    fn make_current<'gl_ctx, 'win: 'gl_ctx>(
        &'gl_ctx mut self,
        window: &'win mut Window,
    ) -> Box<dyn GLES + 'gl_ctx> {
        if self.gl_ctx.is_current() && self.is_loaded {
            return Box::new(GLES1Native {
                _gl_lifetime: PhantomData,
            });
        }

        unsafe {
            window.make_gl_context_current(&self.gl_ctx);
        }
        gles11::load_with(|s| window.gl_get_proc_address(s));
        self.is_loaded = true;
        BIND_CACHE_VALID.with(|v| v.set(false));
        Box::new(GLES1Native {
            _gl_lifetime: PhantomData,
        })
    }

    unsafe fn make_current_unchecked_for_window<'gl_ctx>(
        &'gl_ctx mut self,
        make_current_fn: &mut dyn FnMut(&GLContext),
        loader_fn: &mut dyn FnMut(&'static str) -> *const std::ffi::c_void,
    ) -> Box<dyn GLES + 'gl_ctx> {
        if self.gl_ctx.is_current() && self.is_loaded {
            return Box::new(GLES1Native {
                _gl_lifetime: PhantomData,
            });
        }

        make_current_fn(&self.gl_ctx);
        gles11::load_with(loader_fn);
        self.is_loaded = true;
        BIND_CACHE_VALID.with(|v| v.set(false));
        Box::new(GLES1Native {
            _gl_lifetime: PhantomData,
        })
    }
}

pub struct GLES1Native<'gl_ctx> {
    _gl_lifetime: PhantomData<&'gl_ctx ()>,
}

impl GLES for GLES1Native<'_> {
    unsafe fn driver_description(&self) -> String {
        // [MoleWorld iOS] 在 iOS 上跳过 glGetString:实测在 PlayCover(iOS-on-Mac)下,
        // 即便 SDL_GL_MakeCurrent 成功返回,glGetString 仍因没有真正 current 的 GL 上下文
        // 段错误(EXC_BAD_ACCESS @0x3b0)。driver_description 仅用于打印一行 Driver info,
        // 不值得为它让整个 app 崩;略过它,让启动继续到真正的渲染调用。
        #[cfg(target_os = "ios")]
        {
            return String::from("Native OpenGL ES 1.1 (iOS)");
        }
        #[cfg(not(target_os = "ios"))]
        {
            let version = CStr::from_ptr(gles11::GetString(gles11::VERSION) as *const _);
            let vendor = CStr::from_ptr(gles11::GetString(gles11::VENDOR) as *const _);
            let renderer = CStr::from_ptr(gles11::GetString(gles11::RENDERER) as *const _);
            // OpenGL ES requires the version to be prefixed "OpenGL ES", so we
            // don't need to contextualize it.
            format!(
                "{} / {} / {}",
                version.to_string_lossy(),
                vendor.to_string_lossy(),
                renderer.to_string_lossy()
            )
        }
    }

    // Generic state manipulation
    unsafe fn GetError(&mut self) -> GLenum {
        gles11::GetError()
    }
    unsafe fn Enable(&mut self, cap: GLenum) {
        gles11::Enable(cap)
    }
    unsafe fn IsEnabled(&mut self, cap: GLenum) -> GLboolean {
        gles11::IsEnabled(cap)
    }
    unsafe fn Disable(&mut self, cap: GLenum) {
        gles11::Disable(cap)
    }
    unsafe fn ClientActiveTexture(&mut self, texture: GLenum) {
        gles11::ClientActiveTexture(texture);
    }
    unsafe fn EnableClientState(&mut self, array: GLenum) {
        gles11::EnableClientState(array)
    }
    unsafe fn DisableClientState(&mut self, array: GLenum) {
        gles11::DisableClientState(array)
    }
    unsafe fn GetBooleanv(&mut self, pname: GLenum, params: *mut GLboolean) {
        gles11::GetBooleanv(pname, params)
    }
    unsafe fn GetFloatv(&mut self, pname: GLenum, params: *mut GLfloat) {
        gles11::GetFloatv(pname, params)
    }
    unsafe fn GetIntegerv(&mut self, pname: GLenum, params: *mut GLint) {
        gles11::GetIntegerv(pname, params)
    }
    unsafe fn GetTexEnviv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        gles11::GetTexEnviv(target, pname, params)
    }
    unsafe fn GetTexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *mut GLfloat) {
        gles11::GetTexEnvfv(target, pname, params)
    }
    unsafe fn GetPointerv(&mut self, pname: GLenum, params: *mut *const GLvoid) {
        // The second argument to glGetPointerv must be a mutable pointer,
        // but gl_generator generates the wrong signature by mistake, see
        // https://github.com/brendanzab/gl-rs/issues/541
        gles11::GetPointerv(pname, params as *mut _ as *const _)
    }
    unsafe fn Hint(&mut self, target: GLenum, mode: GLenum) {
        gles11::Hint(target, mode)
    }
    unsafe fn Finish(&mut self) {
        gles11::Finish()
    }
    unsafe fn Flush(&mut self) {
        gles11::Flush()
    }
    unsafe fn GetString(&mut self, name: GLenum) -> *const GLubyte {
        gles11::GetString(name)
    }

    // Other state manipulation
    unsafe fn AlphaFunc(&mut self, func: GLenum, ref_: GLclampf) {
        gles11::AlphaFunc(func, ref_)
    }
    unsafe fn AlphaFuncx(&mut self, func: GLenum, ref_: GLclampx) {
        gles11::AlphaFuncx(func, ref_)
    }
    unsafe fn BlendFunc(&mut self, sfactor: GLenum, dfactor: GLenum) {
        gles11::BlendFunc(sfactor, dfactor)
    }
    unsafe fn BlendEquationOES(&mut self, mode: GLenum) {
        gles11::BlendEquationOES(mode);
    }
    unsafe fn ColorMask(
        &mut self,
        red: GLboolean,
        green: GLboolean,
        blue: GLboolean,
        alpha: GLboolean,
    ) {
        gles11::ColorMask(red, green, blue, alpha)
    }
    unsafe fn ClipPlanef(&mut self, plane: GLenum, equation: *const GLfloat) {
        gles11::ClipPlanef(plane, equation)
    }
    unsafe fn ClipPlanex(&mut self, plane: GLenum, equation: *const GLfixed) {
        gles11::ClipPlanex(plane, equation)
    }
    unsafe fn CullFace(&mut self, mode: GLenum) {
        gles11::CullFace(mode)
    }
    unsafe fn DepthFunc(&mut self, func: GLenum) {
        gles11::DepthFunc(func)
    }
    unsafe fn DepthMask(&mut self, flag: GLboolean) {
        gles11::DepthMask(flag)
    }
    unsafe fn FrontFace(&mut self, mode: GLenum) {
        gles11::FrontFace(mode)
    }
    unsafe fn DepthRangef(&mut self, near: GLclampf, far: GLclampf) {
        gles11::DepthRangef(near, far)
    }
    unsafe fn DepthRangex(&mut self, near: GLclampx, far: GLclampx) {
        gles11::DepthRangex(near, far)
    }
    unsafe fn PolygonOffset(&mut self, factor: GLfloat, units: GLfloat) {
        gles11::PolygonOffset(factor, units)
    }
    unsafe fn PolygonOffsetx(&mut self, factor: GLfixed, units: GLfixed) {
        gles11::PolygonOffsetx(factor, units)
    }
    unsafe fn SampleCoverage(&mut self, value: GLclampf, invert: GLboolean) {
        gles11::SampleCoverage(value, invert)
    }
    unsafe fn SampleCoveragex(&mut self, value: GLclampx, invert: GLboolean) {
        gles11::SampleCoveragex(value, invert)
    }
    unsafe fn ShadeModel(&mut self, mode: GLenum) {
        gles11::ShadeModel(mode)
    }
    unsafe fn Scissor(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        gles11::Scissor(x, y, width, height)
    }
    unsafe fn Viewport(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        gles11::Viewport(x, y, width, height)
    }
    unsafe fn LineWidth(&mut self, val: GLfloat) {
        gles11::LineWidth(val)
    }
    unsafe fn LineWidthx(&mut self, val: GLfixed) {
        gles11::LineWidthx(val)
    }
    unsafe fn StencilFunc(&mut self, func: GLenum, ref_: GLint, mask: GLuint) {
        gles11::StencilFunc(func, ref_, mask);
    }
    unsafe fn StencilOp(&mut self, sfail: GLenum, dpfail: GLenum, dppass: GLenum) {
        gles11::StencilOp(sfail, dpfail, dppass);
    }
    unsafe fn StencilMask(&mut self, mask: GLuint) {
        gles11::StencilMask(mask);
    }
    unsafe fn LogicOp(&mut self, opcode: GLenum) {
        gles11::LogicOp(opcode);
    }

    // Points
    unsafe fn PointSize(&mut self, size: GLfloat) {
        gles11::PointSize(size)
    }
    unsafe fn PointSizex(&mut self, size: GLfixed) {
        gles11::PointSizex(size)
    }
    unsafe fn PointParameterf(&mut self, pname: GLenum, param: GLfloat) {
        gles11::PointParameterf(pname, param)
    }
    unsafe fn PointParameterx(&mut self, pname: GLenum, param: GLfixed) {
        gles11::PointParameterx(pname, param)
    }
    unsafe fn PointParameterfv(&mut self, pname: GLenum, params: *const GLfloat) {
        gles11::PointParameterfv(pname, params)
    }
    unsafe fn PointParameterxv(&mut self, pname: GLenum, params: *const GLfixed) {
        gles11::PointParameterxv(pname, params)
    }

    // Lighting and materials
    unsafe fn Fogf(&mut self, pname: GLenum, param: GLfloat) {
        gles11::Fogf(pname, param)
    }
    unsafe fn Fogx(&mut self, pname: GLenum, param: GLfixed) {
        gles11::Fogx(pname, param)
    }
    unsafe fn Fogfv(&mut self, pname: GLenum, params: *const GLfloat) {
        gles11::Fogfv(pname, params)
    }
    unsafe fn Fogxv(&mut self, pname: GLenum, params: *const GLfixed) {
        gles11::Fogxv(pname, params)
    }
    unsafe fn Lightf(&mut self, light: GLenum, pname: GLenum, param: GLfloat) {
        gles11::Lightf(light, pname, param)
    }
    unsafe fn Lightx(&mut self, light: GLenum, pname: GLenum, param: GLfixed) {
        gles11::Lightx(light, pname, param)
    }
    unsafe fn Lightfv(&mut self, light: GLenum, pname: GLenum, params: *const GLfloat) {
        gles11::Lightfv(light, pname, params)
    }
    unsafe fn Lightxv(&mut self, light: GLenum, pname: GLenum, params: *const GLfixed) {
        gles11::Lightxv(light, pname, params)
    }
    unsafe fn LightModelf(&mut self, pname: GLenum, param: GLfloat) {
        gles11::LightModelf(pname, param)
    }
    unsafe fn LightModelx(&mut self, pname: GLenum, param: GLfixed) {
        gles11::LightModelx(pname, param)
    }
    unsafe fn LightModelfv(&mut self, pname: GLenum, params: *const GLfloat) {
        gles11::LightModelfv(pname, params)
    }
    unsafe fn LightModelxv(&mut self, pname: GLenum, params: *const GLfixed) {
        gles11::LightModelxv(pname, params)
    }
    unsafe fn Materialf(&mut self, face: GLenum, pname: GLenum, param: GLfloat) {
        gles11::Materialf(face, pname, param)
    }
    unsafe fn Materialx(&mut self, face: GLenum, pname: GLenum, param: GLfixed) {
        gles11::Materialx(face, pname, param)
    }
    unsafe fn Materialfv(&mut self, face: GLenum, pname: GLenum, params: *const GLfloat) {
        gles11::Materialfv(face, pname, params)
    }
    unsafe fn Materialxv(&mut self, face: GLenum, pname: GLenum, params: *const GLfixed) {
        gles11::Materialxv(face, pname, params)
    }

    // Buffers
    unsafe fn IsBuffer(&mut self, buffer: GLuint) -> GLboolean {
        gles11::IsBuffer(buffer)
    }
    unsafe fn GenBuffers(&mut self, n: GLsizei, buffers: *mut GLuint) {
        gles11::GenBuffers(n, buffers)
    }
    unsafe fn DeleteBuffers(&mut self, n: GLsizei, buffers: *const GLuint) {
        gles11::DeleteBuffers(n, buffers)
    }
    unsafe fn BindBuffer(&mut self, target: GLenum, buffer: GLuint) {
        assert!(target == gles11::ARRAY_BUFFER || target == gles11::ELEMENT_ARRAY_BUFFER);
        gles11::BindBuffer(target, buffer)
    }
    unsafe fn BufferData(
        &mut self,
        target: GLenum,
        size: GLsizeiptr,
        data: *const GLvoid,
        usage: GLenum,
    ) {
        assert!(target == gles11::ARRAY_BUFFER || target == gles11::ELEMENT_ARRAY_BUFFER);
        gles11::BufferData(target, size, data, usage)
    }

    unsafe fn BufferSubData(
        &mut self,
        target: GLenum,
        offset: GLintptr,
        size: GLsizeiptr,
        data: *const GLvoid,
    ) {
        assert!(target == gles11::ARRAY_BUFFER || target == gles11::ELEMENT_ARRAY_BUFFER);
        gles11::BufferSubData(target, offset, size, data)
    }

    // Non-pointers
    unsafe fn Color4f(&mut self, red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
        gles11::Color4f(red, green, blue, alpha)
    }
    unsafe fn Color4x(&mut self, red: GLfixed, green: GLfixed, blue: GLfixed, alpha: GLfixed) {
        gles11::Color4x(red, green, blue, alpha)
    }
    unsafe fn Color4ub(&mut self, red: GLubyte, green: GLubyte, blue: GLubyte, alpha: GLubyte) {
        gles11::Color4ub(red, green, blue, alpha)
    }
    unsafe fn Normal3f(&mut self, nx: GLfloat, ny: GLfloat, nz: GLfloat) {
        gles11::Normal3f(nx, ny, nz)
    }
    unsafe fn Normal3x(&mut self, nx: GLfixed, ny: GLfixed, nz: GLfixed) {
        gles11::Normal3x(nx, ny, nz)
    }

    // Pointers
    unsafe fn ColorPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        gles11::ColorPointer(size, type_, stride, pointer)
    }
    unsafe fn NormalPointer(&mut self, type_: GLenum, stride: GLsizei, pointer: *const GLvoid) {
        gles11::NormalPointer(type_, stride, pointer)
    }
    unsafe fn TexCoordPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        gles11::TexCoordPointer(size, type_, stride, pointer)
    }
    unsafe fn VertexPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        gles11::VertexPointer(size, type_, stride, pointer)
    }

    // Drawing
    unsafe fn DrawArrays(&mut self, mode: GLenum, first: GLint, count: GLsizei) {
        crate::mole_perf::DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        gles11::DrawArrays(mode, first, count)
    }
    unsafe fn DrawElements(
        &mut self,
        mode: GLenum,
        count: GLsizei,
        type_: GLenum,
        indices: *const GLvoid,
    ) {
        crate::mole_perf::DRAWS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        gles11::DrawElements(mode, count, type_, indices)
    }

    // Clearing
    unsafe fn Clear(&mut self, mask: GLbitfield) {
        gles11::Clear(mask)
    }
    unsafe fn ClearColor(
        &mut self,
        red: GLclampf,
        green: GLclampf,
        blue: GLclampf,
        alpha: GLclampf,
    ) {
        gles11::ClearColor(red, green, blue, alpha)
    }
    unsafe fn ClearColorx(
        &mut self,
        red: GLclampx,
        green: GLclampx,
        blue: GLclampx,
        alpha: GLclampx,
    ) {
        gles11::ClearColorx(red, green, blue, alpha)
    }
    unsafe fn ClearDepthf(&mut self, depth: GLclampf) {
        gles11::ClearDepthf(depth)
    }
    unsafe fn ClearDepthx(&mut self, depth: GLclampx) {
        gles11::ClearDepthx(depth)
    }
    unsafe fn ClearStencil(&mut self, s: GLint) {
        gles11::ClearStencil(s)
    }

    // Textures
    unsafe fn PixelStorei(&mut self, pname: GLenum, param: GLint) {
        gles11::PixelStorei(pname, param)
    }
    unsafe fn ReadPixels(
        &mut self,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *mut GLvoid,
    ) {
        gles11::ReadPixels(x, y, width, height, format, type_, pixels)
    }
    unsafe fn GenTextures(&mut self, n: GLsizei, textures: *mut GLuint) {
        gles11::GenTextures(n, textures)
    }
    unsafe fn DeleteTextures(&mut self, n: GLsizei, textures: *const GLuint) {
        if n > 0 && !textures.is_null() {
            for i in 0..n as isize {
                let t = *textures.offset(i);
                crate::mole_perf::note_delete_texture(t);
                // GL 语义:删除当前绑定的纹理 → 该单元绑定变为 0。跟踪表同步,否则名字复用后会被误判冗余。
                BOUND_2D.with(|b| {
                    for slot in b.borrow_mut().iter_mut() {
                        if *slot == t {
                            *slot = 0;
                        }
                    }
                });
            }
            NPOT_TEX.with(|s| {
                let mut set = s.borrow_mut();
                for i in 0..n as isize {
                    set.remove(&*textures.offset(i));
                }
            });
        }
        gles11::DeleteTextures(n, textures)
    }
    unsafe fn ActiveTexture(&mut self, texture: GLenum) {
        if texture >= gles11::TEXTURE0 {
            ACTIVE_UNIT.with(|c| c.set((texture - gles11::TEXTURE0) as usize));
        }
        gles11::ActiveTexture(texture)
    }
    unsafe fn IsTexture(&mut self, texture: GLuint) -> GLboolean {
        gles11::IsTexture(texture)
    }
    unsafe fn BindTexture(&mut self, target: GLenum, texture: GLuint) {
        crate::mole_perf::BINDTEX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if target == gles11::TEXTURE_2D {
            let u = ACTIVE_UNIT.with(|c| c.get()).min(7);
            // [MoleWorld iOS · 性能] 冗余绑定消除:村里每帧 ~1240 次 BindTexture(每精灵一次),其中
            // 连续同图集的精灵是重复绑定。GLES1→Metal 每次调用都要过驱动翻译层,跳过即净赚。
            // 仅当跟踪可信(未跨上下文)且目标纹理已是当前绑定时跳过。
            let same = BIND_CACHE_VALID.with(|v| v.get()) && BOUND_2D.with(|b| b.borrow()[u] == texture);
            if same {
                crate::mole_perf::BINDTEX_SKIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                return;
            }
            BOUND_2D.with(|b| b.borrow_mut()[u] = texture);
            BIND_CACHE_VALID.with(|v| v.set(true));
        }
        gles11::BindTexture(target, texture)
    }
    unsafe fn TexParameteri(&mut self, target: GLenum, pname: GLenum, mut param: GLint) {
        // NPOT 纹理:游戏若设 REPEAT/ mipmap-min-filter 会让它在原生 GLES1 不完整 → 强制改成
        // CLAMP_TO_EDGE / LINEAR(limited-NPOT 合法配置)。只影响 NPOT,POT 纹理原样放行。
        if target == gles11::TEXTURE_2D {
            let tex = cur_bound_2d();
            if tex != 0 && NPOT_TEX.with(|s| s.borrow().contains(&tex)) {
                if (pname == gles11::TEXTURE_WRAP_S || pname == gles11::TEXTURE_WRAP_T)
                    && param as GLenum == gles11::REPEAT
                {
                    param = gles11::CLAMP_TO_EDGE as GLint;
                } else if pname == gles11::TEXTURE_MIN_FILTER && is_mipmap_min_filter(param) {
                    param = gles11::LINEAR as GLint;
                }
            }
        }
        gles11::TexParameteri(target, pname, param)
    }
    unsafe fn TexParameterf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        gles11::TexParameterf(target, pname, param)
    }
    unsafe fn TexParameterx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        gles11::TexParameterx(target, pname, param)
    }
    unsafe fn TexParameteriv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        gles11::TexParameteriv(target, pname, params)
    }
    unsafe fn TexParameterfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        gles11::TexParameterfv(target, pname, params)
    }
    unsafe fn TexParameterxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        gles11::TexParameterxv(target, pname, params)
    }
    unsafe fn TexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        mut internalformat: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        if level == 0 {
            let mut bound: GLint = 0;
            gles11::GetIntegerv(gles11::TEXTURE_BINDING_2D, &mut bound);
            crate::mole_perf::note_teximage(
                bound as u32,
                crate::mole_perf::tex_bytes(width, height, format, type_),
            );
        }
        if format == gles11::BGRA_EXT {
            // This is needed in order to avoid white screen issue on Android!
            // As per BGRA extension specs
            // https://registry.khronos.org/OpenGL/extensions/EXT/EXT_texture_format_BGRA8888.txt,
            // both internalformat and format should be BGRA
            // Tangentially related issue
            // (actually a reverse of what we're doing here)
            // https://android-review.googlesource.com/c/platform/external/qemu/+/974666
            internalformat = gles11::BGRA_EXT as GLint
        }
        gles11::TexImage2D(
            target,
            level,
            internalformat,
            width,
            height,
            border,
            format,
            type_,
            pixels,
        );
        // [MoleWorld iOS · P0 修复] NPOT 纹理完整性:上传后若是 NPOT,记入集合并立刻强制
        // CLAMP_TO_EDGE + 非 mipmap min filter,使其在原生 GLES1 上"完整"可采样。POT 纹理
        // 不动(主村正常)。TexParameteri 里对该集合的纹理也会持续钳制(防游戏之后再设 REPEAT)。
        if target == gles11::TEXTURE_2D && level == 0 {
            let tex = cur_bound_2d();
            if tex != 0 {
                if tex_is_npot(width, height) {
                    NPOT_TEX.with(|s| {
                        s.borrow_mut().insert(tex);
                    });
                    gles11::TexParameteri(
                        gles11::TEXTURE_2D,
                        gles11::TEXTURE_WRAP_S,
                        gles11::CLAMP_TO_EDGE as GLint,
                    );
                    gles11::TexParameteri(
                        gles11::TEXTURE_2D,
                        gles11::TEXTURE_WRAP_T,
                        gles11::CLAMP_TO_EDGE as GLint,
                    );
                    let mut mf: GLint = 0;
                    gles11::GetTexParameteriv(
                        gles11::TEXTURE_2D,
                        gles11::TEXTURE_MIN_FILTER,
                        &mut mf,
                    );
                    if is_mipmap_min_filter(mf) {
                        gles11::TexParameteri(
                            gles11::TEXTURE_2D,
                            gles11::TEXTURE_MIN_FILTER,
                            gles11::LINEAR as GLint,
                        );
                    }
                    let n = NPOT_LOG_N.with(|c| {
                        let v = c.get();
                        c.set(v + 1);
                        v
                    });
                    if n < 12 || n & 0x3f == 0 {
                        echo!("[NPOT-FIX] tex={} {}x{} -> CLAMP_TO_EDGE (n={})", tex, width, height, n);
                    }
                } else {
                    NPOT_TEX.with(|s| {
                        s.borrow_mut().remove(&tex);
                    });
                }
            }
        }
    }
    unsafe fn TexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        crate::mole_perf::TEXSUBIMAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        gles11::TexSubImage2D(
            target, level, xoffset, yoffset, width, height, format, type_, pixels,
        )
    }
    unsafe fn CompressedTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        image_size: GLsizei,
        data: *const GLvoid,
    ) {
        crate::mole_perf::COMPRESSED_TEX.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if level == 0 {
            let mut bound: GLint = 0;
            gles11::GetIntegerv(gles11::TEXTURE_BINDING_2D, &mut bound);
            // 压缩纹理按解压后 RGBA 记(它在本实现里会被软解成 RGBA 上传)
            crate::mole_perf::note_teximage(
                bound as u32,
                (width.max(0) as u64) * (height.max(0) as u64) * 4,
            );
        }
        let data = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), image_size as usize) };
        // IMG_texture_compression_pvrtc (only on Imagination/Apple GPUs)
        // TODO: It would be more efficient to use hardware decoding where
        // available (I just don't have a suitable device to try this on)
        if try_decode_pvrtc(
            self,
            target,
            level,
            internalformat,
            width,
            height,
            border,
            data,
        ) {
            log_dbg!("Decoded PVRTC");
            return;
        }

        // OES_compressed_paletted_texture is in the common profile of OpenGL ES
        // 1.1, so we can reasonably assume it's supported.
        if PalettedTextureFormat::get_info(internalformat).is_none() {
            unimplemented!("CompressedTexImage2D internalformat: {:#x}", internalformat);
        }
        log_dbg!("Directly supported texture format: {:#x}", internalformat);
        gles11::CompressedTexImage2D(
            target,
            level,
            internalformat,
            width,
            height,
            border,
            image_size,
            data.as_ptr() as *const _,
        );
    }
    unsafe fn CopyTexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLenum,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
    ) {
        gles11::CopyTexImage2D(target, level, internalformat, x, y, width, height, border)
    }
    unsafe fn CopyTexSubImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        xoffset: GLint,
        yoffset: GLint,
        x: GLint,
        y: GLint,
        width: GLsizei,
        height: GLsizei,
    ) {
        gles11::CopyTexSubImage2D(target, level, xoffset, yoffset, x, y, width, height)
    }
    unsafe fn TexEnvf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        gles11::TexEnvf(target, pname, param)
    }
    unsafe fn TexEnvx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        gles11::TexEnvx(target, pname, param)
    }
    unsafe fn TexEnvi(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        gles11::TexEnvi(target, pname, param)
    }
    unsafe fn TexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        if target == gles11::TEXTURE_FILTER_CONTROL_EXT {
            assert!(pname == gles11::TEXTURE_LOD_BIAS_EXT);
            unsafe {
                if !CStr::from_ptr(gles11::GetString(gles11::EXTENSIONS) as _)
                    .to_str()
                    .unwrap()
                    .contains("EXT_texture_lod_bias")
                {
                    log_dbg!("GL_EXT_texture_lod_bias is unsupported, skipping TexEnvfv({:#x}, {:#x}, ...) call", target, pname);
                    return;
                }
            };
        }
        gles11::TexEnvfv(target, pname, params)
    }
    unsafe fn TexEnvxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        gles11::TexEnvxv(target, pname, params)
    }
    unsafe fn TexEnviv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        gles11::TexEnviv(target, pname, params)
    }

    unsafe fn MultiTexCoord4f(
        &mut self,
        target: GLenum,
        s: GLfloat,
        t: GLfloat,
        r: GLfloat,
        q: GLfloat,
    ) {
        gles11::MultiTexCoord4f(target, s, t, r, q)
    }
    unsafe fn MultiTexCoord4x(
        &mut self,
        target: GLenum,
        s: GLfixed,
        t: GLfixed,
        r: GLfixed,
        q: GLfixed,
    ) {
        gles11::MultiTexCoord4x(target, s, t, r, q)
    }

    // Matrix stack operations
    unsafe fn MatrixMode(&mut self, mode: GLenum) {
        gles11::MatrixMode(mode)
    }
    unsafe fn LoadIdentity(&mut self) {
        gles11::LoadIdentity()
    }
    unsafe fn LoadMatrixf(&mut self, m: *const GLfloat) {
        gles11::LoadMatrixf(m)
    }
    unsafe fn LoadMatrixx(&mut self, m: *const GLfixed) {
        gles11::LoadMatrixx(m)
    }
    unsafe fn MultMatrixf(&mut self, m: *const GLfloat) {
        gles11::MultMatrixf(m)
    }
    unsafe fn MultMatrixx(&mut self, m: *const GLfixed) {
        gles11::MultMatrixx(m)
    }
    unsafe fn PushMatrix(&mut self) {
        gles11::PushMatrix()
    }
    unsafe fn PopMatrix(&mut self) {
        gles11::PopMatrix();
    }
    unsafe fn Orthof(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        gles11::Orthof(left, right, bottom, top, near, far)
    }
    unsafe fn Orthox(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        gles11::Orthox(left, right, bottom, top, near, far)
    }
    unsafe fn Frustumf(
        &mut self,
        left: GLfloat,
        right: GLfloat,
        bottom: GLfloat,
        top: GLfloat,
        near: GLfloat,
        far: GLfloat,
    ) {
        gles11::Frustumf(left, right, bottom, top, near, far)
    }
    unsafe fn Frustumx(
        &mut self,
        left: GLfixed,
        right: GLfixed,
        bottom: GLfixed,
        top: GLfixed,
        near: GLfixed,
        far: GLfixed,
    ) {
        gles11::Frustumx(left, right, bottom, top, near, far)
    }
    unsafe fn Rotatef(&mut self, angle: GLfloat, x: GLfloat, y: GLfloat, z: GLfloat) {
        gles11::Rotatef(angle, x, y, z)
    }
    unsafe fn Rotatex(&mut self, angle: GLfixed, x: GLfixed, y: GLfixed, z: GLfixed) {
        gles11::Rotatex(angle, x, y, z)
    }
    unsafe fn Scalef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        gles11::Scalef(x, y, z)
    }
    unsafe fn Scalex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        gles11::Scalex(x, y, z)
    }
    unsafe fn Translatef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        gles11::Translatef(x, y, z)
    }
    unsafe fn Translatex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        gles11::Translatex(x, y, z)
    }

    // OES_framebuffer_object -> EXT_framebuffer_object
    unsafe fn GenFramebuffersOES(&mut self, n: GLsizei, framebuffers: *mut GLuint) {
        gles11::GenFramebuffersOES(n, framebuffers)
    }
    unsafe fn GenRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *mut GLuint) {
        gles11::GenRenderbuffersOES(n, renderbuffers)
    }
    unsafe fn IsFramebufferOES(&mut self, renderbuffer: GLuint) -> GLboolean {
        gles11::IsFramebufferOES(renderbuffer)
    }
    unsafe fn IsRenderbufferOES(&mut self, renderbuffer: GLuint) -> GLboolean {
        gles11::IsRenderbufferOES(renderbuffer)
    }
    unsafe fn BindFramebufferOES(&mut self, target: GLenum, framebuffer: GLuint) {
        gles11::BindFramebufferOES(target, framebuffer)
    }
    unsafe fn BindRenderbufferOES(&mut self, target: GLenum, renderbuffer: GLuint) {
        gles11::BindRenderbufferOES(target, renderbuffer)
    }
    unsafe fn RenderbufferStorageOES(
        &mut self,
        target: GLenum,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
    ) {
        gles11::RenderbufferStorageOES(target, internalformat, width, height)
    }
    unsafe fn FramebufferRenderbufferOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        renderbuffertarget: GLenum,
        renderbuffer: GLuint,
    ) {
        gles11::FramebufferRenderbufferOES(target, attachment, renderbuffertarget, renderbuffer)
    }
    unsafe fn FramebufferTexture2DOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        textarget: GLenum,
        texture: GLuint,
        level: i32,
    ) {
        gles11::FramebufferTexture2DOES(target, attachment, textarget, texture, level)
    }
    unsafe fn GetFramebufferAttachmentParameterivOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gles11::GetFramebufferAttachmentParameterivOES(target, attachment, pname, params)
    }
    unsafe fn GetRenderbufferParameterivOES(
        &mut self,
        target: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gles11::GetRenderbufferParameterivOES(target, pname, params)
    }
    unsafe fn CheckFramebufferStatusOES(&mut self, target: GLenum) -> GLenum {
        gles11::CheckFramebufferStatusOES(target)
    }
    unsafe fn DeleteFramebuffersOES(&mut self, n: GLsizei, framebuffers: *const GLuint) {
        gles11::DeleteFramebuffersOES(n, framebuffers)
    }
    unsafe fn DeleteRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *const GLuint) {
        gles11::DeleteRenderbuffersOES(n, renderbuffers)
    }
    unsafe fn GenerateMipmapOES(&mut self, target: GLenum) {
        gles11::GenerateMipmapOES(target)
    }
    unsafe fn GetBufferParameteriv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        gles11::GetBufferParameteriv(target, pname, params)
    }
    unsafe fn MapBufferOES(&mut self, target: GLenum, access: GLenum) -> *mut GLvoid {
        gles11::MapBufferOES(target, access)
    }
    unsafe fn UnmapBufferOES(&mut self, target: GLenum) -> GLboolean {
        gles11::UnmapBufferOES(target)
    }
}
