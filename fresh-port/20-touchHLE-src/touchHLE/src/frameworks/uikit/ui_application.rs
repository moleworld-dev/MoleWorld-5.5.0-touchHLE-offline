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
    /// [补完 2026-09-15] 正在切后台挂起流程中(suspend_app 从发失活回调到发完激活回调)。
    /// 防止回调里的嵌套 run loop 再次触发挂起而重入。
    suspended: bool,
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

/// [MoleWorld iOS] iOS `applicationWillResignActive:` — the app lost focus
/// (Control Center, home-indicator, notification, or about to background). Tell
/// the guest to pause + save, WITHOUT exiting and WITHOUT gating GL (we're still
/// foreground; GL is legal). If this becomes a true background,
/// [did_enter_background] follows.
#[cfg(target_os = "ios")]
pub(super) fn resign_active(env: &mut Environment) {
    // Don't message the fake app-picker bundle: its game singletons don't exist.
    if env.is_app_picker {
        return;
    }
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let pool: id = msg_class![env; NSAutoreleasePool new];

    // Flush NSUserDefaults now (the game also saves in its own handler; this
    // matches the old exit() behavior so a later OS kill can't lose data).
    let user_defaults: id = msg_class![env; NSUserDefaults standardUserDefaults];
    let _: bool = msg![env; user_defaults synchronize];

    let delegate: id = msg![env; ui_application delegate];
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "applicationWillResignActive:")
    {
        () = msg![env; delegate applicationWillResignActive:ui_application];
    }
    let notif_name = get_static_str(env, UIApplicationWillResignActiveNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];
    let _: () = msg![env; pool drain];
}

/// [MoleWorld iOS] iOS `applicationDidEnterBackground:` — the TRUE background.
/// Gate GL FIRST (so neither this delivery nor anything after touches the GPU),
/// then deliver the message (the game calls `stopAnimation`). iOS kills any app
/// that issues GL after this returns.
#[cfg(target_os = "ios")]
pub(super) fn did_enter_background(env: &mut Environment) {
    if let Some(window) = env.window.as_mut() {
        window.set_backgrounded(true);
    }
    if env.is_app_picker {
        return;
    }
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "applicationDidEnterBackground:")
    {
        () = msg![env; delegate applicationDidEnterBackground:ui_application];
    }
    let notif_name = get_static_str(env, UIApplicationDidEnterBackgroundNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];
    let _: () = msg![env; pool drain];
}

/// [MoleWorld iOS] iOS `applicationWillEnterForeground:` — leaving the
/// background. Clear the GL gate FIRST (GL legal again) so the game's
/// `startAnimation` render path works, then deliver the message.
#[cfg(target_os = "ios")]
pub(super) fn will_enter_foreground(env: &mut Environment) {
    if let Some(window) = env.window.as_mut() {
        window.set_backgrounded(false);
    }
    if env.is_app_picker {
        return;
    }
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "applicationWillEnterForeground:")
    {
        () = msg![env; delegate applicationWillEnterForeground:ui_application];
    }
    let notif_name = get_static_str(env, UIApplicationWillEnterForegroundNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];
    let _: () = msg![env; pool drain];
}

/// [MoleWorld iOS] iOS `applicationDidBecomeActive:` — every resume (overlay
/// dismissal AND background return). Ensure the GL gate is clear (idempotent),
/// then tell the guest to resume.
#[cfg(target_os = "ios")]
pub(super) fn did_become_active(env: &mut Environment) {
    if let Some(window) = env.window.as_mut() {
        window.set_backgrounded(false);
    }
    if env.is_app_picker {
        return;
    }
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "applicationDidBecomeActive:")
    {
        () = msg![env; delegate applicationDidBecomeActive:ui_application];
    }
    let notif_name = get_static_str(env, UIApplicationDidBecomeActiveNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];
    let _: () = msg![env; pool drain];
}

