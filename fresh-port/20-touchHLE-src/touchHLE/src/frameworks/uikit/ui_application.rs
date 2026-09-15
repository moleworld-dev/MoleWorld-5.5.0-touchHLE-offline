/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIApplication` and `UIApplicationMain`.

use super::ui_device::*;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::{from_rust_string, get_static_str};
use crate::frameworks::foundation::{ns_array, ns_string, NSInteger, NSUInteger};
use crate::mem::MutPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, todo_objc_setter,
    ClassExports, HostObject, NSZonePtr, ObjC,
};
use crate::window::DeviceOrientation;
use crate::Environment;

#[derive(Default)]
pub struct State {
    /// [UIApplication sharedApplication]
    shared_application: Option<id>,
    pub(super) status_bar_hidden: bool,
    /// [扫描修 2026-09-15] F12-3:因桌面窗口最小化而发过 applicationWillResignActive:、
    /// 还没发回 applicationDidBecomeActive: 的状态。用来保证失活/激活成对、不重复发。
    inactive_by_window: bool,
}

struct UIApplicationHostObject {
    delegate: id,
    delegate_is_retained: bool,
}
impl HostObject for UIApplicationHostObject {}

pub type UIInterfaceOrientation = UIDeviceOrientation;
#[allow(unused)]
pub const UIInterfaceOrientationPortrait: UIInterfaceOrientation = UIDeviceOrientationPortrait;
#[allow(unused)]
pub const UIInterfaceOrientationPortraitUpsideDown: UIInterfaceOrientation =
    UIDeviceOrientationPortraitUpsideDown;
// These are intentionally swapped and documented as such (the UI on the device
// rotates in the opposite direction to how the device is rotated).
pub const UIInterfaceOrientationLandscapeLeft: UIInterfaceOrientation =
    UIDeviceOrientationLandscapeRight;
pub const UIInterfaceOrientationLandscapeRight: UIInterfaceOrientation =
    UIDeviceOrientationLandscapeLeft;

type UIRemoteNotificationType = NSUInteger;
type UIStatusBarAnimation = NSInteger;
type UIStatusBarStyle = NSInteger;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIApplication: UIResponder

// This should only be called by UIApplicationMain
+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(UIApplicationHostObject {
        delegate: nil,
        delegate_is_retained: false,
    });
    env.objc.alloc_static_object(this, host_object, &mut env.mem)
}

+ (id)sharedApplication {
    env.framework_state.uikit.ui_application.shared_application.unwrap_or(nil)
}

// This should only be called by UIApplicationMain
- (id)init {
    assert!(env.framework_state.uikit.ui_application.shared_application.is_none());
    env.framework_state.uikit.ui_application.shared_application = Some(this);
    this
}

// This is a singleton, it shouldn't be deallocated.
- (id)retain { this }
- (id)autorelease { this }
- (())release {}

- (id)delegate {
    env.objc.borrow::<UIApplicationHostObject>(this).delegate
}
- (())setDelegate:(id)delegate { // something implementing UIApplicationDelegate
    let host_object = env.objc.borrow_mut::<UIApplicationHostObject>(this);
    // This property is quasi-non-retaining: https://stackoverflow.com/a/14271150/736162
    let old_delegate = std::mem::replace(&mut host_object.delegate, delegate);
    if host_object.delegate_is_retained {
        host_object.delegate_is_retained = false;
        if delegate != old_delegate {
            release(env, old_delegate);
        }
    }
}

- (bool)isStatusBarHidden {
    env.framework_state.uikit.ui_application.status_bar_hidden
}
- (())setStatusBarHidden:(bool)hidden {
    env.framework_state.uikit.ui_application.status_bar_hidden = hidden;
}
- (())setStatusBarHidden:(bool)hidden
                animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarHidden:hidden]
}
- (())setStatusBarHidden:(bool)hidden
           withAnimation:(UIStatusBarAnimation)_animation {
    // TODO: animation
    msg![env; this setStatusBarHidden:hidden]
}

- (())setStatusBarStyle:(UIStatusBarStyle)style {
    todo_objc_setter!(this, style);
}
- (())setStatusBarStyle:(UIStatusBarStyle)style
               animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarStyle:style]
}

// [深扫修 2026-09-11] #18:补 -statusBarFrame。
// 根因:此前没有实现,游戏 -[CCMenu initWithItems:vaList:]@0x2ce354 用
// objc_msgSend_stret(r0=调用方栈缓冲区) 取它,touchHLE 找不到选择子时只清
// r0/r1、不写返回缓冲区,于是 CCMenu 默认位置 = winSize.height - 栈上垃圾
// (可能是 NaN / 真实坐标),出现偶发的菜单消失/错位。
// 修法:按 iOS 6 原版语义返回。这里声明为 CGRect 返回类型,由 objc_classes!
// 走大结构体(stret)返回路径,保证写回调用方缓冲区。
// - 状态栏隐藏(本游戏 UIStatusBarHidden=true,启动时已 setStatusBarHidden:true)
//   → CGRectZero,菜单默认位置正好是屏幕中心,与真机一致。
// - 未隐藏 → iOS 7 以前 statusBarFrame 用"屏幕坐标系"(不随界面旋转):
//   竖屏 {0,0,屏宽,20};倒竖屏 {0,屏高-20,屏宽,20};
//   横屏是宽 20、高=屏高 的竖条(所以游戏横屏时读的是 size.width)。
//   [审查修 2026-09-13] E11 更正:界面 LandscapeRight(= 设备 LandscapeLeft,
//   Home 键在右)时设备从竖屏逆时针转了 90°,竖屏坐标的 +x 轴朝上,用户看到的
//   UI 顶边在屏幕坐标 x=屏宽 一侧 → x=屏宽-20;界面 LandscapeLeft
//   (= 设备 LandscapeRight,Home 键在左)→ x=0。
//   原注释"UI 顶部在屏幕坐标左侧 → x=0"推错了。依据:window.rs 的旋转矩阵加上
//   触摸逆映射,会把窗口顶边映射到 guest x=屏宽;gles/present.rs 的纹理矩阵同样让
//   窗口顶边采样 s=1 列;environment.rs 把 Info.plist 的
//   UIInterfaceOrientationLandscapeRight 映射成 DeviceOrientation::LandscapeLeft。
//   本游戏状态栏隐藏,走上面的 CGRectZero;CCMenu/TFProgressHUD 也只读 size,不受影响。
//   绝不能写成 {屏高,20},否则状态栏显示时菜单会偏移半个屏宽。
- (CGRect)statusBarFrame {
    if env.framework_state.uikit.ui_application.status_bar_hidden {
        return CGRect {
            origin: CGPoint { x: 0.0, y: 0.0 },
            size: CGSize { width: 0.0, height: 0.0 },
        };
    }
    const STATUS_BAR_HEIGHT: CGFloat = 20.0;
    // 与 -[UIScreen bounds] 同源(竖屏尺寸,iOS 8 以前不随方向变化)。
    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };
    let (w, h) = (screen_bounds.size.width, screen_bounds.size.height);
    let (x, y, width, height) = match env.window().current_rotation() {
        DeviceOrientation::Portrait => (0.0, 0.0, w, STATUS_BAR_HEIGHT),
        DeviceOrientation::PortraitUpsideDown => (0.0, h - STATUS_BAR_HEIGHT, w, STATUS_BAR_HEIGHT),
        // [审查修 2026-09-13] E11:两条横屏分支原先左右写反,已对调。
        // 设备 LandscapeLeft = 界面 LandscapeRight(Home 键在右):UI 顶边在屏幕坐标
        // x=屏宽 一侧 → x=屏宽-20。
        DeviceOrientation::LandscapeLeft => (w - STATUS_BAR_HEIGHT, 0.0, STATUS_BAR_HEIGHT, h),
        // 设备 LandscapeRight = 界面 LandscapeLeft(Home 键在左):UI 顶边在屏幕坐标
        // x=0 一侧 → x=0。
        DeviceOrientation::LandscapeRight => (0.0, 0.0, STATUS_BAR_HEIGHT, h),
    };
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

- (UIInterfaceOrientation)statusBarOrientation {
    match env.window().current_rotation() {
        DeviceOrientation::Portrait => UIDeviceOrientationPortrait,
        DeviceOrientation::PortraitUpsideDown => UIDeviceOrientationPortraitUpsideDown,
        DeviceOrientation::LandscapeLeft => UIDeviceOrientationLandscapeLeft,
        DeviceOrientation::LandscapeRight => UIDeviceOrientationLandscapeRight
    }
}
- (())setStatusBarOrientation:(UIInterfaceOrientation)orientation {
    env.on_parent_stack_in_coroutine(|window, _| {window.rotate_device(match orientation {
        UIDeviceOrientationPortrait => DeviceOrientation::Portrait,
        UIDeviceOrientationPortraitUpsideDown => DeviceOrientation::PortraitUpsideDown,
        UIDeviceOrientationLandscapeLeft => DeviceOrientation::LandscapeLeft,
        UIDeviceOrientationLandscapeRight => DeviceOrientation::LandscapeRight,
        _ => unimplemented!("Orientation {} not handled yet", orientation),
    })});
}
- (())setStatusBarOrientation:(UIInterfaceOrientation)orientation
                     animated:(bool)_animated {
    // TODO: animation
    msg![env; this setStatusBarOrientation:orientation]
}

- (bool)isIdleTimerDisabled {
    !env.window().is_screen_saver_enabled()
}
- (())setIdleTimerDisabled:(bool)disabled {
    env.on_parent_stack_in_coroutine(|window, _| window.set_screen_saver_enabled(!disabled))
}

// [扫描修 2026-09-15] F12-5:打开外链不再退出游戏。
// 根因:上游照搬 iPhone OS 2/3(无多任务)的语义——宿主浏览器打开 URL 后无条件 exit。
// 本游戏 Info.plist MinimumOSVersion=4.3,真机是多任务:openURL: 切到 Safari/App Store,游戏留在
// 后台、回来继续。于是点「播放动画」(-[BuildingView onPlayMovie]@0xd38b8)、广告/合作建筑跳商店、
// 「检查新版本」等按钮,在 touchHLE 里等于直接关游戏(离线也一样)。
// 修法(补全原版多任务行为):
// - 只有 http/https 交给宿主浏览器打开,成功返回 YES,游戏继续运行;
// - itms:/itms-apps: 以及其它自定义 scheme(微信、广告 SDK 等)在宿主上没有对应程序,
//   不打开、记日志、返回 NO(与真机"没有能处理的 App"一致,游戏自己处理失败)。
// - 兼容上游:应用 MinimumOSVersion 缺失或主版本 < 4(老游戏,如 Super Monkey Ball 每帧 openURL
//   且不看返回值)仍保留"打开即退出";也可用 MOLE_OPENURL_EXIT=1 强制恢复、=0 强制关闭。
// 浏览器抢走焦点属预期;桌面只有最小化才会触发失活(见 handle_window_minimized)。
- (bool)openURL:(id)url { // NSURL
    let Some(url_string) = url_to_string(env, url) else {
        log!("[外链] openURL: 参数为 nil 或取不到 absoluteString,返回 NO");
        return false;
    };

    if legacy_exit_on_open_url(env) {
        // 上游原行为(见上方注释):打开后退出。
        if let Err(e) = crate::window::open_url(env, &url_string) {
            echo!("App opened URL {:?} unsuccessfully ({}), exiting.", url_string, e);
        } else {
            echo!("App opened URL {:?}, exiting.", url_string);
        }

        // iPhone OS doesn't really do multitasking, so the app expects to close
        // when a URL is opened, e.g. Super Monkey Ball keeps opening the URL every
        // frame! Super Monkey Ball also doesn't check whether opening failed, so
        // it's probably best to always exit.
        exit(env);
        return true;
    }

    if !url_is_web(&url_string) {
        echo!("[外链] 非 http/https 链接(App Store 等在宿主上无法打开),不打开并返回 NO:{:?}", url_string);
        return false;
    }
    if env.options.headless {
        log!("[外链] 无头模式不打开浏览器,返回 NO:{:?}", url_string);
        return false;
    }
    match crate::window::open_url(env, &url_string) {
        Ok(()) => {
            echo!("[外链] 已用宿主浏览器打开 {:?},游戏继续运行(不退出)", url_string);
            true
        }
        Err(e) => {
            echo!("[外链] 宿主浏览器打开 {:?} 失败({}),返回 NO", url_string, e);
            false
        }
    }
}

// [扫描修 2026-09-15] F12-5:补 canOpenURL:(此前未实现,靠找不到方法时的 nil 兜底恒为 NO)。
// 与 openURL: 的判定保持一致:http/https → YES;itms*/其它 scheme → NO。
// 游戏的 -[Building openAppViewInAppStore]、SealExchangeLayer/ActivityBulletinLayer
// onChooseToCheckAppVersionOrNot、-[iMoleVillageAppDelegate alertView:clickedButtonAtIndex:] 等
// 会先问 canOpenURL: 再决定是否 openURL:;广告 SDK 用它探测已装 App(自定义 scheme)仍得到 NO。
- (bool)canOpenURL:(id)url { // NSURL
    let Some(url_string) = url_to_string(env, url) else {
        return false;
    };
    let ok = url_is_web(&url_string);
    log_dbg!("[外链] canOpenURL:{:?} → {}", url_string, ok);
    ok
}

// TODO: ignore touches
-(())beginIgnoringInteractionEvents {
    log!("TODO: ignoring beginIgnoringInteractionEvents");
}
- (bool)isIgnoringInteractionEvents {
    false
}
-(())endIgnoringInteractionEvents {
    log!("TODO: ignoring endIgnoringInteractionEvents");
}

- (id)keyWindow {
    let Some(key_window) = env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .key_window else {
        return nil;
    };
    assert!(env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .windows
        .contains(&key_window));
    key_window
}

- (id)windows {
    let windows: Vec<id> = (*env
        .framework_state
        .uikit
        .ui_view
        .ui_window
        .windows).to_vec();
    for window in &windows {
        retain(env, *window);
    }
    let windows = ns_array::from_vec(env, windows);
    autorelease(env, windows)
}

- (())registerForRemoteNotificationTypes:(UIRemoteNotificationType)types {
    log!("TODO: ignoring registerForRemoteNotificationTypes:{}", types);
}

- (NSInteger)applicationIconBadgeNumber {
    0 // default value
}
- (())setApplicationIconBadgeNumber:(NSInteger)bn {
    log!("TODO: ignoring setApplicationIconBadgeNumber:{}", bn);
}

- (bool)applicationSupportsShakeToEdit {
    true // default value
}
- (())setApplicationSupportsShakeToEdit:(bool)enable {
    log!("TODO: ignoring setApplicationSupportsShakeToEdit:{}", enable);
}

// UIResponder implementation
// From the Apple UIView docs regarding [UIResponder nextResponder]:
// "The shared UIApplication object normally returns nil, but it returns its
//  app delegate if that object is a subclass of UIResponder and hasn’t
//  already been called to handle the event."
- (id)nextResponder {
    let delegate = msg![env; this delegate];
    let app_delegate_class = msg![env; delegate class];
    let ui_responder_class = env.objc.get_known_class("UIResponder", &mut env.mem);
    if env.objc.class_is_subclass_of(app_delegate_class, ui_responder_class) {
        // TODO: Send nil if it's already been called to handle the event
        delegate
    } else {
        nil
    }
}

- (())cancelAllLocalNotifications {
    log!("TODO: [(UIApplication*){:?} cancelAllLocalNotifications", this);
}
- (())scheduleLocalNotification:(id)local_notif { // UILocalNotification *
    log!("TODO: [(UIApplication*){:?} scheduleLocalNotification:{:?}", this, local_notif);
}

@end

};

/// `UIApplicationMain`, the entry point of the application.
///
/// This function should never return.
pub(super) fn UIApplicationMain(
    env: &mut Environment,
    _argc: i32,
    _argv: MutPtr<MutPtr<u8>>,
    principal_class_name: id, // NSString*
    delegate_class_name: id,  // NSString*
) {
    // UIKit creates and drains autorelease pools when handling events.
    // It's not clear what granularity this should happen with, but this
    // granularity has already caught several bugs. :)

    let ui_application = {
        let pool: id = msg_class![env; NSAutoreleasePool new];

        let principal_class = if principal_class_name != nil {
            let name = ns_string::to_rust_string(env, principal_class_name);
            env.objc.get_known_class(&name, &mut env.mem)
        } else {
            env.objc.get_known_class("UIApplication", &mut env.mem)
        };
        let ui_application: id = msg![env; principal_class new];

        let device_family = env.options.device_family;
        if let Some(main_nib_filename) = env.bundle.main_nib_filename(device_family) {
            let ns_main_nib_filename = from_rust_string(env, main_nib_filename.to_string());
            // We need to check first if main nib file exists,
            // as `UINib nibWithNibName:bundle:` will crash on nonexistent
            // nib otherwise
            let type_: id = get_static_str(env, "nib");
            let bundle: id = msg_class![env; NSBundle mainBundle];
            let res: id = msg![env; bundle pathForResource:ns_main_nib_filename ofType:type_];
            if res != nil {
                let nib: id = msg_class![env; UINib nibWithNibName:ns_main_nib_filename bundle:nil];
                release(env, ns_main_nib_filename);
                let _: id = msg![env; nib instantiateWithOwner:ui_application
                                               options:nil];
            } else {
                log!(
                    "Warning: couldn't load main nib file {:?}",
                    env.bundle.main_nib_filename(device_family)
                );
            }
        }

        if env.bundle.status_bar_hidden() {
            let _: () = msg![env; ui_application setStatusBarHidden:true];
        }

        let delegate: id = msg![env; ui_application delegate];
        if delegate != nil {
            // The delegate was created while loading the nib file.
            // Retain it so it doesn't get deallocated when the autorelease pool
            // is drained. (See discussion in `setDelegate:`.)
            env.objc
                .borrow_mut::<UIApplicationHostObject>(ui_application)
                .delegate_is_retained = true;
            retain(env, delegate);
        } else {
            assert!(delegate_class_name != nil);
            if msg![env; delegate_class_name isEqual:principal_class_name] {
                // If same non-nil class name is used for both principal and
                // delegate, it means that app is using itself as a delegate
                let _: () = msg![env; ui_application setDelegate:ui_application];
            } else {
                // We have to construct the delegate.
                let name = ns_string::to_rust_string(env, delegate_class_name);
                let class = env.objc.get_known_class(&name, &mut env.mem);
                let delegate: id = msg![env; class new];
                let _: () = msg![env; ui_application setDelegate:delegate];
                assert!(delegate != nil);
            }
        };
        // We can't hang on to the delegate, the guest app may change it at any
        // time.

        let _: () = msg![env; pool drain];

        ui_application
    };

    {
        let pool: id = msg_class![env; NSAutoreleasePool new];
        let delegate: id = msg![env; ui_application delegate];
        // iOS 3+ apps usually use application:didFinishLaunchingWithOptions:,
        // and it seems to be prioritized over applicationDidFinishLaunching:.
        if env.objc.object_has_method_named(
            &env.mem,
            delegate,
            "application:didFinishLaunchingWithOptions:",
        ) {
            let empty_dict: id = msg_class![env; NSDictionary dictionary];
            () = msg![env; delegate application:ui_application didFinishLaunchingWithOptions:empty_dict];
        } else if env.objc.object_has_method_named(
            &env.mem,
            delegate,
            "applicationDidFinishLaunching:",
        ) {
            () = msg![env; delegate applicationDidFinishLaunching:ui_application];
        }

        let center: id = msg_class![env; NSNotificationCenter defaultCenter];
        let notif_name = get_static_str(env, UIApplicationDidFinishLaunchingNotification);
        // TODO: launch options in `userInfo` if it'll ever become a concern
        () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

        let _: () = msg![env; pool drain];
    }

    // Call layoutSubviews on all views in the view hierarchy.
    // See https://medium.com/geekculture/uiview-lifecycle-part-5-faa2d44511c9
    let views = env.framework_state.uikit.ui_view.views.clone();
    for view in views {
        () = msg![env; view layoutSubviews];
    }

    // Send applicationDidBecomeActive now that the application is ready to
    // become active.
    // [扫描修 2026-09-15] 抽成 send_did_become_active,与窗口还原(F12-3)共用,行为不变。
    send_did_become_active(env, ui_application);

    // FIXME: There are more messages we should send.

    // TODO: It might be nicer to return from this function (even though it's
    // conceptually noreturn) and set some global flag that changes how the
    // execution works from this point onwards, though the only real advantages
    // would be a prettier backtrace and maybe the quit button not having to
    // panic.
    let run_loop: id = msg_class![env; NSRunLoop mainRunLoop];
    let _: () = msg![env; run_loop run];
}

/// Tell the app it's about to quit and then exit.
pub(super) fn exit(env: &mut Environment) {
    let ui_application: id = msg_class![env; UIApplication sharedApplication];

    let center: id = msg_class![env; NSNotificationCenter defaultCenter];

    // [扫描修 2026-09-15] 抽成 send_will_resign_active,与窗口最小化(F12-3)共用,行为不变。
    // 即使之前因最小化已经发过一次失活,这里仍照发:原版失活回调里的 saveToLocal:/saveSettings
    // 是幂等存档,多发一次换取"退出前一定落盘",不冒丢档风险。
    send_will_resign_active(env, ui_application);

    {
        let pool: id = msg_class![env; NSAutoreleasePool new];
        let delegate: id = msg![env; ui_application delegate];
        if env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationWillTerminate:")
        {
            () = msg![env; delegate applicationWillTerminate:ui_application];
        }

        let notif_name = get_static_str(env, UIApplicationWillTerminateNotification);
        () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

        let _: () = msg![env; pool drain];
    };

    std::process::exit(0);
}

/// [扫描修 2026-09-15] 给应用委托发 `applicationWillResignActive:`(若实现)并广播
/// `UIApplicationWillResignActiveNotification`;先 `synchronize` 一次 NSUserDefaults。
/// 从 [exit] 中原样抽出,供退出与窗口最小化共用。
fn send_will_resign_active(env: &mut Environment, ui_application: id) {
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let pool: id = msg_class![env; NSAutoreleasePool new];

    // Skip NSUserDefaults code while in the app picker, otherwise we get
    // a strange error when existing touchHLE due to the fake bundle.
    if !env.is_app_picker {
        // Apple's docs (used to) vaguely mention that `synchronize` is
        // invoked on periodic intervals.
        // Second best - and implemented here - is to save before app exits.
        // TODO: call `synchronize` periodically
        let user_defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
        let _: bool = msg![env; user_defaults synchronize];
    }

    let delegate: id = msg![env; ui_application delegate];
    if delegate != nil
        && env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationWillResignActive:")
    {
        () = msg![env; delegate applicationWillResignActive:ui_application];
    }

    let notif_name = get_static_str(env, UIApplicationWillResignActiveNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

    let _: () = msg![env; pool drain];
}

/// [扫描修 2026-09-15] 给应用委托发 `applicationDidBecomeActive:`(若实现)并广播
/// `UIApplicationDidBecomeActiveNotification`。从 [UIApplicationMain] 中原样抽出,供启动与窗口还原共用。
fn send_did_become_active(env: &mut Environment, ui_application: id) {
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if delegate != nil
        && env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationDidBecomeActive:")
    {
        () = msg![env; delegate applicationDidBecomeActive:ui_application];
    }

    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let notif_name = get_static_str(env, UIApplicationDidBecomeActiveNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

    let _: () = msg![env; pool drain];
}

/// [扫描修 2026-09-15] F12-3:桌面窗口最小化(W2 在 window.rs 发 `Event::WindowMinimized`)。
///
/// 真机按 Home/来电 → `applicationWillResignActive:`,本游戏在里面暂停 CCDirector、pauseMiniGame、
/// saveToLocal:/saveSettings、排作物提醒;CDAudioManager 靠 UIApplicationWillResignActiveNotification
/// 自己暂停音乐。以前 touchHLE 桌面端完全不处理最小化:小游戏倒计时照跑、音乐照放、不触发失活存档。
///
/// 取舍:
/// - 只发"失活",【不】发 `applicationDidEnterBackground:` / `applicationWillEnterForeground:`:
///   桌面最小化不是进后台,而原版 WillEnterForeground(@0x1133c)会 disconnect + getServerTime 重连、
///   触发反作弊/IAP 检查,联机时有副作用。
/// - 原版 -[iMoleVillageAppDelegate applicationWillResignActive:]@0xfdb8 在 +0xe0 / +0x11e 判断
///   runningScene 是 LogoLayer / LoadingScene 时直接 `exit(0)`(真机按 Home 会杀掉还在启动/加载的游戏)。
///   touchHLE 最小化时进程并不挂起,照搬等于"最小化就关游戏",所以这两个场景下跳过本次失活(也就不配对发激活)。
/// - 普通失焦(点别的窗口)不处理,免得切个显示器音乐就停;由 W2 决定只在最小化时发事件。
pub(super) fn handle_window_minimized(env: &mut Environment) {
    if env.framework_state.uikit.ui_application.inactive_by_window {
        return;
    }
    if env.is_app_picker {
        return;
    }
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    if ui_application == nil {
        log!("[生命周期] 窗口最小化:UIApplication 尚未创建,忽略");
        return;
    }
    if running_scene_exits_on_resign(env, ui_application) {
        log!("[生命周期] 窗口最小化:当前在 LogoLayer/LoadingScene,原版失活回调此时会 exit(0),跳过失活");
        return;
    }
    env.framework_state.uikit.ui_application.inactive_by_window = true;
    log!("[生命周期] 窗口最小化 → applicationWillResignActive: + UIApplicationWillResignActiveNotification");
    send_will_resign_active(env, ui_application);
}

/// [扫描修 2026-09-15] F12-3:桌面窗口从最小化还原(`Event::WindowRestored`)。
/// 只有之前真的发过失活才配对发 `applicationDidBecomeActive:`(游戏在里面 resume/resumeMiniGame、
/// checkIsNightComing/showNightVillage、SystemTimeCheck check、clearAllNotification),
/// CDAudioManager 收到 UIApplicationDidBecomeActiveNotification 恢复音乐。
pub(super) fn handle_window_restored(env: &mut Environment) {
    if !env.framework_state.uikit.ui_application.inactive_by_window {
        return;
    }
    env.framework_state.uikit.ui_application.inactive_by_window = false;
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    if ui_application == nil {
        return;
    }
    log!("[生命周期] 窗口还原 → applicationDidBecomeActive: + UIApplicationDidBecomeActiveNotification");
    send_did_become_active(env, ui_application);
}

/// [扫描修 2026-09-15] 复刻原版失活回调开头的两道判断(@0xfe90 isKindOfClass:LogoLayer、
/// @0xfece isKindOfClass:LoadingScene,命中即 exit(0)),用于决定桌面最小化时要不要发失活。
/// 只在应用委托确实是本游戏的 iMoleVillageAppDelegate 时才去碰 CCDirector(别的应用不会伪造出这个类);
/// 类名沿 isa → superclass 链逐级比对,不调用 get_known_class,避免给不存在的类造假类。
fn running_scene_exits_on_resign(env: &mut Environment, ui_application: id) -> bool {
    let delegate: id = msg![env; ui_application delegate];
    if delegate == nil {
        return false;
    }
    let delegate_class = ObjC::read_isa(delegate, &env.mem);
    if delegate_class == nil
        || env.objc.try_get_class_name(delegate_class) != Some("iMoleVillageAppDelegate")
    {
        return false;
    }
    let director_class = env.objc.get_known_class("CCDirector", &mut env.mem);
    let director: id = msg![env; director_class sharedDirector];
    if director == nil {
        return false;
    }
    let scene: id = msg![env; director runningScene];
    if scene == nil {
        // 原版 [nil isKindOfClass:] 为 NO,不会 exit。
        return false;
    }
    let mut class = ObjC::read_isa(scene, &env.mem);
    for _ in 0..64 {
        if class == nil {
            break;
        }
        match env.objc.try_get_class_name(class) {
            Some("LogoLayer") | Some("LoadingScene") => return true,
            Some(_) => {}
            None => break,
        }
        class = env.objc.get_superclass(class);
    }
    false
}

/// [扫描修 2026-09-15] F12-5:NSURL → 字符串;nil 或 absoluteString 为 nil 时返回 None。
fn url_to_string(env: &mut Environment, url: id) -> Option<String> {
    if url == nil {
        return None;
    }
    let absolute: id = msg![env; url absoluteString];
    if absolute == nil {
        return None;
    }
    Some(ns_string::to_rust_string(env, absolute).into_owned())
}

/// [扫描修 2026-09-15] F12-5:是否 http/https 链接(scheme 不区分大小写)。
fn url_is_web(url: &str) -> bool {
    let scheme = url.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
    scheme == "http" || scheme == "https"
}

/// [扫描修 2026-09-15] F12-5:是否沿用上游"openURL: 后退出"的老行为。
/// MOLE_OPENURL_EXIT=1 强制退出、=0 强制不退出;否则按 Info.plist MinimumOSVersion 判断:
/// 缺失或主版本 < 4(iPhone OS 3 及以前、无多任务)→ 退出;≥ 4(本游戏 4.3)→ 不退出。
fn legacy_exit_on_open_url(env: &Environment) -> bool {
    match std::env::var("MOLE_OPENURL_EXIT").ok().as_deref() {
        Some("1") => return true,
        Some("0") => return false,
        _ => {}
    }
    match env.bundle.minimum_os_version() {
        Some(version) => version
            .split('.')
            .next()
            .and_then(|major| major.trim().parse::<u32>().ok())
            .is_none_or(|major| major < 4),
        None => true,
    }
}

/// App life-cycle notifications
const UIApplicationDidFinishLaunchingNotification: &str =
    "UIApplicationDidFinishLaunchingNotification";
const UIApplicationDidBecomeActiveNotification: &str = "UIApplicationDidBecomeActiveNotification";
const UIApplicationDidEnterBackgroundNotification: &str =
    "UIApplicationDidEnterBackgroundNotification";
const UIApplicationWillEnterForegroundNotification: &str =
    "UIApplicationWillEnterForegroundNotification";
const UIApplicationWillResignActiveNotification: &str = "UIApplicationWillResignActiveNotification";
const UIApplicationWillTerminateNotification: &str = "UIApplicationWillTerminateNotification";
/// Other app notifications
const UIApplicationLaunchOptionsRemoteNotificationKey: &str =
    "UIApplicationLaunchOptionsRemoteNotificationKey";
const UIApplicationDidReceiveMemoryWarningNotification: &str =
    "UIApplicationDidReceiveMemoryWarningNotification";

/// `UIApplicationLaunchOptionsKey` and `NSNotificationName` values.
/// (Both types are strings)
pub const CONSTANTS: ConstantExports = &[
    (
        // UIBackgroundTaskIdentifier UIBackgroundTaskInvalid = NSUIntegerMax.
        // Not an NSString; it's an integer constant read directly as a word.
        // Without exporting it, any code reading *(&UIBackgroundTaskInvalid)
        // null-derefs (e.g. InMobi's -[IMNiceParamsMgr init]).
        "_UIBackgroundTaskInvalid",
        HostConstant::Custom(|env| {
            env.mem
                .alloc_and_write::<u32>(u32::MAX)
                .cast_void()
                .cast_const()
        }),
    ),
    (
        "_UIApplicationDidFinishLaunchingNotification",
        HostConstant::NSString(UIApplicationDidFinishLaunchingNotification),
    ),
    (
        "_UIApplicationDidBecomeActiveNotification",
        HostConstant::NSString(UIApplicationDidBecomeActiveNotification),
    ),
    (
        "_UIApplicationDidEnterBackgroundNotification",
        HostConstant::NSString(UIApplicationDidEnterBackgroundNotification),
    ),
    (
        "_UIApplicationWillEnterForegroundNotification",
        HostConstant::NSString(UIApplicationWillEnterForegroundNotification),
    ),
    (
        "_UIApplicationWillResignActiveNotification",
        HostConstant::NSString(UIApplicationWillResignActiveNotification),
    ),
    (
        "_UIApplicationWillTerminateNotification",
        HostConstant::NSString(UIApplicationWillTerminateNotification),
    ),
    (
        "_UIApplicationDidReceiveMemoryWarningNotification",
        HostConstant::NSString(UIApplicationDidReceiveMemoryWarningNotification),
    ),
    (
        "_UIApplicationLaunchOptionsRemoteNotificationKey",
        HostConstant::NSString(UIApplicationLaunchOptionsRemoteNotificationKey),
    ),
];

pub const FUNCTIONS: FunctionExports = &[export_c_func!(UIApplicationMain(_, _, _, _))];
