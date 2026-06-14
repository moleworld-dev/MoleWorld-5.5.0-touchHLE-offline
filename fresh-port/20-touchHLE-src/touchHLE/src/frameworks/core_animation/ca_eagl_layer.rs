/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CAEAGLLayer`.

use super::ca_layer::CALayerHostObject;
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::objc::{id, msg, msg_class, nil, objc_classes, Class, ClassExports};
use crate::Environment;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation CAEAGLLayer: CALayer

// EAGLDrawable implementation (the only one)

- (id)drawableProperties {
    // FIXME: do we need to return an empty dictionary rather than nil?
    env.objc.borrow::<CALayerHostObject>(this).drawable_properties
}

- (())setDrawableProperties:(id)props { // NSDictionary<NSString*, id>*
    let props: id = msg![env; props copy];
    env.objc.borrow_mut::<CALayerHostObject>(this).drawable_properties = props;
}

@end

};

/// If there is an opaque `CAEAGLLayer` that covers the entire screen, this
/// returns a pointer to it. Otherwise, it returns [nil].
///
/// To avoid a state management nightmare, we want to have an internal OpenGL ES
/// context for compositing, separate from any OpenGL ES contexts the app uses
/// for its rendering. When we have a `CAEAGLLayer` though, we need to transfer
/// a rendered frame from the app's context to the compositor's context, and
/// unfortunately the most practical way to do this is `glReadPixels()`, which
/// is highly inefficient. To make things efficient, then, we have a shortcut:
/// if the result of composition would be identical to the rendered frame, i.e.
/// there's a single full-screen layer, we skip transferring between contexts
/// and present it directly from the app's context. This function is used to
/// determine when that will happen.
pub fn find_fullscreen_eagl_layer(env: &mut Environment) -> id {
    // [MoleWorld] 编辑文本时强制走 composition 路径(返回 nil = 无 fullscreen 快路径),
    // 否则 UITextField/UILabel 的逐字符更新永远不上屏(快路径只 present 游戏 GL renderbuffer,
    // recomposite 又在 fullscreen-EAGL 处早退跳过 UIKit overlay)。返回 nil 后 presentRenderbuffer:
    // 走 slow path 存 presented_pixels 底图,故合成时游戏画面+输入框文字一起显示,不黑屏。
    if env.options.force_composition || crate::window::mole_text_input_active() {
        return nil;
    }

    let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
    // Assumes the windows in the list are ordered back-to-front.
    // TODO: this may not be correct once we support windowLevel.
    let Some(top_window) = windows
        .into_iter()
        .rev()
        .find(|&window| !msg![env; window isHidden])
    else {
        return nil;
    };

    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };

    let screen = screen_bounds.size;
    // The screen with width/height swapped — a landscape app's fullscreen layer
    // on a portrait iPad screen has these dimensions (and a 90° rotation).
    let swapped = CGSize {
        width: screen.height,
        height: screen.width,
    };
    let center = CGPoint {
        x: screen.width / 2.0,
        y: screen.height / 2.0,
    };

    let mut layer: id = msg![env; top_window layer];

    // Descend through the hierarchy, looking only at the last layer in each
    // list of children, since that should be the one on top.
    // TODO: this is not correct once we support zPosition.
    loop {
        assert!(layer != nil);

        let h: &CALayerHostObject = env.objc.borrow(layer);

        // This is stricter than it should be. In theory we should accumulate
        // the transforms and handle different anchor points etc, but real apps
        // probably only use this common case.
        let identity_fs = h.affine_transform.is_identity() && h.bounds.size == screen;
        // [MoleWorld wasm 启动页闪烁修复] 摩尔是横屏游戏跑在竖屏 iPad(UIScreen=768x1024)上,
        // 把全屏 CAEAGLLayer 旋转 90°(bounds 与屏幕宽高对调)。原逻辑只认 identity 全屏,这种
        // 旋转全屏被打回 slow-path 合成;wasm 的 webgl2 后端下 slow-path 每帧 glReadPixels 取游戏
        // renderbuffer 会抖动(交替读到淘米 splash 帧与游戏帧)→ 启动页来回闪。认下「旋转 90° 后
        // 仍全屏」即走 fast path 直接 present 当前 renderbuffer(朝向由窗口 rotation_matrix 处理),
        // 一帧一present,彻底消除合成抖动。只 wasm 门控:桌面/iOS 的 slow-path 本就不闪,保持原样
        // = 五平台零回归。判旋转:仿射 a≈0,d≈0,|b|≈|c|≈1,b≈-c(纯 90°/270° 转,非翻转/缩放)。
        let rotated_fs = if cfg!(target_arch = "wasm32") {
            let t = h.affine_transform;
            let is_quarter = t.a.abs() < 1e-3
                && t.d.abs() < 1e-3
                && (t.b.abs() - 1.0).abs() < 1e-3
                && (t.c.abs() - 1.0).abs() < 1e-3
                && (t.b + t.c).abs() < 1e-3;
            is_quarter && h.bounds.size == swapped
        } else {
            false
        };

        let ok = (identity_fs || rotated_fs)
            && h.bounds.origin == (CGPoint { x: 0.0, y: 0.0 })
            && h.anchor_point == (CGPoint { x: 0.5, y: 0.5 })
            && h.position == center
            && !h.hidden
            && h.opacity == 1.0;
        if !ok {
            return nil;
        }

        if let Some(&next) = h.sublayers.last() {
            layer = next;
        } else {
            break;
        }
    }

    if !env.objc.borrow::<CALayerHostObject>(layer).opaque {
        return nil;
    }

    let ca_eagl_layer_class: Class = msg_class![env; CAEAGLLayer class];
    if !msg![env; layer isKindOfClass:ca_eagl_layer_class] {
        return nil;
    }

    layer
}

/// For use by `EAGLContext` when presenting to a `CAEAGLLayer`:
/// [std::mem::take]s the buffer used to hold the pixels. It should be passed
/// back to [present_pixels] once it has been filled.
pub fn get_pixels_vec_for_presenting(env: &mut Environment, layer: id) -> Vec<u8> {
    env.objc
        .borrow_mut::<CALayerHostObject>(layer)
        .presented_pixels
        .take()
        .map(|(vec, _width, _height)| vec)
        .unwrap_or_default()
}

/// For use by `EAGLContext` when presenting to a `CAEAGLLayer`: provide the new
/// frame rendered by the app, so it can be used when compositing. The buffer
/// should have been obtained with [get_pixels_vec_for_presenting] before
/// filling. The data must be in RGBA8 format.
pub fn present_pixels(env: &mut Environment, layer: id, pixels: Vec<u8>, width: u32, height: u32) {
    let host_obj = env.objc.borrow_mut::<CALayerHostObject>(layer);
    host_obj.presented_pixels = Some((pixels, width, height));
    host_obj.gles_texture_is_up_to_date = false;
}
