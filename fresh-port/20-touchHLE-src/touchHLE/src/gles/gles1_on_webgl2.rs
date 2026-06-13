/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! 在 WebGL2 / OpenGL ES 3.0 之上实现 OpenGL ES 1.1(仅 wasm/emscripten)。
//!
//! 浏览器只提供 WebGL1/WebGL2,没有 GLES1.1 固定管线,也没有桌面 GL2.1 兼容
//! profile。所以这个后端**不能像 [super::gles1_on_gl2] 那样把每个调用直接转发**,
//! 而是要在 CPU 上自己维护 GLES1.1 固定管线状态(矩阵栈 / 当前颜色 / texenv /
//! alpha test 等),并在 `DrawArrays`/`DrawElements` 时:
//!
//! 1. 把游戏的 client-side 顶点数组(WebGL2 禁止 client 指针)拷进持久 VBO;
//! 2. 用一套手写的 GLSL ES 3.00 着色器(uber-shader)完成 MVP 变换 + 顶点色 +
//!    可选纹理(MODULATE/REPLACE)+ alpha test。
//!
//! 关键认知:present 链(present.rs)本身就走这个后端的 trait 方法
//! (`EnableClientState`→`VertexPointer`→`MatrixMode(TEXTURE)`→`DrawArrays`),
//! 所以「这个后端能画一张 textured quad」== present 能通 == 标题画面能出来。

use super::gl21compat_raw as gl21; // 仅取常量(ARRAYS 表用)
use super::gles11_raw as gles11; // 仅取常量 + types
use super::gles11_raw::types::*;
use super::gles30_raw as gl30; // ES3/WebGL2 可编程管线函数
use super::gles_generic::GLES;
use super::util::{fixed_to_float, matrix_fixed_to_float, ParamTable, ParamType};
use super::GLESContext;
use crate::matrix::Matrix;
use crate::window::{GLContext, GLVersion, Window};
use std::collections::HashSet;

/// 我们支持追踪的纹理单元上限(cocos2d-iphone 2D 渲染只用单元 0,留些余量)。
const MAX_TEX_UNITS: usize = 8;

// ───────────────────────────── 顶点数组描述 ─────────────────────────────
//
// 直接照搬 gles1_on_gl2.rs 的 ArrayInfo / ARRAYS(枚举值在 gl21/gles11 间一致)。
// 注意:ARRAYS 的顺序固定为 [COLOR, NORMAL, TEXTURE_COORD, VERTEX],下面很多
// 数组索引常量都依赖这个顺序。

// 为与 gles1_on_gl2 的 ARRAYS 表保持字段对齐而保留全部字段;本后端只读 name/size/
// pointer,其余几个枚举字段仅作文档对照,故 allow(dead_code)。
#[allow(dead_code)]
pub struct ArrayInfo {
    /// `glEnableClientState`/`glDisableClientState`/`glGetBoolean` 用的枚举。
    pub name: GLenum,
    /// `glGetInteger` 查 buffer binding 用的枚举。
    pub buffer_binding: GLenum,
    /// `glGetInteger` 查 size 用的枚举(NORMAL 没有 size)。
    size: Option<GLenum>,
    /// `glGetInteger` 查 stride 用的枚举。
    stride: GLenum,
    /// `glGetInteger` 查 type 用的枚举。
    type_: GLenum,
    /// `glGetPointer` 查 pointer 用的枚举。
    pub pointer: GLenum,
}

/// OpenGL ES 1.1 / OpenGL 2.1 共享的数组列表。顺序固定。
pub const ARRAYS: &[ArrayInfo] = &[
    ArrayInfo {
        name: gl21::COLOR_ARRAY,
        buffer_binding: gl21::COLOR_ARRAY_BUFFER_BINDING,
        size: Some(gl21::COLOR_ARRAY_SIZE),
        stride: gl21::COLOR_ARRAY_STRIDE,
        type_: gl21::COLOR_ARRAY_TYPE,
        pointer: gl21::COLOR_ARRAY_POINTER,
    },
    ArrayInfo {
        name: gl21::NORMAL_ARRAY,
        buffer_binding: gl21::NORMAL_ARRAY_BUFFER_BINDING,
        size: None,
        stride: gl21::NORMAL_ARRAY_STRIDE,
        type_: gl21::NORMAL_ARRAY_TYPE,
        pointer: gl21::NORMAL_ARRAY_POINTER,
    },
    ArrayInfo {
        name: gl21::TEXTURE_COORD_ARRAY,
        buffer_binding: gl21::TEXTURE_COORD_ARRAY_BUFFER_BINDING,
        size: Some(gl21::TEXTURE_COORD_ARRAY_SIZE),
        stride: gl21::TEXTURE_COORD_ARRAY_STRIDE,
        type_: gl21::TEXTURE_COORD_ARRAY_TYPE,
        pointer: gl21::TEXTURE_COORD_ARRAY_POINTER,
    },
    ArrayInfo {
        name: gl21::VERTEX_ARRAY,
        buffer_binding: gl21::VERTEX_ARRAY_BUFFER_BINDING,
        size: Some(gl21::VERTEX_ARRAY_SIZE),
        stride: gl21::VERTEX_ARRAY_STRIDE,
        type_: gl21::VERTEX_ARRAY_TYPE,
        pointer: gl21::VERTEX_ARRAY_POINTER,
    },
];

// ARRAYS 表里的固定索引(供 draw / uniform 逻辑直接命名)。
const ARRAY_COLOR: usize = 0;
const ARRAY_NORMAL: usize = 1;
const ARRAY_TEXCOORD: usize = 2;
const ARRAY_VERTEX: usize = 3;

// 顶点属性 location(与着色器 layout(location=) 一一对应)。
const LOC_POSITION: GLuint = 0;
const LOC_COLOR: GLuint = 1;
const LOC_TEXCOORD: GLuint = 2;

