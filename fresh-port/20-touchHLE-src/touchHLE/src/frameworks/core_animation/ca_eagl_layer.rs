/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CAEAGLLayer`.

use super::ca_layer::CALayerHostObject;
use crate::frameworks::core_graphics::{CGPoint, CGRect};
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

    let mut layer: id = msg![env; top_window layer];

    // Descend through the hierarchy, looking only at the last layer in each
    // list of children, since that should be the one on top.
    // TODO: this is not correct once we support zPosition.
    loop {
        assert!(layer != nil);

        let layer_host_obj: &CALayerHostObject = env.objc.borrow(layer);

        // This is stricter than it should be. In theory we should accumulate
        // the transforms and handle different anchor points etc, but real apps
        // probably only use this common case.
        // [同步 iOS 2026-09-16] 尺寸/位置比较带容差(移植自 iOS 分支 1f4f66b):--fill-screen 算出的逻辑屏
        // (如 1188×768)经 guest 的 frame→bounds 浮点换算后会带尾差(768.00006),精确相等会把真正的全屏层
        // 判成"不是全屏" → 每帧掉进慢合成路径(整屏像素来回拷)。4:3 的 1024/768 全是整数所以从没暴露。
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        let bounds = layer_host_obj.bounds;
        let position = layer_host_obj.position;
        if !(near(bounds.size.width, screen_bounds.size.width)
            && near(bounds.size.height, screen_bounds.size.height))
            || !(near(bounds.origin.x, 0.0) && near(bounds.origin.y, 0.0))
            || layer_host_obj.anchor_point != (CGPoint { x: 0.5, y: 0.5 })
            || !(near(position.x, screen_bounds.size.width / 2.0)
                && near(position.y, screen_bounds.size.height / 2.0))
            || layer_host_obj.hidden
            || layer_host_obj.opacity != 1.0
            // TODO: support affine transforms that result in a full-screen
            //       layer (typical example is 90° rotation).
            || !layer_host_obj.affine_transform.is_identity()
        {
            return nil;
        }

        if let Some(&next) = layer_host_obj.sublayers.last() {
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
