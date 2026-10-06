/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIDevice`.

use crate::dyld::ConstantExports;
use crate::dyld::HostConstant;
use crate::environment::Environment;
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::{ns_string, NSInteger};
use crate::msg_class;
use crate::objc::{
    id, msg, objc_classes, todo_objc_setter, ClassExports, NSZonePtr, TrivialHostObject,
};
use crate::window::{get_battery_status, BatteryState, DeviceFamily, DeviceOrientation};

/// [2026-10-06 第九轮 R9-A2] 离线时 [UIDevice name] 返回的宿主设备名(只取一次)。拿不到就用「iPad」(游戏按 iPad 跑,
/// 也是出厂未改名 iPad 的默认名)。去掉首尾空白与控制字符,最长 40 个字符。
/// - macOS:系统设置里的「电脑名称」(scutil --get ComputerName,如「某某的 MacBook Pro」);
/// - Windows:COMPUTERNAME;Linux:主机名;
/// - 安卓:用户在「关于手机」里设的设备名(persist.sys.device_name,部分 ROM 有),否则市场名(ro.product.marketname),
///   否则型号(ro.product.model);没有 JNI,只读系统属性;
/// - iOS:iOS 16 起普通应用拿到的设备名本来就只是「iPhone」/「iPad」,这里按 hw.machine 的机型族给出同样的结果。
fn offline_device_name() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let raw = host_device_name().unwrap_or_default();
        let cleaned: String = raw
            .trim()
            .chars()
            .filter(|c| !c.is_control())
            .take(40)
            .collect();
        let name = if cleaned.trim().is_empty() {
            "iPad".to_string()
        } else {
            cleaned.trim().to_string()
        };
        log!("[设备名] 离线 [UIDevice name] = 「{}」(新号默认昵称)", name);
        name
    })
}

#[cfg(target_os = "macos")]
fn host_device_name() -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

#[cfg(windows)]
fn host_device_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(target_os = "linux")]
fn host_device_name() -> Option<String> {
    std::fs::read_to_string("/etc/hostname").ok()
}

#[cfg(target_os = "android")]
fn host_device_name() -> Option<String> {
    fn prop(key: &str) -> Option<String> {
        let key = std::ffi::CString::new(key).ok()?;
        // PROP_VALUE_MAX = 92
        let mut buf = [0 as std::ffi::c_char; 92];
        let n = unsafe { ::libc::__system_property_get(key.as_ptr(), buf.as_mut_ptr()) };
        if n <= 0 {
            return None;
        }
        let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        Some(s.to_string_lossy().into_owned()).filter(|s| !s.trim().is_empty())
    }
    prop("persist.sys.device_name")
        .or_else(|| prop("ro.product.marketname"))
        .or_else(|| prop("ro.product.model"))
}

#[cfg(target_os = "ios")]
fn host_device_name() -> Option<String> {
    let key = std::ffi::CString::new("hw.machine").ok()?;
    let mut buf = [0u8; 64];
    let mut len: ::libc::size_t = buf.len();
    let r = unsafe {
        ::libc::sysctlbyname(
            key.as_ptr(),
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return None;
    }
    let machine = String::from_utf8_lossy(&buf[..len.min(buf.len())]);
    Some(if machine.starts_with("iPhone") { "iPhone" } else { "iPad" }.to_string())
}

#[cfg(not(any(
    target_os = "macos",
    windows,
    target_os = "linux",
    target_os = "android",
    target_os = "ios"
)))]
fn host_device_name() -> Option<String> {
    None
}

pub const UIDeviceOrientationDidChangeNotification: &str =
    "UIDeviceOrientationDidChangeNotification";

pub type UIDeviceOrientation = NSInteger;
#[allow(dead_code)]
pub const UIDeviceOrientationUnknown: UIDeviceOrientation = 0;
pub const UIDeviceOrientationPortrait: UIDeviceOrientation = 1;
pub const UIDeviceOrientationPortraitUpsideDown: UIDeviceOrientation = 2;
pub const UIDeviceOrientationLandscapeLeft: UIDeviceOrientation = 3;
pub const UIDeviceOrientationLandscapeRight: UIDeviceOrientation = 4;
#[allow(dead_code)]
pub const UIDeviceOrientationFaceUp: UIDeviceOrientation = 5;
#[allow(dead_code)]
pub const UIDeviceOrientationFaceDown: UIDeviceOrientation = 6;

pub type UIDeviceBatteryState = NSInteger;
pub const UIDeviceBatteryStateUnknown: UIDeviceBatteryState = 0;
pub const UIDeviceBatteryStateUnplugged: UIDeviceBatteryState = 1;
pub const UIDeviceBatteryStateCharging: UIDeviceBatteryState = 2;
pub const UIDeviceBatteryStateFull: UIDeviceBatteryState = 3;

type UIUserInterfaceIdiom = NSInteger;
#[allow(dead_code)]
const UIUserInterfaceIdiomUnspecified: UIUserInterfaceIdiom = -1;
const UIUserInterfaceIdiomPhone: UIUserInterfaceIdiom = 0;
const UIUserInterfaceIdiomPad: UIUserInterfaceIdiom = 1;

#[derive(Default)]
pub struct State {
    current_device: Option<id>,
    is_generating_device_orientation_notifications: bool,
}
impl State {
    pub fn is_generating_device_orientation_notifications(&self) -> bool {
        self.is_generating_device_orientation_notifications
    }
}

pub const CONSTANTS: ConstantExports = &[(
    "_UIDeviceOrientationDidChangeNotification",
    HostConstant::NSString(UIDeviceOrientationDidChangeNotification),
)];

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIDevice: NSObject

+ (id)currentDevice {
    if let Some(device) = env.framework_state.uikit.ui_device.current_device {
        device
    } else {
        let new = msg_class![env; _touchHLE_UIDevice_Static alloc];
        env.framework_state.uikit.ui_device.current_device = Some(new);
        new
    }
}

- (())beginGeneratingDeviceOrientationNotifications {
    log_dbg!("[UIDevice beginGeneratingDeviceOrientationNotifications]");
    env.framework_state.uikit.ui_device.is_generating_device_orientation_notifications = true;
}
- (())endGeneratingDeviceOrientationNotifications {
    log_dbg!("[UIDevice endGeneratingDeviceOrientationNotifications]");
    env.framework_state.uikit.ui_device.is_generating_device_orientation_notifications = false;
}
- (bool)isGeneratingDeviceOrientationNotifications {
    let res = env.framework_state.uikit.ui_device.is_generating_device_orientation_notifications;
    log_dbg!("[UIDevice isGeneratingDeviceOrientationNotifications] -> {}", res);
    res
}

- (id)model {
    // TODO: Hardcoded to iPhone for now
    ns_string::get_static_str(env, "iPhone")
}
- (id)localizedModel {
    // TODO: localization
    msg![env; this model]
}

- (id)name {
    // [2026-10-06 第九轮 R9-A2] 原版离线新号的昵称来自这里:-[UserInfoData init] 0xb90c6 [UIDevice currentDevice] →
    // 0xb90d6 name → 0xb90fe 写 name_(+4),即真机设备名(如「某某的 iPad」)。以前写死「iPhone」,而游戏按 iPad 跑。
    // 用户拍板:离线用真实设备名(见 offline_device_name);联机保持原样,不改变登录上报的设备信息。只影响之后新建的号,
    // 已有存档里的昵称不动,仍可在换头像面板改名。
    if env.options.network_access {
        return ns_string::get_static_str(env, "iPhone");
    }
    let name = offline_device_name();
    ns_string::get_static_str(env, name)
}

- (id)systemName {
    ns_string::get_static_str(env, "iPhone OS")
}

// NSString
- (id)systemVersion {
    ns_string::get_static_str(env, "2.0")
}

- (id)uniqueIdentifier {
    // Aspen Simulator returns (null) here
    // A device unique identifier must be 40 characters long
    ns_string::get_static_str(env, "touchHLEdevice..........................")
}

- (bool)isMultitaskingSupported {
    false
}

- (UIDeviceOrientation)orientation {
    match env.window().current_rotation() {
        DeviceOrientation::Portrait => UIDeviceOrientationPortrait,
        DeviceOrientation::PortraitUpsideDown => UIDeviceOrientationPortraitUpsideDown,
        DeviceOrientation::LandscapeLeft => UIDeviceOrientationLandscapeLeft,
        DeviceOrientation::LandscapeRight => UIDeviceOrientationLandscapeRight
    }
}
- (())setOrientation:(UIDeviceOrientation)orientation {
    let prev_orientation = env.window().current_rotation();
    env.on_parent_stack_in_coroutine(|window, _| {window.rotate_device(match orientation {
        UIDeviceOrientationPortrait => DeviceOrientation::Portrait,
        UIDeviceOrientationPortraitUpsideDown => DeviceOrientation::PortraitUpsideDown,
        UIDeviceOrientationLandscapeLeft => DeviceOrientation::LandscapeLeft,
        UIDeviceOrientationLandscapeRight => DeviceOrientation::LandscapeRight,
        _ => unimplemented!("Orientation {} not handled yet", orientation),
    })});
    if prev_orientation != env.window().current_rotation() {
        generate_device_orientation_notification(env);
    }
}

- (bool)isBatteryMonitoringEnabled {
    true
}
- (())setBatteryMonitoringEnabled:(bool)enabled {
    todo_objc_setter!(this, enabled);
    assert!(enabled);
}
- (f32)batteryLevel {
    let pct = get_battery_status().0;
    if pct < 0 {
        log_dbg!("batteryLevel percentage could not be determined, returning 100% for compatibility");
        return 1.0
    }
    pct as f32 / 100.0 // narrow down to 0.0 - 1.0
}
- (UIDeviceBatteryState)batteryState {
    match get_battery_status().1 {
        BatteryState::Unknown => UIDeviceBatteryStateUnknown,
        BatteryState::OnBattery => UIDeviceBatteryStateUnplugged,
        BatteryState::NoBattery | BatteryState::Charging => UIDeviceBatteryStateCharging,
        BatteryState::Full => UIDeviceBatteryStateFull,
    }
}

- (UIUserInterfaceIdiom)userInterfaceIdiom {
    match env.window().device_family() {
        DeviceFamily::iPhone => UIUserInterfaceIdiomPhone,
        DeviceFamily::iPad => UIUserInterfaceIdiomPad,
    }
}

@end

// Private static implementation of UIDevice, used for the current device
@implementation _touchHLE_UIDevice_Static: UIDevice

+ (id)allocWithZone:(NSZonePtr)_zone {
    env.objc.alloc_static_object(
        this,
        Box::new(TrivialHostObject),
        &mut env.mem
    )
}

- (id) retain { this }
- (()) release {}
- (id) autorelease { this }

@end

};

pub fn generate_device_orientation_notification(env: &mut Environment) {
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let name = get_static_str(env, UIDeviceOrientationDidChangeNotification);
    let device: id = msg_class![env; UIDevice currentDevice];
    let _: () = msg![env; center postNotificationName:name object:device];
}