/// `glGet` 参数表:供 GetIntegerv/GetFloatv/GetBooleanv 校验 pname 类型与分量数。
/// 用 gles11 枚举(guest 发的是 GLES1.1 枚举值)。只列出本后端会被查询的项
/// (present_renderbuffer 备份/恢复链 + cocos2d 初始化用到的)。
const GET_PARAMS: ParamTable = ParamTable(&[
    (gles11::ACTIVE_TEXTURE, ParamType::Int, 1),
    (gles11::CLIENT_ACTIVE_TEXTURE, ParamType::Int, 1),
    (gles11::MATRIX_MODE, ParamType::Int, 1),
    (gles11::VIEWPORT, ParamType::Int, 4),
    (gles11::SCISSOR_BOX, ParamType::Int, 4),
    (gles11::CURRENT_COLOR, ParamType::FloatSpecial, 4),
    (gles11::COLOR_CLEAR_VALUE, ParamType::FloatSpecial, 4),
    (gles11::MODELVIEW_MATRIX, ParamType::Float, 16),
    (gles11::PROJECTION_MATRIX, ParamType::Float, 16),
    (gles11::TEXTURE_MATRIX, ParamType::Float, 16),
    (gles11::MAX_TEXTURE_SIZE, ParamType::Int, 1),
    (gles11::MAX_TEXTURE_UNITS, ParamType::Int, 1),
    (gles11::TEXTURE_BINDING_2D, ParamType::Int, 1),
    (gles11::BLEND_SRC, ParamType::Int, 1),
    (gles11::BLEND_DST, ParamType::Int, 1),
    (gles11::DEPTH_FUNC, ParamType::Int, 1),
    (gles11::ALPHA_TEST_FUNC, ParamType::Int, 1),
    (gles11::ALPHA_TEST_REF, ParamType::FloatSpecial, 1),
    (gles11::ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    (gles11::ELEMENT_ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    // capabilities(布尔开关)
    (gles11::ALPHA_TEST, ParamType::Boolean, 1),
    (gles11::BLEND, ParamType::Boolean, 1),
    (gles11::DEPTH_TEST, ParamType::Boolean, 1),
    (gles11::SCISSOR_TEST, ParamType::Boolean, 1),
    (gles11::CULL_FACE, ParamType::Boolean, 1),
    (gles11::TEXTURE_2D, ParamType::Boolean, 1),
    // 数组(布尔启用 + 整型属性)
    (gles11::COLOR_ARRAY, ParamType::Boolean, 1),
    (gles11::NORMAL_ARRAY, ParamType::Boolean, 1),
    (gles11::TEXTURE_COORD_ARRAY, ParamType::Boolean, 1),
    (gles11::VERTEX_ARRAY, ParamType::Boolean, 1),
    (gles11::COLOR_ARRAY_SIZE, ParamType::Int, 1),
    (gles11::COLOR_ARRAY_TYPE, ParamType::Int, 1),
    (gles11::COLOR_ARRAY_STRIDE, ParamType::Int, 1),
    (gles11::COLOR_ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    (gles11::NORMAL_ARRAY_TYPE, ParamType::Int, 1),
    (gles11::NORMAL_ARRAY_STRIDE, ParamType::Int, 1),
    (gles11::NORMAL_ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    (gles11::TEXTURE_COORD_ARRAY_SIZE, ParamType::Int, 1),
    (gles11::TEXTURE_COORD_ARRAY_TYPE, ParamType::Int, 1),
    (gles11::TEXTURE_COORD_ARRAY_STRIDE, ParamType::Int, 1),
    (gles11::TEXTURE_COORD_ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    (gles11::VERTEX_ARRAY_SIZE, ParamType::Int, 1),
    (gles11::VERTEX_ARRAY_TYPE, ParamType::Int, 1),
    (gles11::VERTEX_ARRAY_STRIDE, ParamType::Int, 1),
    (gles11::VERTEX_ARRAY_BUFFER_BINDING, ParamType::Int, 1),
    // framebuffer/renderbuffer 绑定(present_renderbuffer 备份)
    (gles11::FRAMEBUFFER_BINDING_OES, ParamType::Int, 1),
    (gles11::RENDERBUFFER_BINDING_OES, ParamType::Int, 1),
]);

// ───────────────────────────── 着色器源码 ─────────────────────────────

const VERTEX_SRC: &str = r#"#version 300 es
precision highp float;

uniform mat4 u_mvp;        // = projection * modelview(CPU 算好)
uniform mat4 u_texMatrix;  // TEXTURE 矩阵(present 旋转用;默认 identity)

layout(location = 0) in vec4 a_position;   // VERTEX_ARRAY
layout(location = 1) in vec4 a_color;      // COLOR_ARRAY
layout(location = 2) in vec2 a_texcoord;   // TEXTURE_COORD_ARRAY

uniform vec4 u_constColor;     // 无 COLOR_ARRAY 时用 glColor* 常量
uniform bool u_hasColorArray;

out vec4 v_color;
out vec2 v_texcoord;

void main() {
    gl_Position = u_mvp * a_position;
    v_color = u_hasColorArray ? a_color : u_constColor;
    v_texcoord = (u_texMatrix * vec4(a_texcoord, 0.0, 1.0)).xy;
}
"#;

const FRAGMENT_SRC: &str = r#"#version 300 es
precision highp float;

in vec4 v_color;
in vec2 v_texcoord;

uniform sampler2D u_tex0;
uniform bool  u_texEnable;     // TEXTURE_2D 是否启用
uniform int   u_texEnvMode;    // 0=MODULATE 1=REPLACE

uniform bool  u_alphaTest;     // ALPHA_TEST 是否启用
uniform int   u_alphaFunc;     // GL_LESS=0x0201 等,传原 enum
uniform float u_alphaRef;

out vec4 fragColor;

void main() {
    vec4 c = v_color;
    if (u_texEnable) {
        vec4 t = texture(u_tex0, v_texcoord);
        if (u_texEnvMode == 1) {          // REPLACE
            c = t;
        } else {                          // MODULATE(默认)
            c = v_color * t;
        }
    }
    if (u_alphaTest) {
        // GL_NEVER=0x0200 LESS=0x0201 EQUAL=0x0202 LEQUAL=0x0203
        // GREATER=0x0204 NOTEQUAL=0x0205 GEQUAL=0x0206 ALWAYS=0x0207
        bool pass;
        if      (u_alphaFunc == 0x0200) pass = false;
        else if (u_alphaFunc == 0x0201) pass = c.a <  u_alphaRef;
        else if (u_alphaFunc == 0x0202) pass = c.a == u_alphaRef;
        else if (u_alphaFunc == 0x0203) pass = c.a <= u_alphaRef;
        else if (u_alphaFunc == 0x0204) pass = c.a >  u_alphaRef;
        else if (u_alphaFunc == 0x0205) pass = c.a != u_alphaRef;
        else if (u_alphaFunc == 0x0206) pass = c.a >= u_alphaRef;
        else                            pass = true;   // ALWAYS
        if (!pass) discard;
    }
    fragColor = c;
}
"#;

/// 编译好的 uber-shader 程序 + 缓存的 uniform location。
struct ProgramCache {
    prog: GLuint,
    loc_mvp: GLint,
    loc_tex_matrix: GLint,
    loc_const_color: GLint,
    loc_has_color_array: GLint,
    loc_tex0: GLint,
    loc_tex_enable: GLint,
    loc_tex_env_mode: GLint,
    loc_alpha_test: GLint,
    loc_alpha_func: GLint,
    loc_alpha_ref: GLint,
}

/// 单个 client-side 顶点数组的描述(VertexPointer 等记录,draw 时打进 VBO)。
#[derive(Copy, Clone)]
struct ArrayState {
    enabled: bool,
    size: GLint,
    type_: GLenum,
    stride: GLsizei,
    pointer: *const GLvoid,
    /// 设置该数组时绑定的 ARRAY_BUFFER(0 = client 内存)。
    buffer_binding: GLuint,
}
impl Default for ArrayState {
    fn default() -> Self {
        ArrayState {
            enabled: false,
            size: 4,
            type_: gles11::FLOAT,
            stride: 0,
            pointer: std::ptr::null(),
            buffer_binding: 0,
        }
    }
}

// ───────────────────────────── State ─────────────────────────────

pub struct GLES1OnWebGL2State {
    // 定点数组追踪(照搬 gles1_on_gl2)
    pointer_is_fixed_point: [bool; ARRAYS.len()],
    fixed_point_texture_units: HashSet<GLenum>,
    fixed_point_translation_buffers: [Vec<GLfloat>; ARRAYS.len()],

    // 矩阵栈(CPU 自建)
    matrix_mode: GLenum,
    modelview_stack: Vec<Matrix<4>>,
    projection_stack: Vec<Matrix<4>>,
    texture_stack: Vec<Matrix<4>>,

    // 当前顶点属性常量(无数组时的默认值)
    current_color: [GLfloat; 4],
    current_normal: [GLfloat; 3],

    // client 数组描述
    arrays: [ArrayState; ARRAYS.len()],
    client_active_texture: GLenum, // TexCoordPointer 作用的单元
    active_texture: GLenum,        // 采样/BindTexture 作用的单元
    array_buffer_binding: GLuint,  // 当前 ARRAY_BUFFER
    element_array_buffer_binding: GLuint,

    // 纹理环境(每单元 MODULATE/REPLACE...)
    tex_env_mode: [GLenum; MAX_TEX_UNITS],

    // 能力开关
    blend_enabled: bool,
    blend_sfactor: GLenum,
    blend_dfactor: GLenum,
    alpha_test_enabled: bool,
    alpha_func: GLenum,
    alpha_ref: GLfloat,
    texture_2d_enabled: bool,
    depth_test_enabled: bool,
    depth_func: GLenum,
    cull_enabled: bool,
    cull_face: GLenum,
    front_face: GLenum,
    scissor_enabled: bool,
    scissor: (GLint, GLint, GLsizei, GLsizei),

    // clear / viewport
    clear_color: [GLfloat; 4],
    viewport: (GLint, GLint, GLsizei, GLsizei),

    // 每单元 TEXTURE_2D 绑定追踪
    bound_texture_2d: [GLuint; MAX_TEX_UNITS],
}

impl GLES1OnWebGL2State {
    fn new() -> Self {
        GLES1OnWebGL2State {
            pointer_is_fixed_point: [false; ARRAYS.len()],
            fixed_point_texture_units: HashSet::new(),
            fixed_point_translation_buffers: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            matrix_mode: gles11::MODELVIEW,
            modelview_stack: vec![Matrix::<4>::identity()],
            projection_stack: vec![Matrix::<4>::identity()],
            texture_stack: vec![Matrix::<4>::identity()],
            current_color: [1.0, 1.0, 1.0, 1.0],
            current_normal: [0.0, 0.0, 1.0],
            arrays: [ArrayState::default(); ARRAYS.len()],
            client_active_texture: gles11::TEXTURE0,
            active_texture: gles11::TEXTURE0,
            array_buffer_binding: 0,
            element_array_buffer_binding: 0,
            tex_env_mode: [gles11::MODULATE; MAX_TEX_UNITS],
            blend_enabled: false,
            blend_sfactor: gles11::ONE,
            blend_dfactor: gles11::ZERO,
            alpha_test_enabled: false,
            alpha_func: gles11::ALWAYS,
            alpha_ref: 0.0,
            texture_2d_enabled: false,
            depth_test_enabled: false,
            depth_func: gles11::LESS,
            cull_enabled: false,
            cull_face: gles11::BACK,
            front_face: gles11::CCW,
            scissor_enabled: false,
            scissor: (0, 0, 0, 0),
            clear_color: [0.0, 0.0, 0.0, 0.0],
            viewport: (0, 0, 0, 0),
            bound_texture_2d: [0; MAX_TEX_UNITS],
        }
    }

    /// 当前活动纹理单元的索引(0..MAX_TEX_UNITS)。
    fn active_tex_idx(&self) -> usize {
        ((self.active_texture - gles11::TEXTURE0) as usize).min(MAX_TEX_UNITS - 1)
    }

    fn top_modelview(&self) -> &Matrix<4> {
        self.modelview_stack.last().unwrap()
    }
    fn top_projection(&self) -> &Matrix<4> {
        self.projection_stack.last().unwrap()
    }
    fn top_texture(&self) -> &Matrix<4> {
        self.texture_stack.last().unwrap()
    }

    /// 取当前矩阵模式对应的栈顶可变引用。
    fn current_stack_top_mut(&mut self) -> &mut Matrix<4> {
        match self.matrix_mode {
            gles11::PROJECTION => self.projection_stack.last_mut().unwrap(),
            gles11::TEXTURE => self.texture_stack.last_mut().unwrap(),
            _ => self.modelview_stack.last_mut().unwrap(), // MODELVIEW
        }
    }
}

// ───────────────────────────── Context ─────────────────────────────

pub struct GLES1OnWebGL2Context {
    gl_ctx: GLContext,
    state: GLES1OnWebGL2State,
    program: Option<ProgramCache>,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
    is_loaded: bool,
}

impl GLESContext for GLES1OnWebGL2Context {
    fn description() -> &'static str {
        "OpenGL ES 1.1 via touchHLE GLES1-on-WebGL2 layer"
    }

    fn new(window: &mut Window) -> Result<Self, String> {
        Ok(Self {
            gl_ctx: window.create_gl_context(GLVersion::WebGL2)?,
            state: GLES1OnWebGL2State::new(),
            program: None,
            vao: 0,
            vbo: 0,
            ebo: 0,
            is_loaded: false,
        })
    }

    fn make_current<'gl_ctx, 'win: 'gl_ctx>(
        &'gl_ctx mut self,
        window: &'win mut Window,
    ) -> Box<dyn GLES + 'gl_ctx> {
        if self.gl_ctx.is_current() && self.is_loaded {
            return Box::new(GLES1OnWebGL2 {
                state: &mut self.state,
                program: self.program.as_ref().unwrap(),
                vao: self.vao,
                vbo: self.vbo,
                ebo: self.ebo,
            });
        }

        unsafe {
            window.make_gl_context_current(&self.gl_ctx);
        }
        gl30::load_with(|s| window.gl_get_proc_address(s));
        self.first_time_init();
        self.is_loaded = true;

        Box::new(GLES1OnWebGL2 {
            state: &mut self.state,
            program: self.program.as_ref().unwrap(),
            vao: self.vao,
            vbo: self.vbo,
            ebo: self.ebo,
        })
    }

    unsafe fn make_current_unchecked_for_window<'gl_ctx>(
        &'gl_ctx mut self,
        make_current_fn: &mut dyn FnMut(&GLContext),
        loader_fn: &mut dyn FnMut(&'static str) -> *const std::ffi::c_void,
    ) -> Box<dyn GLES + 'gl_ctx> {
        if self.gl_ctx.is_current() && self.is_loaded {
            return Box::new(GLES1OnWebGL2 {
                state: &mut self.state,
                program: self.program.as_ref().unwrap(),
                vao: self.vao,
                vbo: self.vbo,
                ebo: self.ebo,
            });
        }

        make_current_fn(&self.gl_ctx);
        gl30::load_with(&mut *loader_fn);
        self.first_time_init();
        self.is_loaded = true;

        Box::new(GLES1OnWebGL2 {
            state: &mut self.state,
            program: self.program.as_ref().unwrap(),
            vao: self.vao,
            vbo: self.vbo,
            ebo: self.ebo,
        })
    }
}

impl GLES1OnWebGL2Context {
    /// 首次 make_current:编译着色器 + 生成持久 VAO/VBO/EBO。
    /// WebGL2 禁止 client-side 顶点数组,且严格模式下默认 VAO 0 不可用于配置 attrib,
    /// 所以我们主动生成一个非 0 VAO 全程绑定(与桌面后端「绝不绑非 0 VAO」相反)。
    fn first_time_init(&mut self) {
        if self.program.is_some() {
            return;
        }
        unsafe {
            self.program = Some(compile_program());
            gl30::GenVertexArrays(1, &mut self.vao);
            gl30::BindVertexArray(self.vao);
            gl30::GenBuffers(1, &mut self.vbo);
            gl30::GenBuffers(1, &mut self.ebo);
        }
        log!("[WASM] GLES1OnWebGL2: 着色器编译 + 持久 VAO/VBO/EBO 就绪");
    }
}

/// 编译并 link uber-shader,缓存 uniform location。
unsafe fn compile_program() -> ProgramCache {
    let vs = compile_shader(gl30::VERTEX_SHADER, VERTEX_SRC);
    let fs = compile_shader(gl30::FRAGMENT_SHADER, FRAGMENT_SRC);
    let prog = gl30::CreateProgram();
    gl30::AttachShader(prog, vs);
    gl30::AttachShader(prog, fs);
    gl30::LinkProgram(prog);

    let mut status: GLint = 0;
    gl30::GetProgramiv(prog, gl30::LINK_STATUS, &mut status);
    if status == 0 {
        let mut log_buf = [0u8; 1024];
        let mut len: GLsizei = 0;
        gl30::GetProgramInfoLog(prog, 1024, &mut len, log_buf.as_mut_ptr() as *mut _);
        let msg = String::from_utf8_lossy(&log_buf[..len.max(0) as usize]).into_owned();
        panic!("GLES1OnWebGL2 着色器 link 失败: {}", msg);
    }
    // 链接后即可删除 shader 对象(程序已持有副本)。
    gl30::DeleteShader(vs);
    gl30::DeleteShader(fs);

    let loc = |name: &[u8]| -> GLint { gl30::GetUniformLocation(prog, name.as_ptr() as *const _) };
    ProgramCache {
        prog,
        loc_mvp: loc(b"u_mvp\0"),
        loc_tex_matrix: loc(b"u_texMatrix\0"),
        loc_const_color: loc(b"u_constColor\0"),
        loc_has_color_array: loc(b"u_hasColorArray\0"),
        loc_tex0: loc(b"u_tex0\0"),
        loc_tex_enable: loc(b"u_texEnable\0"),
        loc_tex_env_mode: loc(b"u_texEnvMode\0"),
        loc_alpha_test: loc(b"u_alphaTest\0"),
        loc_alpha_func: loc(b"u_alphaFunc\0"),
        loc_alpha_ref: loc(b"u_alphaRef\0"),
    }
}

unsafe fn compile_shader(kind: GLenum, src: &str) -> GLuint {
    let shader = gl30::CreateShader(kind);
    let ptr = src.as_ptr() as *const GLchar;
    let len = src.len() as GLint;
    gl30::ShaderSource(shader, 1, &ptr, &len);
    gl30::CompileShader(shader);
    let mut status: GLint = 0;
    gl30::GetShaderiv(shader, gl30::COMPILE_STATUS, &mut status);
    if status == 0 {
        let mut log_buf = [0u8; 1024];
        let mut log_len: GLsizei = 0;
        gl30::GetShaderInfoLog(shader, 1024, &mut log_len, log_buf.as_mut_ptr() as *mut _);
        let msg = String::from_utf8_lossy(&log_buf[..log_len.max(0) as usize]).into_owned();
        panic!(
            "GLES1OnWebGL2 shader 编译失败 (kind={:#x}): {}",
            kind, msg
        );
    }
    shader
}

// ───────────────────────────── GLES impl ─────────────────────────────

pub struct GLES1OnWebGL2<'a> {
    state: &'a mut GLES1OnWebGL2State,
    program: &'a ProgramCache,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
}