/// [扫描修 2026-09-15] 给应用委托发 `applicationWillResignActive:`(若实现)并广播
/// `UIApplicationWillResignActiveNotification`;先 `synchronize` 一次 NSUserDefaults。
/// 从 [exit] 中原样抽出,供退出与窗口最小化共用。
/// [补完 2026-09-15] 切后台挂起([suspend_app])也用它。
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
/// [补完 2026-09-15] 切后台挂起回来([suspend_app])也用它。
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
///   桌面最小化不是进后台。[补完 2026-09-15] 更正:原版 WillEnterForeground(@0x1133c)会做贝壳/金币反作弊
///   检查(hasIllegalApp,以及与进后台 @0x11270 记下的 _currentVipGold/_currentGold 比较)、startAnimation、
///   IAP 指示器收尾,已连接(isConnected @0x1171c 为真)时才发 getServerTime / 每日任务请求,联机时有副作用;
///   该回调里没有 disconnect,disconnect 在 applicationWillTerminate:(@0x119e2)。
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

/// [补完 2026-09-15] 切后台时是否走「挂起」而不是上游的「退出」:仅 Android(运行期判断,
/// 桌面宿主构建也会对挂起路径做类型检查)。应用选择器(is_app_picker)没有游戏委托,沿用退出。
/// [同步 2026-09-24] iOS 不走这里:frameworks/uikit.rs 对 iOS 用 cfg(target_os = "ios") 的专用臂
/// (上面的 [resign_active] / [did_enter_background] / [will_enter_foreground] / [did_become_active],
/// 暂停不退出、进后台先关 GL 闸门),本函数只在非 iOS 构建里存在。
#[cfg(not(target_os = "ios"))]
pub(super) fn suspend_on_background(env: &Environment) -> bool {
    std::env::consts::OS == "android" && !env.is_app_picker
}

/// [补完 2026-09-15] MOLE_BG_CALLBACKS=0:切后台挂起时只发失活/激活(与桌面最小化一致),
/// 不发 applicationDidEnterBackground: / applicationWillEnterForeground:。默认(未设或非 0)发。
fn background_callbacks_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        std::env::var("MOLE_BG_CALLBACKS")
            .map(|v| v.trim() != "0")
            .unwrap_or(true)
    })
}

