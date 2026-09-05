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
    if env.options.force_composition {
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
        // [MoleWorld iOS] Accept a *rotated* fullscreen layer. A landscape cocos2d
        // game gives its CAEAGLLayer the swapped screen size (e.g. 1024x768 for a
        // 768x1024 portrait UIScreen) plus a 90° affine_transform. It is still the
        // single fullscreen layer; present_renderbuffer() orients the frame with the
        // window's device rotation_matrix (NOT the layer's transform), so accept the
        // swapped size and don't require an identity transform here. Without this we
        // drop to the slow composition path, which never displays the frame = black
        // screen (the renderbuffer is confirmed non-black, full game content).
        let bsz = layer_host_obj.bounds.size;
        let ssz = screen_bounds.size;
        // [MoleWorld iOS 宽屏] 尺寸/位置比较必须带容差:--fill-screen 算出的逻辑屏 1669×768 经 guest 的
        // frame→bounds 浮点换算后是 1669×768.00006(4:3 的 1024/768 全是整数所以从没暴露),精确相等会把
        // 真正的全屏层判成"不是全屏" → 100% 帧掉进慢合成路径(每帧整屏 5MB 重传,且铺屏模式下画出来是黑的)。
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        let size_ok = (near(bsz.width, ssz.width) && near(bsz.height, ssz.height))
            || (near(bsz.width, ssz.height) && near(bsz.height, ssz.width));
        // [MoleWorld iOS · 性能] 位置检查也要接受"旋转后的全屏层":上面已按"宽高互换"放行了横屏
        // 1024×768 的层,但它的 position 是自己坐标系的中心 (512,384),而不是竖屏 UIScreen 的中心
        // (384,512)。真机实测村里 12.5% 的帧([PRESENT] SLOW 45×64)就是在这里被判 nil 掉进慢路径
        // (每帧 glReadPixels 整屏 + 3MB 重传),[FSLAYER-NIL] 打印全是 sizeok=true 而 pos=(512,384)。
        // 修:位置与屏幕中心【或】与互换后的中心相等,都算全屏。
        let pos = layer_host_obj.position;
        let center = CGPoint { x: ssz.width / 2.0, y: ssz.height / 2.0 };
        let center_swapped = CGPoint { x: ssz.height / 2.0, y: ssz.width / 2.0 };
        let pos_ok = (near(pos.x, center.x) && near(pos.y, center.y))
            || (near(pos.x, center_swapped.x) && near(pos.y, center_swapped.y));
        let origin = layer_host_obj.bounds.origin;
        if !size_ok
            || !(near(origin.x, 0.0) && near(origin.y, 0.0))
            || layer_host_obj.anchor_point != (CGPoint { x: 0.5, y: 0.5 })
            || !pos_ok
            || layer_host_obj.hidden
            || layer_host_obj.opacity != 1.0
        {
            // [MoleWorld iOS · 诊断] 好友村"卡死"= present 跌出全屏快路径、走每帧 glReadPixels 整屏回读的
            // 慢合成路径(无 JIT 解释器上慢到 ~2-5fps = 假死)。这里打印【到底是哪个 layer、因为什么条件】
            // 把快路径判没了,一次定位真正盖住全屏的那个 UIKit 层(HUD?广告 WebView?尺寸/透明度不符?)。
            // 节流打印,避免刷屏。
            {
                use std::sync::atomic::{AtomicU64, Ordering};
                static NIL_N: AtomicU64 = AtomicU64::new(0);
                let n = NIL_N.fetch_add(1, Ordering::Relaxed);
                if n < 8 || n % 256 == 0 {
                    // 复制出 packed 字段(不能直接借用 packed struct 的字段)
                    let (bw, bh) = (bsz.width, bsz.height);
                    let (sw, sh) = (ssz.width, ssz.height);
                    let (box_, boy) = (
                        layer_host_obj.bounds.origin.x,
                        layer_host_obj.bounds.origin.y,
                    );
                    let (apx, apy) = (layer_host_obj.anchor_point.x, layer_host_obj.anchor_point.y);
                    let (pox, poy) = (layer_host_obj.position.x, layer_host_obj.position.y);
                    let (hid, op) = (layer_host_obj.hidden, layer_host_obj.opacity);
                    let cls = env
                        .objc
                        .try_get_class_name(layer)
                        .unwrap_or("?")
                        .to_string();
                    echo!(
                        "[FSLAYER-NIL] n={} 顶层layer类={} bounds={}x{}@({},{}) screen={}x{} anchor=({},{}) pos=({},{}) hidden={} opacity={} sizeok={}",
                        n, cls, bw, bh, box_, boy, sw, sh, apx, apy, pox, poy, hid, op, size_ok
                    );
                }
            }
            return nil;
        }

        // [MoleWorld iOS · P0 好友村卡死根治] 原逻辑"永远下降到最后一个子层",一旦界面在游戏
        // 全屏 CAEAGLLayer 之上叠了任何【小的 UIKit 浮层】(实测真凶:好友界面那个 220x40 的
        // "ID or Name" 搜索输入框 UITextField),下降就撞到它 → 尺寸不符 → 判定"没有全屏层" →
        // present 跌进慢合成路径(每帧 glReadPixels 整屏回读+重传),无 JIT 解释器上直接慢到
        // ~2-5fps = 用户看到的"点好友卡死"。
        // 修:选择下一层时【跳过未被聚焦的小浮层】(面积 < 半屏且其 UIView 既不在编辑也不是第一
        // 响应者),从而仍能识别底下的全屏 CAEAGLLayer、留在快路径。
        // 取舍:被跳过的小浮层这一帧不参与合成(不显示)。但【正在输入的输入框不会被跳过】——
        // 用户点进改名框时它是第一响应者/editing=true,照常走合成显示,打字所见即所得不受影响。
        let subs: Vec<id> = layer_host_obj.sublayers.clone();
        let mut next_layer: Option<id> = None;
        for &cand in subs.iter().rev() {
            if overlay_is_ignorable(env, cand, screen_bounds.size) {
                continue;
            }
            next_layer = Some(cand);
            break;
        }
        if let Some(next) = next_layer {
            layer = next;
        } else {
            break;
        }
    }

    // [MoleWorld iOS · 诊断] 同上:另两条判 nil 的出口也打印,区分"顶层不透明度不符"与"顶层压根不是 CAEAGLLayer
    // (=被某个 UIKit 覆盖层顶掉)"。后者正是好友村跌慢路径最可能的形态。
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TAIL_N: AtomicU64 = AtomicU64::new(0);
        let opaque = env.objc.borrow::<CALayerHostObject>(layer).opaque;
        let ca_eagl_layer_class: Class = msg_class![env; CAEAGLLayer class];
        let is_eagl: bool = msg![env; layer isKindOfClass:ca_eagl_layer_class];
        if !opaque || !is_eagl {
            let n = TAIL_N.fetch_add(1, Ordering::Relaxed);
            if n < 8 || n % 256 == 0 {
                let cls = env
                    .objc
                    .try_get_class_name(layer)
                    .unwrap_or("?")
                    .to_string();
                echo!(
                    "[FSLAYER-NIL] n={} (尾判) 顶层layer类={} opaque={} isCAEAGLLayer={}",
                    n, cls, opaque, is_eagl
                );
            }
            return nil;
        }
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

/// [MoleWorld iOS] 该子层是否是"可以在寻找全屏层时跳过的小浮层"。
///
/// 判据:(1) 面积明显小于半屏(全屏候选不可能这么小);(2) 它背后的 UIView 当前【不在编辑、也不是
/// 第一响应者】——正在输入的控件必须参与合成,否则用户看不见自己打的字。
/// 用途见 [find_fullscreen_eagl_layer] 里的说明(好友村"卡死"根治)。
fn overlay_is_ignorable(env: &mut Environment, layer: id, screen: CGSize) -> bool {
    let (size, delegate) = {
        let o: &CALayerHostObject = env.objc.borrow(layer);
        (o.bounds.size, o.delegate)
    };
    let screen_area = (screen.width * screen.height).abs();
    if screen_area <= 0.0 {
        return false;
    }
    if (size.width * size.height).abs() >= screen_area * 0.5 {
        return false; // 够大,可能就是全屏层本身,不能跳过
    }
    if delegate != nil {
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "isEditing")
        {
            let editing: bool = msg![env; delegate isEditing];
            if editing {
                return false;
            }
        }
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "isFirstResponder")
        {
            let focused: bool = msg![env; delegate isFirstResponder];
            if focused {
                return false;
            }
        }
    }
    true
}