impl GLES1OnWebGL2<'_> {
    // ── 矩阵构造工具(Matrix<4> 只内置 identity/translate_3d/multiply,其余手写)──

    /// 列主序 4x4 缩放矩阵。
    fn make_scale(x: f32, y: f32, z: f32) -> Matrix<4> {
        Matrix::<4>::from_columns([
            [x, 0.0, 0.0, 0.0],
            [0.0, y, 0.0, 0.0],
            [0.0, 0.0, z, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// 列主序绕任意轴旋转矩阵(angle 单位:度),等价 glRotatef。
    fn make_rotate(angle_deg: f32, x: f32, y: f32, z: f32) -> Matrix<4> {
        let len = (x * x + y * y + z * z).sqrt();
        if len == 0.0 {
            return Matrix::<4>::identity();
        }
        let (x, y, z) = (x / len, y / len, z / len);
        let a = angle_deg.to_radians();
        let c = a.cos();
        let s = a.sin();
        let omc = 1.0 - c;
        // 行主序(数学)旋转矩阵的元素 m[row][col]:
        let m00 = x * x * omc + c;
        let m01 = x * y * omc - z * s;
        let m02 = x * z * omc + y * s;
        let m10 = y * x * omc + z * s;
        let m11 = y * y * omc + c;
        let m12 = y * z * omc - x * s;
        let m20 = z * x * omc - y * s;
        let m21 = z * y * omc + x * s;
        let m22 = z * z * omc + c;
        // Matrix<4> 是列主序:from_columns 的每个内层数组是一【列】。
        Matrix::<4>::from_columns([
            [m00, m10, m20, 0.0],
            [m01, m11, m21, 0.0],
            [m02, m12, m22, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ])
    }

    /// 列主序正交投影矩阵(等价 glOrthof)。
    fn make_ortho(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Matrix<4> {
        let tx = -(r + l) / (r - l);
        let ty = -(t + b) / (t - b);
        let tz = -(f + n) / (f - n);
        Matrix::<4>::from_columns([
            [2.0 / (r - l), 0.0, 0.0, 0.0],
            [0.0, 2.0 / (t - b), 0.0, 0.0],
            [0.0, 0.0, -2.0 / (f - n), 0.0],
            [tx, ty, tz, 1.0],
        ])
    }

    /// 列主序透视投影矩阵(等价 glFrustumf)。
    fn make_frustum(l: f32, r: f32, b: f32, t: f32, n: f32, f: f32) -> Matrix<4> {
        let a = (r + l) / (r - l);
        let bb = (t + b) / (t - b);
        let cc = -(f + n) / (f - n);
        let dd = -(2.0 * f * n) / (f - n);
        Matrix::<4>::from_columns([
            [2.0 * n / (r - l), 0.0, 0.0, 0.0],
            [0.0, 2.0 * n / (t - b), 0.0, 0.0],
            [a, bb, cc, -1.0],
            [0.0, 0.0, dd, 0.0],
        ])
    }

    /// 从 *const GLfloat 读 16 个分量构成列主序 Matrix<4>(GL 矩阵本就是列主序)。
    unsafe fn matrix_from_ptr(m: *const GLfloat) -> Matrix<4> {
        let mut cols = [[0.0f32; 4]; 4];
        for (c, col) in cols.iter_mut().enumerate() {
            for (r, cell) in col.iter_mut().enumerate() {
                *cell = m.add(c * 4 + r).read_unaligned();
            }
        }
        Matrix::<4>::from_columns(cols)
    }

    /// 把 *= other(右乘),等价固定管线的 glMultMatrix(current = current * m)。
    fn mult_current(&mut self, m: &Matrix<4>) {
        let top = self.state.current_stack_top_mut();
        *top = top.multiply(m);
    }

    // ── 定点数组转换:照搬 gles1_on_gl2,但输出到 float Vec(draw 时打进 VBO)──
    //
    // 对每个标记为定点的、且 enabled 的数组,从 client 内存读 16.16 定点转 float,
    // 存进 fixed_point_translation_buffers[i]。draw 时若该数组是定点,就改用这段
    // float buffer 作为源(type=FLOAT、紧凑排列)。
    unsafe fn translate_fixed_point_arrays(&mut self, first: GLint, count: GLsizei) {
        for i in 0..ARRAYS.len() {
            if !self.state.pointer_is_fixed_point[i] {
                continue;
            }
            let arr = self.state.arrays[i];
            if !arr.enabled {
                continue;
            }
            // 纹理坐标数组:只在当前活动单元为定点时才转换。
            if ARRAYS[i].name == gl21::TEXTURE_COORD_ARRAY
                && !self
                    .state
                    .fixed_point_texture_units
                    .contains(&self.state.active_texture)
            {
                continue;
            }

            let size = ARRAYS[i].size.map(|_| arr.size).unwrap_or(3); // NORMAL=3
            let stride = if arr.stride == 0 {
                size * 4 // 紧凑模式:sizeof(GLfixed)=4
            } else {
                arr.stride
            };
            let total = ((first + count) * size).max(0) as usize;

            let buffer = &mut self.state.fixed_point_translation_buffers[i];
            buffer.clear();
            buffer.resize(total, 0.0);

            // 只有 client 内存(buffer_binding==0)能直接读;VBO 里的定点暂不支持
            // (cocos2d 的定点数组都是 client 内存,实测够用)。
            if arr.buffer_binding == 0 && !arr.pointer.is_null() {
                let base = arr.pointer as *const u8;
                let size_u = size as usize;
                let stride_u = stride as usize;
                let first_u = first.max(0) as usize;
                let count_u = count.max(0) as usize;
                for j in first_u..(first_u + count_u) {
                    let vec_ptr = base.add(j * stride_u) as *const GLfixed;
                    for k in 0..size_u {
                        buffer[j * size_u + k] = fixed_to_float(vec_ptr.add(k).read_unaligned());
                    }
                }
            }
        }
    }

    /// 计算某数组单个顶点的字节步长(stride==0 时按紧凑模式推导)。
    fn array_stride_bytes(arr: &ArrayState) -> usize {
        if arr.stride != 0 {
            return arr.stride as usize;
        }
        let comp = gl_type_size(arr.type_);
        comp * arr.size.max(0) as usize
    }

    /// 把所有 enabled 的数组打进持久 VBO,并配置 vertex attrib。
    /// `vertex_count` = 需要的顶点总数(DrawArrays:first+count;DrawElements:max index+1)。
    unsafe fn upload_arrays_and_setup(&mut self, vertex_count: usize) {
        gl30::BindVertexArray(self.vao);
        gl30::BindBuffer(gl30::ARRAY_BUFFER, self.vbo);

        // 收集每个 attrib 的源数据(host 可读的字节切片)与布局。
        // staging buffer:把三类数组紧凑追加进同一个 VBO,分别记录其 offset。
        let mut staging: Vec<u8> = Vec::new();

        // 局部辅助闭包无法借 self,改用显式块。
        struct Plan {
            loc: GLuint,
            size: GLint,
            gl_type: GLenum,
            normalized: GLboolean,
            offset: usize,
        }
        let mut plans: Vec<Plan> = Vec::new();

        // 处理顺序:VERTEX / COLOR / TEXCOORD。
        for (idx, loc) in [
            (ARRAY_VERTEX, LOC_POSITION),
            (ARRAY_COLOR, LOC_COLOR),
            (ARRAY_TEXCOORD, LOC_TEXCOORD),
        ] {
            let arr = self.state.arrays[idx];
            if !arr.enabled {
                gl30::DisableVertexAttribArray(loc);
                continue;
            }

            // 定点数组改用已转好的 float buffer(紧凑、FLOAT)。
            let is_fixed = self.state.pointer_is_fixed_point[idx] && arr.type_ == gles11::FIXED;
            let offset = staging.len();

            if is_fixed {
                let fbuf = &self.state.fixed_point_translation_buffers[idx];
                let need = vertex_count * arr.size.max(0) as usize;
                let avail = fbuf.len().min(need);
                let bytes = std::slice::from_raw_parts(
                    fbuf.as_ptr() as *const u8,
                    avail * std::mem::size_of::<GLfloat>(),
                );
                staging.extend_from_slice(bytes);
                plans.push(Plan {
                    loc,
                    size: arr.size,
                    gl_type: gles11::FLOAT,
                    normalized: gles11::FALSE,
                    offset,
                });
            } else if arr.buffer_binding == 0 {
                // client 内存:按 stride 逐顶点 memcpy 紧凑进 staging。
                let stride = Self::array_stride_bytes(&arr);
                let comp = gl_type_size(arr.type_) * arr.size.max(0) as usize;
                if !arr.pointer.is_null() && stride > 0 && comp > 0 {
                    let base = arr.pointer as *const u8;
                    for v in 0..vertex_count {
                        let src = std::slice::from_raw_parts(base.add(v * stride), comp);
                        staging.extend_from_slice(src);
                    }
                }
                plans.push(Plan {
                    loc,
                    size: arr.size,
                    gl_type: arr.type_,
                    normalized: color_needs_normalize(idx, arr.type_),
                    offset,
                });
            } else {
                // 数组源在 VBO 里:本后端首版不支持(把它打进 staging 需 MapBuffer),
                // 直接禁用该 attrib(标题画面用 client 数组,不触发)。
                gl30::DisableVertexAttribArray(loc);
                continue;
            }
        }

        // 一次性把 staging 传进 VBO。
        if !staging.is_empty() {
            gl30::BufferData(
                gl30::ARRAY_BUFFER,
                staging.len() as GLsizeiptr,
                staging.as_ptr() as *const GLvoid,
                gl30::DYNAMIC_DRAW,
            );
        }

        // 配置每个 attrib(offset 现在是 VBO 内偏移)。
        for p in &plans {
            gl30::EnableVertexAttribArray(p.loc);
            gl30::VertexAttribPointer(
                p.loc,
                p.size,
                p.gl_type,
                p.normalized,
                0, // 紧凑追加,stride=0
                p.offset as *const GLvoid,
            );
        }
    }

    /// 传 uniform + 绑纹理 + 应用固定功能硬件状态,然后 UseProgram。
    unsafe fn setup_draw_state(&mut self) {
        let prog = self.program;
        gl30::UseProgram(prog.prog);

        // u_mvp = projection * modelview
        let mvp = self
            .state
            .top_projection()
            .multiply(self.state.top_modelview());
        gl30::UniformMatrix4fv(
            prog.loc_mvp,
            1,
            gles11::FALSE,
            mvp.columns().as_ptr() as *const GLfloat,
        );
        gl30::UniformMatrix4fv(
            prog.loc_tex_matrix,
            1,
            gles11::FALSE,
            self.state.top_texture().columns().as_ptr() as *const GLfloat,
        );

        let has_color = self.state.arrays[ARRAY_COLOR].enabled;
        gl30::Uniform1i(prog.loc_has_color_array, has_color as GLint);
        let cc = self.state.current_color;
        gl30::Uniform4f(prog.loc_const_color, cc[0], cc[1], cc[2], cc[3]);

        let tex_enable =
            self.state.texture_2d_enabled && self.state.arrays[ARRAY_TEXCOORD].enabled;
        gl30::Uniform1i(prog.loc_tex_enable, tex_enable as GLint);
        let env_mode = if self.state.tex_env_mode[self.state.active_tex_idx()] == gles11::REPLACE {
            1
        } else {
            0
        };
        gl30::Uniform1i(prog.loc_tex_env_mode, env_mode);
        gl30::Uniform1i(prog.loc_tex0, 0);

        gl30::Uniform1i(prog.loc_alpha_test, self.state.alpha_test_enabled as GLint);
        gl30::Uniform1i(prog.loc_alpha_func, self.state.alpha_func as GLint);
        gl30::Uniform1f(prog.loc_alpha_ref, self.state.alpha_ref);

        // 绑定纹理单元 0 的纹理。
        gl30::ActiveTexture(gles11::TEXTURE0);
        gl30::BindTexture(gles11::TEXTURE_2D, self.state.bound_texture_2d[0]);

        // 固定功能硬件状态(着色器管不了的)。
        self.apply_fixed_function_state();
    }

    /// 把 State 里的 blend/depth/cull/scissor 同步到真实 GL。
    unsafe fn apply_fixed_function_state(&mut self) {
        if self.state.blend_enabled {
            gl30::Enable(gles11::BLEND);
            gl30::BlendFunc(self.state.blend_sfactor, self.state.blend_dfactor);
        } else {
            gl30::Disable(gles11::BLEND);
        }
        if self.state.depth_test_enabled {
            gl30::Enable(gles11::DEPTH_TEST);
            gl30::DepthFunc(self.state.depth_func);
        } else {
            gl30::Disable(gles11::DEPTH_TEST);
        }
        if self.state.cull_enabled {
            gl30::Enable(gles11::CULL_FACE);
            gl30::CullFace(self.state.cull_face);
            gl30::FrontFace(self.state.front_face);
        } else {
            gl30::Disable(gles11::CULL_FACE);
        }
        if self.state.scissor_enabled {
            gl30::Enable(gles11::SCISSOR_TEST);
            let (x, y, w, h) = self.state.scissor;
            gl30::Scissor(x, y, w, h);
        } else {
            gl30::Disable(gles11::SCISSOR_TEST);
        }
    }
}

/// 某个 GL 标量类型的字节大小。
fn gl_type_size(type_: GLenum) -> usize {
    match type_ {
        gles11::BYTE | gles11::UNSIGNED_BYTE => 1,
        gles11::SHORT | gles11::UNSIGNED_SHORT => 2,
        gles11::FLOAT | gles11::FIXED | gl30::INT | gl30::UNSIGNED_INT => 4,
        _ => 4,
    }
}

/// COLOR_ARRAY 若是 UNSIGNED_BYTE,需 normalized=TRUE(0-255 → 0-1)。
fn color_needs_normalize(array_idx: usize, type_: GLenum) -> GLboolean {
    if array_idx == ARRAY_COLOR
        && (type_ == gles11::UNSIGNED_BYTE
            || type_ == gles11::BYTE
            || type_ == gles11::UNSIGNED_SHORT
            || type_ == gles11::SHORT)
    {
        gles11::TRUE
    } else {
        gles11::FALSE
    }
}

impl GLES for GLES1OnWebGL2<'_> {
    unsafe fn driver_description(&self) -> String {
        String::from("OpenGL ES 1.1 via WebGL2 (touchHLE wasm)")
    }

    // ── 通用状态 ──
    unsafe fn GetError(&mut self) -> GLenum {
        gl30::GetError()
    }
    unsafe fn Enable(&mut self, cap: GLenum) {
        match cap {
            gles11::BLEND => self.state.blend_enabled = true,
            gles11::ALPHA_TEST => self.state.alpha_test_enabled = true,
            gles11::TEXTURE_2D => self.state.texture_2d_enabled = true,
            gles11::DEPTH_TEST => self.state.depth_test_enabled = true,
            gles11::CULL_FACE => self.state.cull_enabled = true,
            gles11::SCISSOR_TEST => self.state.scissor_enabled = true,
            // 灯光/雾/材质/点精灵/dither 等:固定管线被着色器替代,忽略。
            _ => {}
        }
    }
    unsafe fn IsEnabled(&mut self, cap: GLenum) -> GLboolean {
        let on = match cap {
            gles11::BLEND => self.state.blend_enabled,
            gles11::ALPHA_TEST => self.state.alpha_test_enabled,
            gles11::TEXTURE_2D => self.state.texture_2d_enabled,
            gles11::DEPTH_TEST => self.state.depth_test_enabled,
            gles11::CULL_FACE => self.state.cull_enabled,
            gles11::SCISSOR_TEST => self.state.scissor_enabled,
            gles11::COLOR_ARRAY => self.state.arrays[ARRAY_COLOR].enabled,
            gles11::NORMAL_ARRAY => self.state.arrays[ARRAY_NORMAL].enabled,
            gles11::TEXTURE_COORD_ARRAY => self.state.arrays[ARRAY_TEXCOORD].enabled,
            gles11::VERTEX_ARRAY => self.state.arrays[ARRAY_VERTEX].enabled,
            _ => false,
        };
        if on {
            gles11::TRUE
        } else {
            gles11::FALSE
        }
    }
    unsafe fn Disable(&mut self, cap: GLenum) {
        match cap {
            gles11::BLEND => self.state.blend_enabled = false,
            gles11::ALPHA_TEST => self.state.alpha_test_enabled = false,
            gles11::TEXTURE_2D => self.state.texture_2d_enabled = false,
            gles11::DEPTH_TEST => self.state.depth_test_enabled = false,
            gles11::CULL_FACE => self.state.cull_enabled = false,
            gles11::SCISSOR_TEST => self.state.scissor_enabled = false,
            _ => {}
        }
    }
    unsafe fn ClientActiveTexture(&mut self, texture: GLenum) {
        self.state.client_active_texture = texture;
    }
    unsafe fn EnableClientState(&mut self, array: GLenum) {
        if let Some(i) = ARRAYS.iter().position(|a| a.name == array) {
            self.state.arrays[i].enabled = true;
        }
    }
    unsafe fn DisableClientState(&mut self, array: GLenum) {
        if let Some(i) = ARRAYS.iter().position(|a| a.name == array) {
            self.state.arrays[i].enabled = false;
        }
    }
    unsafe fn GetBooleanv(&mut self, pname: GLenum, params: *mut GLboolean) {
        let val = self.IsEnabled(pname);
        *params = val;
    }
    unsafe fn GetFloatv(&mut self, pname: GLenum, params: *mut GLfloat) {
        match pname {
            gles11::CURRENT_COLOR => {
                for i in 0..4 {
                    *params.add(i) = self.state.current_color[i];
                }
            }
            gles11::COLOR_CLEAR_VALUE => {
                for i in 0..4 {
                    *params.add(i) = self.state.clear_color[i];
                }
            }
            gles11::MODELVIEW_MATRIX => {
                let cols = self.state.top_modelview().columns();
                for c in 0..4 {
                    for r in 0..4 {
                        *params.add(c * 4 + r) = cols[c][r];
                    }
                }
            }
            gles11::PROJECTION_MATRIX => {
                let cols = self.state.top_projection().columns();
                for c in 0..4 {
                    for r in 0..4 {
                        *params.add(c * 4 + r) = cols[c][r];
                    }
                }
            }
            gles11::TEXTURE_MATRIX => {
                let cols = self.state.top_texture().columns();
                for c in 0..4 {
                    for r in 0..4 {
                        *params.add(c * 4 + r) = cols[c][r];
                    }
                }
            }
            gles11::ALPHA_TEST_REF => *params = self.state.alpha_ref,
            _ => {
                // 其它整型参数转 float 返回。
                let (_t, count) = GET_PARAMS.get_type_info(pname);
                let mut tmp = [0 as GLint; 4];
                self.GetIntegerv(pname, tmp.as_mut_ptr());
                for i in 0..count as usize {
                    *params.add(i) = tmp[i] as GLfloat;
                }
            }
        }
    }
    unsafe fn GetIntegerv(&mut self, pname: GLenum, params: *mut GLint) {
        // 确保 pname 已登记(否则 panic,便于补表)。
        GET_PARAMS.assert_known_param(pname);
        match pname {
            gles11::ACTIVE_TEXTURE => *params = self.state.active_texture as GLint,
            gles11::CLIENT_ACTIVE_TEXTURE => {
                *params = self.state.client_active_texture as GLint
            }
            gles11::MATRIX_MODE => *params = self.state.matrix_mode as GLint,
            gles11::VIEWPORT => {
                let (x, y, w, h) = self.state.viewport;
                *params = x;
                *params.add(1) = y;
                *params.add(2) = w;
                *params.add(3) = h;
            }
            gles11::SCISSOR_BOX => {
                let (x, y, w, h) = self.state.scissor;
                *params = x;
                *params.add(1) = y;
                *params.add(2) = w;
                *params.add(3) = h;
            }
            gles11::MAX_TEXTURE_SIZE => *params = 4096,
            gles11::MAX_TEXTURE_UNITS => *params = MAX_TEX_UNITS as GLint,
            gles11::TEXTURE_BINDING_2D => {
                *params = self.state.bound_texture_2d[self.state.active_tex_idx()] as GLint
            }
            gles11::BLEND_SRC => *params = self.state.blend_sfactor as GLint,
            gles11::BLEND_DST => *params = self.state.blend_dfactor as GLint,
            gles11::DEPTH_FUNC => *params = self.state.depth_func as GLint,
            gles11::ALPHA_TEST_FUNC => *params = self.state.alpha_func as GLint,
            gles11::ARRAY_BUFFER_BINDING => *params = self.state.array_buffer_binding as GLint,
            gles11::ELEMENT_ARRAY_BUFFER_BINDING => {
                *params = self.state.element_array_buffer_binding as GLint
            }
            gles11::FRAMEBUFFER_BINDING_OES => gl30::GetIntegerv(gl30::FRAMEBUFFER_BINDING, params),
            gles11::RENDERBUFFER_BINDING_OES => {
                gl30::GetIntegerv(gl30::RENDERBUFFER_BINDING, params)
            }
            // 数组属性
            gles11::COLOR_ARRAY_SIZE => *params = self.state.arrays[ARRAY_COLOR].size,
            gles11::COLOR_ARRAY_TYPE => *params = self.state.arrays[ARRAY_COLOR].type_ as GLint,
            gles11::COLOR_ARRAY_STRIDE => *params = self.state.arrays[ARRAY_COLOR].stride,
            gles11::COLOR_ARRAY_BUFFER_BINDING => {
                *params = self.state.arrays[ARRAY_COLOR].buffer_binding as GLint
            }
            gles11::NORMAL_ARRAY_TYPE => *params = self.state.arrays[ARRAY_NORMAL].type_ as GLint,
            gles11::NORMAL_ARRAY_STRIDE => *params = self.state.arrays[ARRAY_NORMAL].stride,
            gles11::NORMAL_ARRAY_BUFFER_BINDING => {
                *params = self.state.arrays[ARRAY_NORMAL].buffer_binding as GLint
            }
            gles11::TEXTURE_COORD_ARRAY_SIZE => *params = self.state.arrays[ARRAY_TEXCOORD].size,
            gles11::TEXTURE_COORD_ARRAY_TYPE => {
                *params = self.state.arrays[ARRAY_TEXCOORD].type_ as GLint
            }
            gles11::TEXTURE_COORD_ARRAY_STRIDE => {
                *params = self.state.arrays[ARRAY_TEXCOORD].stride
            }
            gles11::TEXTURE_COORD_ARRAY_BUFFER_BINDING => {
                *params = self.state.arrays[ARRAY_TEXCOORD].buffer_binding as GLint
            }
            gles11::VERTEX_ARRAY_SIZE => *params = self.state.arrays[ARRAY_VERTEX].size,
            gles11::VERTEX_ARRAY_TYPE => *params = self.state.arrays[ARRAY_VERTEX].type_ as GLint,
            gles11::VERTEX_ARRAY_STRIDE => *params = self.state.arrays[ARRAY_VERTEX].stride,
            gles11::VERTEX_ARRAY_BUFFER_BINDING => {
                *params = self.state.arrays[ARRAY_VERTEX].buffer_binding as GLint
            }
            // 布尔 capability 当整型查
            gles11::ALPHA_TEST
            | gles11::BLEND
            | gles11::DEPTH_TEST
            | gles11::SCISSOR_TEST
            | gles11::CULL_FACE
            | gles11::TEXTURE_2D
            | gles11::COLOR_ARRAY
            | gles11::NORMAL_ARRAY
            | gles11::TEXTURE_COORD_ARRAY
            | gles11::VERTEX_ARRAY => {
                *params = if self.IsEnabled(pname) == gles11::TRUE {
                    1
                } else {
                    0
                }
            }
            _ => {
                // 已登记但没显式处理的:返回 0(保守)。
                *params = 0;
            }
        }
    }
    unsafe fn GetTexEnviv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        assert_eq!(target, gles11::TEXTURE_ENV);
        if pname == gles11::TEXTURE_ENV_MODE {
            *params = self.state.tex_env_mode[self.state.active_tex_idx()] as GLint;
        } else {
            *params = 0;
        }
    }
    unsafe fn GetTexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *mut GLfloat) {
        assert_eq!(target, gles11::TEXTURE_ENV);
        if pname == gles11::TEXTURE_ENV_MODE {
            *params = self.state.tex_env_mode[self.state.active_tex_idx()] as GLfloat;
        } else {
            *params = 0.0;
        }
    }
    unsafe fn GetPointerv(&mut self, pname: GLenum, params: *mut *const GLvoid) {
        if let Some(i) = ARRAYS.iter().position(|a| a.pointer == pname) {
            *params = self.state.arrays[i].pointer;
        } else {
            *params = std::ptr::null();
        }
    }
    unsafe fn Hint(&mut self, _target: GLenum, _mode: GLenum) {
        // 固定管线提示在着色器路径无意义,忽略。
    }
    unsafe fn Finish(&mut self) {
        gl30::Finish();
    }
    unsafe fn Flush(&mut self) {
        gl30::Flush();
    }
    unsafe fn GetString(&mut self, _name: GLenum) -> *const GLubyte {
        // 返回固定字符串指针(静态),供偶发查询。
        b"OpenGL ES 1.1 (WebGL2)\0".as_ptr()
    }

    // ── 其它状态 ──
    unsafe fn AlphaFunc(&mut self, func: GLenum, ref_: GLclampf) {
        self.state.alpha_func = func;
        self.state.alpha_ref = ref_;
    }
    unsafe fn AlphaFuncx(&mut self, func: GLenum, ref_: GLclampx) {
        self.state.alpha_func = func;
        self.state.alpha_ref = fixed_to_float(ref_);
    }
    unsafe fn BlendFunc(&mut self, sfactor: GLenum, dfactor: GLenum) {
        self.state.blend_sfactor = sfactor;
        self.state.blend_dfactor = dfactor;
    }
    unsafe fn BlendEquationOES(&mut self, _mode: GLenum) {
        // 首版只支持默认 ADD,忽略(ES3 有 BlendEquation,后续可转发)。
    }
    unsafe fn ColorMask(
        &mut self,
        red: GLboolean,
        green: GLboolean,
        blue: GLboolean,
        alpha: GLboolean,
    ) {
        gl30::ColorMask(red, green, blue, alpha);
    }
    unsafe fn ClipPlanef(&mut self, _plane: GLenum, _equation: *const GLfloat) {}
    unsafe fn ClipPlanex(&mut self, _plane: GLenum, _equation: *const GLfixed) {}
    unsafe fn CullFace(&mut self, mode: GLenum) {
        self.state.cull_face = mode;
    }
    unsafe fn DepthFunc(&mut self, func: GLenum) {
        self.state.depth_func = func;
    }
    unsafe fn DepthMask(&mut self, flag: GLboolean) {
        gl30::DepthMask(flag);
    }
    unsafe fn DepthRangef(&mut self, near: GLclampf, far: GLclampf) {
        gl30::DepthRangef(near, far);
    }
    unsafe fn DepthRangex(&mut self, near: GLclampx, far: GLclampx) {
        gl30::DepthRangef(fixed_to_float(near), fixed_to_float(far));
    }
    unsafe fn FrontFace(&mut self, mode: GLenum) {
        self.state.front_face = mode;
    }
    unsafe fn PolygonOffset(&mut self, factor: GLfloat, units: GLfloat) {
        gl30::PolygonOffset(factor, units);
    }
    unsafe fn PolygonOffsetx(&mut self, factor: GLfixed, units: GLfixed) {
        gl30::PolygonOffset(fixed_to_float(factor), fixed_to_float(units));
    }
    unsafe fn SampleCoverage(&mut self, value: GLclampf, invert: GLboolean) {
        gl30::SampleCoverage(value, invert);
    }
    unsafe fn SampleCoveragex(&mut self, value: GLclampx, invert: GLboolean) {
        gl30::SampleCoverage(fixed_to_float(value), invert);
    }
    unsafe fn ShadeModel(&mut self, _mode: GLenum) {
        // 只支持 GL_SMOOTH 语义,忽略。
    }
    unsafe fn Scissor(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        self.state.scissor = (x, y, width, height);
        gl30::Scissor(x, y, width, height);
    }
    unsafe fn Viewport(&mut self, x: GLint, y: GLint, width: GLsizei, height: GLsizei) {
        self.state.viewport = (x, y, width, height);
        gl30::Viewport(x, y, width, height);
    }
    unsafe fn LineWidth(&mut self, val: GLfloat) {
        gl30::LineWidth(val);
    }
    unsafe fn LineWidthx(&mut self, val: GLfixed) {
        gl30::LineWidth(fixed_to_float(val));
    }
    unsafe fn StencilFunc(&mut self, func: GLenum, ref_: GLint, mask: GLuint) {
        gl30::StencilFunc(func, ref_, mask);
    }
    unsafe fn StencilOp(&mut self, sfail: GLenum, dpfail: GLenum, dppass: GLenum) {
        gl30::StencilOp(sfail, dpfail, dppass);
    }
    unsafe fn StencilMask(&mut self, mask: GLuint) {
        gl30::StencilMask(mask);
    }
    unsafe fn LogicOp(&mut self, _opcode: GLenum) {
        // WebGL2 无 glLogicOp,忽略。
    }

    // ── 点 ──
    unsafe fn PointSize(&mut self, _size: GLfloat) {}
    unsafe fn PointSizex(&mut self, _size: GLfixed) {}
    unsafe fn PointParameterf(&mut self, _pname: GLenum, _param: GLfloat) {}
    unsafe fn PointParameterx(&mut self, _pname: GLenum, _param: GLfixed) {}
    unsafe fn PointParameterfv(&mut self, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn PointParameterxv(&mut self, _pname: GLenum, _params: *const GLfixed) {}

    // ── 灯光/材质/雾(全部 no-op,2D 渲染不用)──
    unsafe fn Fogf(&mut self, _pname: GLenum, _param: GLfloat) {}
    unsafe fn Fogx(&mut self, _pname: GLenum, _param: GLfixed) {}
    unsafe fn Fogfv(&mut self, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn Fogxv(&mut self, _pname: GLenum, _params: *const GLfixed) {}
    unsafe fn Lightf(&mut self, _light: GLenum, _pname: GLenum, _param: GLfloat) {}
    unsafe fn Lightx(&mut self, _light: GLenum, _pname: GLenum, _param: GLfixed) {}
    unsafe fn Lightfv(&mut self, _light: GLenum, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn Lightxv(&mut self, _light: GLenum, _pname: GLenum, _params: *const GLfixed) {}
    unsafe fn LightModelf(&mut self, _pname: GLenum, _param: GLfloat) {}
    unsafe fn LightModelx(&mut self, _pname: GLenum, _param: GLfixed) {}
    unsafe fn LightModelfv(&mut self, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn LightModelxv(&mut self, _pname: GLenum, _params: *const GLfixed) {}
    unsafe fn Materialf(&mut self, _face: GLenum, _pname: GLenum, _param: GLfloat) {}
    unsafe fn Materialx(&mut self, _face: GLenum, _pname: GLenum, _param: GLfixed) {}
    unsafe fn Materialfv(&mut self, _face: GLenum, _pname: GLenum, _params: *const GLfloat) {}
    unsafe fn Materialxv(&mut self, _face: GLenum, _pname: GLenum, _params: *const GLfixed) {}

    // ── 缓冲(直转 gl30)──
    unsafe fn IsBuffer(&mut self, buffer: GLuint) -> GLboolean {
        gl30::IsBuffer(buffer)
    }
    unsafe fn GenBuffers(&mut self, n: GLsizei, buffers: *mut GLuint) {
        gl30::GenBuffers(n, buffers);
    }
    unsafe fn DeleteBuffers(&mut self, n: GLsizei, buffers: *const GLuint) {
        gl30::DeleteBuffers(n, buffers);
    }
    unsafe fn BindBuffer(&mut self, target: GLenum, buffer: GLuint) {
        match target {
            gles11::ARRAY_BUFFER => self.state.array_buffer_binding = buffer,
            gles11::ELEMENT_ARRAY_BUFFER => self.state.element_array_buffer_binding = buffer,
            _ => {}
        }
        gl30::BindBuffer(target, buffer);
    }
    unsafe fn BufferData(
        &mut self,
        target: GLenum,
        size: GLsizeiptr,
        data: *const GLvoid,
        usage: GLenum,
    ) {
        gl30::BufferData(target, size, data, usage);
    }
    unsafe fn BufferSubData(
        &mut self,
        target: GLenum,
        offset: GLintptr,
        size: GLsizeiptr,
        data: *const GLvoid,
    ) {
        gl30::BufferSubData(target, offset, size, data);
    }

    // ── 非指针顶点属性 ──
    unsafe fn Color4f(&mut self, red: GLfloat, green: GLfloat, blue: GLfloat, alpha: GLfloat) {
        self.state.current_color = [red, green, blue, alpha];
    }
    unsafe fn Color4x(&mut self, red: GLfixed, green: GLfixed, blue: GLfixed, alpha: GLfixed) {
        self.state.current_color = [
            fixed_to_float(red),
            fixed_to_float(green),
            fixed_to_float(blue),
            fixed_to_float(alpha),
        ];
    }
    unsafe fn Color4ub(&mut self, red: GLubyte, green: GLubyte, blue: GLubyte, alpha: GLubyte) {
        self.state.current_color = [
            red as f32 / 255.0,
            green as f32 / 255.0,
            blue as f32 / 255.0,
            alpha as f32 / 255.0,
        ];
    }
    unsafe fn Normal3f(&mut self, nx: GLfloat, ny: GLfloat, nz: GLfloat) {
        self.state.current_normal = [nx, ny, nz];
    }
    unsafe fn Normal3x(&mut self, nx: GLfixed, ny: GLfixed, nz: GLfixed) {
        self.state.current_normal = [
            fixed_to_float(nx),
            fixed_to_float(ny),
            fixed_to_float(nz),
        ];
    }

    // ── 指针数组(记录到 State,draw 时打 VBO)──
    unsafe fn ColorPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        self.set_array(ARRAY_COLOR, size, type_, stride, pointer);
    }
    unsafe fn NormalPointer(&mut self, type_: GLenum, stride: GLsizei, pointer: *const GLvoid) {
        self.set_array(ARRAY_NORMAL, 3, type_, stride, pointer);
    }
    unsafe fn TexCoordPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        // 纹理坐标数组按 client_active_texture 区分;首版只追踪单元 0 的那份。
        self.set_array(ARRAY_TEXCOORD, size, type_, stride, pointer);
    }
    unsafe fn VertexPointer(
        &mut self,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        self.set_array(ARRAY_VERTEX, size, type_, stride, pointer);
    }

    // ── 绘制 ──
    unsafe fn DrawArrays(&mut self, mode: GLenum, first: GLint, count: GLsizei) {
        if count <= 0 {
            return;
        }
        let vertex_count = (first + count).max(0) as usize;
        self.translate_fixed_point_arrays(first, count);
        self.upload_arrays_and_setup(vertex_count);
        self.setup_draw_state();
        gl30::DrawArrays(mode, first, count);
    }
    unsafe fn DrawElements(
        &mut self,
        mode: GLenum,
        count: GLsizei,
        type_: GLenum,
        indices: *const GLvoid,
    ) {
        if count <= 0 {
            return;
        }
        // 找出最大索引,确定需要上传多少顶点。
        let count_u = count as usize;
        let mut max_index: usize = 0;
        // indices 是 client 内存指针(guest 已转 host 可读)。
        match type_ {
            gles11::UNSIGNED_BYTE => {
                let p = indices as *const u8;
                for i in 0..count_u {
                    max_index = max_index.max(*p.add(i) as usize);
                }
            }
            gles11::UNSIGNED_SHORT => {
                let p = indices as *const u16;
                for i in 0..count_u {
                    max_index = max_index.max(p.add(i).read_unaligned() as usize);
                }
            }
            gl30::UNSIGNED_INT => {
                let p = indices as *const u32;
                for i in 0..count_u {
                    max_index = max_index.max(p.add(i).read_unaligned() as usize);
                }
            }
            _ => return,
        }
        let vertex_count = max_index + 1;

        self.translate_fixed_point_arrays(0, vertex_count as GLsizei);
        self.upload_arrays_and_setup(vertex_count);
        self.setup_draw_state();

        // 把索引拷进持久 EBO。
        let idx_size = gl_type_size(type_);
        let bytes = std::slice::from_raw_parts(indices as *const u8, count_u * idx_size);
        gl30::BindBuffer(gl30::ELEMENT_ARRAY_BUFFER, self.ebo);
        gl30::BufferData(
            gl30::ELEMENT_ARRAY_BUFFER,
            bytes.len() as GLsizeiptr,
            bytes.as_ptr() as *const GLvoid,
            gl30::DYNAMIC_DRAW,
        );
        gl30::DrawElements(mode, count, type_, std::ptr::null());
        // 还原 guest 的 EBO 绑定。
        gl30::BindBuffer(gl30::ELEMENT_ARRAY_BUFFER, self.state.element_array_buffer_binding);
    }

    // ── 清屏 ──
    unsafe fn Clear(&mut self, mask: GLbitfield) {
        gl30::Clear(mask);
    }
    unsafe fn ClearColor(
        &mut self,
        red: GLclampf,
        green: GLclampf,
        blue: GLclampf,
        alpha: GLclampf,
    ) {
        self.state.clear_color = [red, green, blue, alpha];
        gl30::ClearColor(red, green, blue, alpha);
    }
    unsafe fn ClearColorx(
        &mut self,
        red: GLclampx,
        green: GLclampx,
        blue: GLclampx,
        alpha: GLclampx,
    ) {
        let c = [
            fixed_to_float(red),
            fixed_to_float(green),
            fixed_to_float(blue),
            fixed_to_float(alpha),
        ];
        self.state.clear_color = c;
        gl30::ClearColor(c[0], c[1], c[2], c[3]);
    }
    unsafe fn ClearDepthf(&mut self, depth: GLclampf) {
        gl30::ClearDepthf(depth);
    }
    unsafe fn ClearDepthx(&mut self, depth: GLclampx) {
        gl30::ClearDepthf(fixed_to_float(depth));
    }
    unsafe fn ClearStencil(&mut self, s: GLint) {
        gl30::ClearStencil(s);
    }

    // ── 纹理 ──
    unsafe fn PixelStorei(&mut self, pname: GLenum, param: GLint) {
        gl30::PixelStorei(pname, param);
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
        gl30::ReadPixels(x, y, width, height, format, type_, pixels);
    }
    unsafe fn GenTextures(&mut self, n: GLsizei, textures: *mut GLuint) {
        gl30::GenTextures(n, textures);
    }
    unsafe fn DeleteTextures(&mut self, n: GLsizei, textures: *const GLuint) {
        gl30::DeleteTextures(n, textures);
    }
    unsafe fn ActiveTexture(&mut self, texture: GLenum) {
        self.state.active_texture = texture;
        gl30::ActiveTexture(texture);
    }
    unsafe fn IsTexture(&mut self, texture: GLuint) -> GLboolean {
        gl30::IsTexture(texture)
    }
    unsafe fn BindTexture(&mut self, target: GLenum, texture: GLuint) {
        if target == gles11::TEXTURE_2D {
            let idx = self.state.active_tex_idx();
            self.state.bound_texture_2d[idx] = texture;
        }
        gl30::BindTexture(target, texture);
    }
    unsafe fn TexParameteri(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        gl30::TexParameteri(target, pname, param);
    }
    unsafe fn TexParameterf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        gl30::TexParameteri(target, pname, param as GLint);
    }
    unsafe fn TexParameterx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        // GLES1 的 x 版纹理参数其实是整型枚举,不需缩放。
        gl30::TexParameteri(target, pname, param);
    }
    unsafe fn TexParameteriv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        gl30::TexParameteri(target, pname, *params);
    }
    unsafe fn TexParameterfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        gl30::TexParameteri(target, pname, *params as GLint);
    }
    unsafe fn TexParameterxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        gl30::TexParameteri(target, pname, *params);
    }
    unsafe fn TexImage2D(
        &mut self,
        target: GLenum,
        level: GLint,
        internalformat: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        type_: GLenum,
        pixels: *const GLvoid,
    ) {
        // WebGL2 的 internalformat 与 format 通常一致(sized 时另说);先直接转发。
        gl30::TexImage2D(
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
        // NPOT 纹理在 WebGL2 默认 REPEAT 会 incomplete → 采样全黑。强制 CLAMP_TO_EDGE,
        // 并把 min filter 设为 LINEAR(无 mipmap)。仅对 level 0 的 TEXTURE_2D 设置。
        if target == gles11::TEXTURE_2D && level == 0 {
            gl30::TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_WRAP_S,
                gles11::CLAMP_TO_EDGE as GLint,
            );
            gl30::TexParameteri(
                gles11::TEXTURE_2D,
                gles11::TEXTURE_WRAP_T,
                gles11::CLAMP_TO_EDGE as GLint,
            );
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
        gl30::TexSubImage2D(
            target, level, xoffset, yoffset, width, height, format, type_, pixels,
        );
    }
    unsafe fn CompressedTexImage2D(
        &mut self,
        _target: GLenum,
        _level: GLint,
        internalformat: GLenum,
        _width: GLsizei,
        _height: GLsizei,
        _border: GLint,
        _image_size: GLsizei,
        _data: *const GLvoid,
    ) {
        // 首版不支持压缩纹理(标题画面是 RGBA PNG)。记录后忽略,避免崩溃。
        log!(
            "[WASM] GLES1OnWebGL2: 忽略 CompressedTexImage2D(internalformat={:#x})",
            internalformat
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
        gl30::CopyTexImage2D(target, level, internalformat, x, y, width, height, border);
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
        gl30::CopyTexSubImage2D(target, level, xoffset, yoffset, x, y, width, height);
    }
    unsafe fn TexEnvf(&mut self, target: GLenum, pname: GLenum, param: GLfloat) {
        self.set_tex_env(target, pname, param as GLint);
    }
    unsafe fn TexEnvx(&mut self, target: GLenum, pname: GLenum, param: GLfixed) {
        self.set_tex_env(target, pname, param);
    }
    unsafe fn TexEnvi(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        self.set_tex_env(target, pname, param);
    }
    unsafe fn TexEnvfv(&mut self, target: GLenum, pname: GLenum, params: *const GLfloat) {
        self.set_tex_env(target, pname, *params as GLint);
    }
    unsafe fn TexEnvxv(&mut self, target: GLenum, pname: GLenum, params: *const GLfixed) {
        self.set_tex_env(target, pname, *params);
    }
    unsafe fn TexEnviv(&mut self, target: GLenum, pname: GLenum, params: *const GLint) {
        self.set_tex_env(target, pname, *params);
    }

    unsafe fn MultiTexCoord4f(
        &mut self,
        _target: GLenum,
        _s: GLfloat,
        _t: GLfloat,
        _r: GLfloat,
        _q: GLfloat,
    ) {
        // 即时纹理坐标:cocos2d 用数组路径,极少用这个,忽略。
    }
    unsafe fn MultiTexCoord4x(
        &mut self,
        _target: GLenum,
        _s: GLfixed,
        _t: GLfixed,
        _r: GLfixed,
        _q: GLfixed,
    ) {
    }

    // ── 矩阵栈 ──
    unsafe fn MatrixMode(&mut self, mode: GLenum) {
        self.state.matrix_mode = mode;
    }
    unsafe fn LoadIdentity(&mut self) {
        *self.state.current_stack_top_mut() = Matrix::<4>::identity();
    }
    unsafe fn LoadMatrixf(&mut self, m: *const GLfloat) {
        *self.state.current_stack_top_mut() = Self::matrix_from_ptr(m);
    }
    unsafe fn LoadMatrixx(&mut self, m: *const GLfixed) {
        let f = matrix_fixed_to_float(m);
        *self.state.current_stack_top_mut() = Self::matrix_from_ptr(f.as_ptr());
    }
    unsafe fn MultMatrixf(&mut self, m: *const GLfloat) {
        let mm = Self::matrix_from_ptr(m);
        self.mult_current(&mm);
    }
    unsafe fn MultMatrixx(&mut self, m: *const GLfixed) {
        let f = matrix_fixed_to_float(m);
        let mm = Self::matrix_from_ptr(f.as_ptr());
        self.mult_current(&mm);
    }
    unsafe fn PushMatrix(&mut self) {
        match self.state.matrix_mode {
            gles11::PROJECTION => {
                let top = *self.state.top_projection();
                self.state.projection_stack.push(top);
            }
            gles11::TEXTURE => {
                let top = *self.state.top_texture();
                self.state.texture_stack.push(top);
            }
            _ => {
                let top = *self.state.top_modelview();
                self.state.modelview_stack.push(top);
            }
        }
    }
    unsafe fn PopMatrix(&mut self) {
        match self.state.matrix_mode {
            gles11::PROJECTION => {
                if self.state.projection_stack.len() > 1 {
                    self.state.projection_stack.pop();
                }
            }
            gles11::TEXTURE => {
                if self.state.texture_stack.len() > 1 {
                    self.state.texture_stack.pop();
                }
            }
            _ => {
                if self.state.modelview_stack.len() > 1 {
                    self.state.modelview_stack.pop();
                }
            }
        }
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
        let m = Self::make_ortho(left, right, bottom, top, near, far);
        self.mult_current(&m);
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
        let m = Self::make_ortho(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        );
        self.mult_current(&m);
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
        let m = Self::make_frustum(left, right, bottom, top, near, far);
        self.mult_current(&m);
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
        let m = Self::make_frustum(
            fixed_to_float(left),
            fixed_to_float(right),
            fixed_to_float(bottom),
            fixed_to_float(top),
            fixed_to_float(near),
            fixed_to_float(far),
        );
        self.mult_current(&m);
    }
    unsafe fn Rotatef(&mut self, angle: GLfloat, x: GLfloat, y: GLfloat, z: GLfloat) {
        let m = Self::make_rotate(angle, x, y, z);
        self.mult_current(&m);
    }
    unsafe fn Rotatex(&mut self, angle: GLfixed, x: GLfixed, y: GLfixed, z: GLfixed) {
        let m = Self::make_rotate(
            fixed_to_float(angle),
            fixed_to_float(x),
            fixed_to_float(y),
            fixed_to_float(z),
        );
        self.mult_current(&m);
    }
    unsafe fn Scalef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let m = Self::make_scale(x, y, z);
        self.mult_current(&m);
    }
    unsafe fn Scalex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        let m = Self::make_scale(fixed_to_float(x), fixed_to_float(y), fixed_to_float(z));
        self.mult_current(&m);
    }
    unsafe fn Translatef(&mut self, x: GLfloat, y: GLfloat, z: GLfloat) {
        let m = Matrix::<4>::translate_3d(x, y, z);
        self.mult_current(&m);
    }
    unsafe fn Translatex(&mut self, x: GLfixed, y: GLfixed, z: GLfixed) {
        let m = Matrix::<4>::translate_3d(
            fixed_to_float(x),
            fixed_to_float(y),
            fixed_to_float(z),
        );
        self.mult_current(&m);
    }

    // ── OES framebuffer:WebGL2 原生 FBO,*OES 直转 gl30 同名(枚举值一致)──
    unsafe fn GenFramebuffersOES(&mut self, n: GLsizei, framebuffers: *mut GLuint) {
        gl30::GenFramebuffers(n, framebuffers);
    }
    unsafe fn GenRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *mut GLuint) {
        gl30::GenRenderbuffers(n, renderbuffers);
    }
    unsafe fn IsFramebufferOES(&mut self, framebuffer: GLuint) -> GLboolean {
        gl30::IsFramebuffer(framebuffer)
    }
    unsafe fn IsRenderbufferOES(&mut self, renderbuffer: GLuint) -> GLboolean {
        gl30::IsRenderbuffer(renderbuffer)
    }
    unsafe fn BindFramebufferOES(&mut self, target: GLenum, framebuffer: GLuint) {
        gl30::BindFramebuffer(target, framebuffer);
    }
    unsafe fn BindRenderbufferOES(&mut self, target: GLenum, renderbuffer: GLuint) {
        gl30::BindRenderbuffer(target, renderbuffer);
    }
    unsafe fn RenderbufferStorageOES(
        &mut self,
        target: GLenum,
        internalformat: GLenum,
        width: GLsizei,
        height: GLsizei,
    ) {
        gl30::RenderbufferStorage(target, internalformat, width, height);
    }
    unsafe fn FramebufferRenderbufferOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        renderbuffertarget: GLenum,
        renderbuffer: GLuint,
    ) {
        gl30::FramebufferRenderbuffer(target, attachment, renderbuffertarget, renderbuffer);
    }
    unsafe fn FramebufferTexture2DOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        textarget: GLenum,
        texture: GLuint,
        level: i32,
    ) {
        gl30::FramebufferTexture2D(target, attachment, textarget, texture, level);
    }
    unsafe fn GetFramebufferAttachmentParameterivOES(
        &mut self,
        target: GLenum,
        attachment: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl30::GetFramebufferAttachmentParameteriv(target, attachment, pname, params);
    }
    unsafe fn GetRenderbufferParameterivOES(
        &mut self,
        target: GLenum,
        pname: GLenum,
        params: *mut GLint,
    ) {
        gl30::GetRenderbufferParameteriv(target, pname, params);
    }
    unsafe fn CheckFramebufferStatusOES(&mut self, target: GLenum) -> GLenum {
        gl30::CheckFramebufferStatus(target)
    }
    unsafe fn DeleteFramebuffersOES(&mut self, n: GLsizei, framebuffers: *const GLuint) {
        gl30::DeleteFramebuffers(n, framebuffers);
    }
    unsafe fn DeleteRenderbuffersOES(&mut self, n: GLsizei, renderbuffers: *const GLuint) {
        gl30::DeleteRenderbuffers(n, renderbuffers);
    }
    unsafe fn GenerateMipmapOES(&mut self, target: GLenum) {
        gl30::GenerateMipmap(target);
    }
    unsafe fn GetBufferParameteriv(&mut self, target: GLenum, pname: GLenum, params: *mut GLint) {
        gl30::GetBufferParameteriv(target, pname, params);
    }
    unsafe fn MapBufferOES(&mut self, _target: GLenum, _access: GLenum) -> *mut GLvoid {
        // WebGL2 无 glMapBuffer(只有 MapBufferRange,且不返回持久指针)。cocos2d 不用,
        // 返回 null 让调用方走非 map 路径。
        std::ptr::null_mut()
    }
    unsafe fn UnmapBufferOES(&mut self, _target: GLenum) -> GLboolean {
        gles11::FALSE
    }
}