/// [补完 2026-09-15] 切后台挂起:失活 →(进后台)→ 模拟器线程挂起 →(进前台)→ 激活,不退出、不重置。
///
/// 调用点都在 frameworks/uikit.rs 的 handle_events:Android 的 `Event::AppWillResignActive`
/// (end = Foreground,等 SDL 回前台事件),以及 /tmp/mole_input 注入 `suspend <秒数>`(end = Timer,
/// 在桌面上无头验证同一条路径)。handle_events 在 run loop 顶部、定时器阶段(CADisplayLink →
/// CCDirector mainLoop/drawScene)之前被调用,与分发触摸一样可以安全发 msg_send。
///
/// 真机按 Home:applicationWillResignActive: → applicationDidEnterBackground:;回来:
/// applicationWillEnterForeground: → applicationDidBecomeActive:(每个回调之后广播对应通知)。本游戏:
/// - 失活 @0xfdb8:SystemTimeCheck start、GameSettings saveSettings、[CCDirector pause]、pauseMiniGame、
///   GameData saveToLocal:、NewSceneData updateBeginTime、排作物成熟/枯萎本地通知;runningScene 是
///   LogoLayer/LoadingScene 时直接 exit(0)。CDAudioManager 靠 UIApplicationWillResignActiveNotification
///   停背景音乐(-[GameSoundManager asynchronousSetup] 在 0x152fe 处 setResignBehavior:1(kAMRBStopPlay)
///   autoHandle:1),激活通知时恢复。
/// - 进后台 @0x11270:[CCDirector stopAnimation],把当前贝壳(vipGoldWithNewType)、金币记进
///   _currentVipGold/_currentGold。
/// - 进前台 @0x1133c:[UIDevice hasIllegalApp] 非 0、或贝壳/金币与进后台时不同 → showCheatWarningMessage
///   (mole_cheats 已拦截);[CCDirector startAnimation];在黄金岛(curSceneId==10)时
///   [currentVillageLayer resetReconnectCounter];isInPurchase/transactionExist → HideIndicator;
///   currentGameMode==-1 时给 runningScene 子节点里的 LoadingScene 重新 schedule LoadingLayer 的选择子;
///   isConnected 为真才 getServerTime + getDailyTaskListFromServerWithSceneId:。反汇编里没有 disconnect
///   (扫描记录这一条有误,disconnect 在 applicationWillTerminate:@0x119e2);联机时发的请求与真机回前台一致。
///   [补完 2026-09-15] 更正离线情形:主村离线 isConnected 为假,不发;离线黄金岛(进岛窗口 / 岛加载中 / 在岛上)
///   mole_cheats::intercept 把 [NetworkManager isConnected] 强制为 1,回来时会发 getServerTime(@0x226970)与
///   getDailyTaskListFromServerWithSceneId:(@0x1cb5cc),但两者都经 sendPacket:commandId: 发包,被同一个岛上
///   条件块里的 `(_, "sendPacket:commandId:")` 臂吞掉,无副作用。挂起期间不跑 guest,贝壳/金币不会变,
///   反作弊比较不会误报。
/// - 激活 @0x10c20:resume、resumeMiniGame、SystemTimeCheck check、夜晚/灯光重算、clearAllNotification 等
///   (与桌面还原共用 send_did_become_active)。
/// 所以四个回调成对照发;MOLE_BG_CALLBACKS=0 时只发失活/激活。
///
/// 取舍:
/// - 发回调之前先丢掉窗口队列里还没交给游戏的输入,并把游戏里仍按着的触点以取消结束;回来前再清一次。
/// - LogoLayer/LoadingScene:原版失活会 exit(0)(真机上等于没有"后台"),这里四个回调都不发,只挂起。
/// - 桌面窗口已因最小化失活(inactive_by_window)时不重复发失活,回来也不发激活(留给窗口还原配对)。
/// - 挂起期间收到终止(SDL_QUIT / SDL_APP_TERMINATING),或回前台时 SDL 报渲染设备重置(原 EGL 上下文
///   恢复失败、GL 资源全失效):走 [exit](会再发一次失活并发 applicationWillTerminate:,saveToLocal: 幂等),
///   不在失效的 GL 上下文上继续跑。
/// - touchHLE 的 guest 线程在同一个宿主线程上协作调度,挂在这里时其它 guest 线程也一起停住。
/// - [补完 2026-09-15] 宿主 OpenAL 混音线程不会跟着停:发完回调后(不管发没发)把游戏音频静音,回前台先解除
///   再发进前台/激活回调,见 [mute_game_audio_for_suspend]。
pub(super) fn suspend_app(env: &mut Environment, end: crate::window::SuspendEnd, reason: &str) {
    use crate::window::SuspendOutcome;

    if env.framework_state.uikit.ui_application.suspended {
        log!("[生命周期] 已在挂起流程中,忽略再次触发的挂起({})", reason);
        return;
    }
    if env.window.is_none() {
        log!("[生命周期] 没有窗口(headless),无法挂起({})", reason);
        return;
    }
    env.framework_state.uikit.ui_application.suspended = true;

    // ① 输入收尾:队列里还没交给游戏的丢掉,游戏里仍按着的以取消结束。
    let dropped = env.window_mut().discard_pending_input();
    if dropped > 0 {
        log!("[生命周期] 丢弃挂起前尚未分发的输入事件 {} 个", dropped);
    }
    super::cancel_tracked_touches(env, "进入后台挂起");

    // ② 决定发哪些回调。
    let ui_application: id = msg_class![env; UIApplication sharedApplication];
    let callbacks = if env.is_app_picker || ui_application == nil {
        log!("[生命周期] UIApplication 尚未创建(或在应用选择器里):只挂起,不发生命周期回调");
        false
    } else if running_scene_exits_on_resign(env, ui_application) {
        log!("[生命周期] 当前在 LogoLayer/LoadingScene,原版失活回调此时会 exit(0):只挂起,不发生命周期回调");
        false
    } else {
        true
    };
    let send_resign = callbacks && !env.framework_state.uikit.ui_application.inactive_by_window;
    let send_background = callbacks && background_callbacks_enabled();

    log!(
        "[生命周期] 进入后台挂起({}):applicationWillResignActive:={} applicationDidEnterBackground:={}",
        reason,
        send_resign,
        send_background
    );
    if send_resign {
        send_will_resign_active(env, ui_application);
    }
    if send_background {
        send_did_enter_background(env, ui_application);
    }
    // [补完 2026-09-15] 不管上面发没发回调,挂起前都把游戏音频静音(宿主 OpenAL 混音线程不随模拟器线程挂起,
    // 循环音效会在后台一直响),见 mute_game_audio_for_suspend。放在回调之后:失活通知里 CDAudioManager
    // 先按原版停背景音乐。
    let muted_audio = mute_game_audio_for_suspend(env, ui_application);

    // ③ 挂起:必须在主栈上跑(Android 的 SDL poll 会走 JNI,见 on_parent_stack_in_coroutine)。
    let outcome = env
        .on_parent_stack_in_coroutine(move |window, _options| window.suspend_until_foreground(end));

    // ④ 回来。
    match outcome {
        SuspendOutcome::Resumed => {
            log!(
                "[生命周期] 回到前台({}):applicationWillEnterForeground:={} applicationDidBecomeActive:={}",
                reason,
                send_background,
                send_resign
            );
            // [补完 2026-09-15] 先解除挂起时加的静音,再发进前台/激活回调(CDAudioManager 在激活通知里按原版
            // 恢复背景音乐)。Terminate / RenderDeviceLost 分支不解除:马上退出,且 GameSettings saveSettings
            // (@0x185914)不读静音状态(游戏里 `mute` 选择子只有广告 SDK 的 IMMediaManager/IMMraidVideoPlayer 在用),
            // 不会把静音存进设置。
            unmute_game_audio_after_suspend(env, muted_audio);
            if send_background {
                send_will_enter_foreground(env, ui_application);
            }
            if send_resign {
                send_did_become_active(env, ui_application);
            }
            env.framework_state.uikit.ui_application.suspended = false;
            log!("[生命周期] 挂起流程结束,游戏从原画面继续运行");
        }
        SuspendOutcome::Terminate => {
            log!("[生命周期] 挂起期间系统要求结束应用 → 走退出流程(失活 + 终止回调落盘后退出)");
            exit(env);
        }
        SuspendOutcome::RenderDeviceLost => {
            log!("[生命周期] 回到前台时 GL 上下文已丢失,继续运行会花屏或在下次切换上下文时 panic → 存档后退出");
            echo!("[生命周期] GL 上下文在后台被系统回收,已存档并退出,请重新打开游戏。");
            exit(env);
        }
    }
}

