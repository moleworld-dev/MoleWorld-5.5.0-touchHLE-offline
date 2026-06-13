/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Utilities for presenting frames to the window using an abstract OpenGL ES
//! implementation.

use super::gles11_raw as gles11; // constants and types only
use super::GLES;
use crate::matrix::Matrix;
use std::time::{Duration, Instant};

pub struct FpsCounter {
    time: std::time::Instant,
    frames: u32,
}
impl FpsCounter {
    pub fn start() -> Self {
        FpsCounter {
            time: Instant::now(),
            frames: 0,
        }
    }

    pub fn count_frame(&mut self, label: std::fmt::Arguments<'_>) {
        self.frames += 1;
        let now = Instant::now();
        let duration = now - self.time;
        if duration >= Duration::from_secs(1) {
            self.time = now;
            echo!(
                "touchHLE: {} FPS: {:.2}",
                label,
                std::mem::take(&mut self.frames) as f32 / duration.as_secs_f32()
            );
        }
    }
}

/// Present the the latest frame (e.g. the app's splash screen or rendering
/// output), provided as a texture bound to `GL_TEXTURE_2D`, by drawing it on
/// the window. It may be rotated, scaled and/or letterboxed as necessary. The
/// virtual cursor is also drawn if it should be currently visible.
///
/// The provided context must be current.
pub unsafe fn present_frame(
    gles: &mut dyn GLES,
    viewport: (u32, u32, u32, u32),
    rotation_matrix: Matrix<2>,
    virtual_cursor_visible_at: Option<(f32, f32, bool)>,
    // [MoleWorld iOS] 窗口的默认 framebuffer:桌面/安卓=0(=窗口);iOS=SDL 绑到 CAEAGLLayer
    // 的非 0 viewFramebuffer。绘制前必须绑定它,否则 iOS 上画进空的 FBO 0 → 窗口灰屏不显示。
    default_framebuffer: gles11::types::GLuint,
) {
    // While this is a generic utility, it is closely tied to
    // crate::frameworks::opengles::eagl::present_renderbuffer, which handles
    // backing up and restoring OpenGL ES state that this function might touch,
    // so these need to be updated in tandem.

    use gles11::types::*;

    // [MoleWorld iOS] 绘制前绑定窗口的默认 framebuffer(绘制目标)。桌面/安卓 default_framebuffer=0
    // (=窗口);iOS 上由各 present 路径传入【该 context 自己的 viewFramebuffer】(splash/composition
    // 走 internal_gl_ins,其 viewFramebuffer=1)。★iOS 的 SDL swap 走 [presentRenderbuffer:],呈现
    // GL_RENDERBUFFER 当前绑定——故各路径在 swap 前还必须把【该 context 的 viewRenderbuffer】绑回
    // GL_RENDERBUFFER(见 composition.rs / window.rs splash 的 swap 前 BindRenderbufferOES),否则黑屏。
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, default_framebuffer);

    // Draw the quad
    gles.Viewport(
        viewport.0 as _,
        viewport.1 as _,
        viewport.2 as _,
        viewport.3 as _,
    );
    gles.ClearColor(0.0, 0.0, 0.0, 1.0);
    gles.Clear(gles11::COLOR_BUFFER_BIT | gles11::DEPTH_BUFFER_BIT | gles11::STENCIL_BUFFER_BIT);
    gles.BindBuffer(gles11::ARRAY_BUFFER, 0);
    let vertices: [f32; 12] = [
        -1.0, -1.0, -1.0, 1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0,
    ];
    gles.EnableClientState(gles11::VERTEX_ARRAY);
    gles.VertexPointer(2, gles11::FLOAT, 0, vertices.as_ptr() as *const GLvoid);
    #[allow(unused_mut)]
    let mut tex_coords: [f32; 12] = [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    gles.MatrixMode(gles11::TEXTURE);
    // [MoleWorld iOS] The orientation rotation must NOT be applied via the
    // GL_TEXTURE matrix on the device: that rotates the texcoords around the
    // ORIGIN, pushing them outside [0,1]; with CLAMP_TO_EDGE the whole quad then
    // samples one edge → the image smears into vertical bands. Instead rotate
    // the texcoords around their CENTRE (0.5,0.5) on the CPU and keep the
    // texture matrix identity (a centred rotation keeps the unit square in
    // place). Desktop keeps the matrix form unchanged (it renders correctly and
    // we don't want to risk a regression on the working path).
    #[cfg(target_os = "ios")]
    {
        let mut i = 0;
        while i < tex_coords.len() {
            let [s, t] = rotation_matrix.transform([tex_coords[i] - 0.5, tex_coords[i + 1] - 0.5]);
            tex_coords[i] = s + 0.5;
            tex_coords[i + 1] = t + 0.5;
            i += 2;
        }
        gles.LoadIdentity();
    }
    #[cfg(not(target_os = "ios"))]
    {
        let matrix = Matrix::<4>::from(&rotation_matrix);
        gles.LoadMatrixf(matrix.columns().as_ptr() as *const _);
    }
    gles.EnableClientState(gles11::TEXTURE_COORD_ARRAY);
    gles.TexCoordPointer(2, gles11::FLOAT, 0, tex_coords.as_ptr() as *const GLvoid);
    gles.Enable(gles11::TEXTURE_2D);
    gles.DrawArrays(gles11::TRIANGLES, 0, 6);
    // clean this up so we don't need to worry about it in e.g. Core Animation
    gles.LoadIdentity();

    // Display virtual cursor
    if let Some((x, y, pressed)) = virtual_cursor_visible_at {
        let (vx, vy, vw, vh) = viewport;
        let x = x - vx as f32;
        let y = y - vy as f32;

        gles.DisableClientState(gles11::TEXTURE_COORD_ARRAY);
        gles.Disable(gles11::TEXTURE_2D);

        gles.Enable(gles11::BLEND);
        gles.BlendFunc(gles11::ONE, gles11::ONE_MINUS_SRC_ALPHA);
        gles.Color4f(0.0, 0.0, 0.0, if pressed { 2.0 / 3.0 } else { 1.0 / 3.0 });

        let radius = 10.0;

        let mut vertices = vertices;
        for i in (0..vertices.len()).step_by(2) {
            vertices[i] = (vertices[i] * radius + x) / (vw as f32 / 2.0) - 1.0;
            vertices[i + 1] = 1.0 - (vertices[i + 1] * radius + y) / (vh as f32 / 2.0);
        }
        gles.VertexPointer(2, gles11::FLOAT, 0, vertices.as_ptr() as *const GLvoid);
        gles.DrawArrays(gles11::TRIANGLES, 0, 6);
    }

    // [MoleWorld DIAG] Snapshot the presented frame to disk so the developer can
    // see the game without a host screenshot (the window is on its own macOS
    // Space and can't be captured). Throttled internally.
    crate::mole_diag::maybe_dump_frame(gles, viewport);
}