impl GLES1OnWebGL2<'_> {
    /// 记录一个 client 数组的描述。
    fn set_array(
        &mut self,
        idx: usize,
        size: GLint,
        type_: GLenum,
        stride: GLsizei,
        pointer: *const GLvoid,
    ) {
        let arr = &mut self.state.arrays[idx];
        arr.size = size;
        arr.type_ = type_;
        arr.stride = stride;
        arr.pointer = pointer;
        arr.buffer_binding = self.state.array_buffer_binding;
        // 记录该数组是否为定点(供 draw 时转换)。
        self.state.pointer_is_fixed_point[idx] = type_ == gles11::FIXED;
        if idx == ARRAY_TEXCOORD {
            if type_ == gles11::FIXED {
                self.state
                    .fixed_point_texture_units
                    .insert(self.state.client_active_texture);
            } else {
                self.state
                    .fixed_point_texture_units
                    .remove(&self.state.client_active_texture);
            }
        }
    }

    /// 设置纹理环境模式(只关心 TEXTURE_ENV_MODE)。
    fn set_tex_env(&mut self, target: GLenum, pname: GLenum, param: GLint) {
        if target == gles11::TEXTURE_ENV && pname == gles11::TEXTURE_ENV_MODE {
            let idx = self.state.active_tex_idx();
            self.state.tex_env_mode[idx] = param as GLenum;
        }
        // 其它 texenv 参数(COMBINE/SCALE 等)首版忽略。
    }
}