/// [补完 2026-09-15] 给应用委托发 `applicationDidEnterBackground:`(若实现)并广播
/// `UIApplicationDidEnterBackgroundNotification`。只用于切后台挂起(见 [suspend_app])。
fn send_did_enter_background(env: &mut Environment, ui_application: id) {
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if delegate != nil
        && env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationDidEnterBackground:")
    {
        () = msg![env; delegate applicationDidEnterBackground:ui_application];
    }

    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let notif_name = get_static_str(env, UIApplicationDidEnterBackgroundNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

    let _: () = msg![env; pool drain];
}

/// [补完 2026-09-15] 给应用委托发 `applicationWillEnterForeground:`(若实现)并广播
/// `UIApplicationWillEnterForegroundNotification`。只用于切后台挂起(见 [suspend_app])。
fn send_will_enter_foreground(env: &mut Environment, ui_application: id) {
    let pool: id = msg_class![env; NSAutoreleasePool new];
    let delegate: id = msg![env; ui_application delegate];
    if delegate != nil
        && env
            .objc
            .object_has_method_named(&env.mem, delegate, "applicationWillEnterForeground:")
    {
        () = msg![env; delegate applicationWillEnterForeground:ui_application];
    }

    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let notif_name = get_static_str(env, UIApplicationWillEnterForegroundNotification);
    () = msg![env; center postNotificationName:notif_name object:ui_application userInfo:nil];

    let _: () = msg![env; pool drain];
}

/// [补完 2026-09-15] 切后台挂起期间把游戏音频静音;返回自己加了静音的 CDAudioManager,回前台时交给
/// [unmute_game_audio_after_suspend] 解除。没有动静音(不是本游戏 / 音频管理器未初始化 / 本来就静音)返回 None。
///
/// 根因:以前 Android 切后台直接退出,没有这个问题;改成挂起后,模拟器线程停住,但宿主 OpenAL Soft 的混音线程照跑
/// (Android 上它直连 OpenSLES,不走 SDL 音频;SDL 暂停时的 pauseAudio 只停 SDL 自己打开的设备),正在播放的
/// AL_LOOPING 音效会在后台一直响到用户回来。原版 CDAudioManager 的失活通知(-[GameSoundManager asynchronousSetup]
/// 在 0x152fe 设 kAMRBStopPlay)只停 CDLongAudioSource(背景音乐),不停 CDSoundEngine 的 OpenAL 音效源;游戏里
/// 至少 9 处 -[GameSoundManager playSound:loop:] 传 loop=YES(如 0xb3f52、0x14e68c、0x16a8cc 处 movs r3, #1)。
/// LogoLayer/LoadingScene 下连失活通知都不发。
///
/// 做法:本包拿不到宿主侧的 OpenAL 设备(设备表是 frameworks/openal.rs 私有的,也没有 ALC_SOFT_pause_device 绑定),
/// 退而用游戏自带的 CocosDenshion 静音:-[CDAudioManager setMute:YES](@0x2fcdfc,_mute 相同则直接返回;否则
/// [soundEngine setMute:] 记下 masterGain 到 _preMuteGain 后 alListenerf(AL_GAIN, 0),并把 audioSourceChannels 里
/// 每个 CDLongAudioSource 设为静音),回来时 setMute:NO 按 _preMuteGain 恢复。真机进后台是整个进程被冻结、回来后
/// 循环音效接着响;这里静音期间音源静默地继续推进,听感上等价。更彻底的做法是在音频模块补
/// alcDevicePauseSOFT / alcDeviceResumeSOFT 暂停 touchHLE 打开的全部 OpenAL 设备(连混音与 OpenSLES 输出一起停,
/// 也覆盖不走 CocosDenshion 的声音),补上后这里可以换成调用它。
///
/// 取舍:
/// - 只在应用委托是 iMoleVillageAppDelegate 时做(本游戏的类一定存在,不给别的应用造假类)。
/// - 只在 +[CDAudioManager sharedManagerState](@0x2fc620)== kAMStateInitialised(2,+sharedManager 建好后在
///   @0x2fc592 写入)时才碰:未初始化时 +sharedManager 会当场 alloc/init:(@0x2fc54e)建音频管理器,
///   不能在挂起时顺手初始化。CDSoundEngine 的 OpenAL 上下文若没建成,alListenerf 在 touchHLE 里只记日志跳过。
/// - 挂起前游戏已经静音就不动,回来也不解除;回来时只解除自己加的静音。
/// - 调用上下文与 suspend_app 相同(run loop 顶部的 handle_events),可以发 msg_send;都是游戏自己实现的方法。
fn mute_game_audio_for_suspend(env: &mut Environment, ui_application: id) -> Option<id> {
    if ui_application == nil || env.is_app_picker {
        return None;
    }
    let delegate: id = msg![env; ui_application delegate];
    if delegate == nil {
        return None;
    }
    let delegate_class = ObjC::read_isa(delegate, &env.mem);
    if delegate_class == nil
        || env.objc.try_get_class_name(delegate_class) != Some("iMoleVillageAppDelegate")
    {
        return None;
    }
    let manager_class = env.objc.get_known_class("CDAudioManager", &mut env.mem);
    // tAudioManagerState:0 未初始化 / 1 初始化中 / 2 已初始化。
    let state: i32 = msg![env; manager_class sharedManagerState];
    if state != 2 {
        log!(
            "[生命周期] CDAudioManager 尚未初始化完成(sharedManagerState={}),挂起期间不处理游戏音频",
            state
        );
        return None;
    }
    let manager: id = msg![env; manager_class sharedManager];
    if manager == nil {
        return None;
    }
    let already_muted: bool = msg![env; manager mute];
    if already_muted {
        log!("[生命周期] 游戏音频挂起前已是静音,挂起期间不改动");
        return None;
    }
    () = msg![env; manager setMute:true];
    log!("[生命周期] 挂起期间静音游戏音频([CDAudioManager setMute:YES]),循环音效不会在后台一直响");
    Some(manager)
}

/// [补完 2026-09-15] 回前台时解除 [mute_game_audio_for_suspend] 加的静音(`manager` 为 None 表示没加过,什么都不做)。
/// 挂起期间不跑 guest,静音状态不会被游戏改掉;保险起见仍先确认还是静音再解除。
fn unmute_game_audio_after_suspend(env: &mut Environment, manager: Option<id>) {
    let Some(manager) = manager else {
        return;
    };
    let still_muted: bool = msg![env; manager mute];
    if !still_muted {
        log!("[生命周期] 回到前台时游戏音频已不是静音,不再解除");
        return;
    }
    () = msg![env; manager setMute:false];
    log!("[生命周期] 回到前台,解除挂起时加的静音([CDAudioManager setMute:NO])");
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
