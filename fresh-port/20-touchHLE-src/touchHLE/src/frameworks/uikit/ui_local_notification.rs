/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UILocalNotification`.
//!
//! [2026-10-29 第十八轮 R18-C-2] 以前各 setter 只打 TODO,-[UIApplication scheduleLocalNotification:] 也是空操作。
//! 原版切后台时(-[iMoleVillageAppDelegate applicationWillResignActive:] → -[MKLocalNotificationsScheduler
//! scheduleNotificationOn:text:action:sound:launchImage:userInfo:]@0x183090)按本地存档预约「作物成熟 / 作物快枯萎 /
//! 摩尔派遣回来 / 太久没来」四条提醒,纯本地、不需要联网。现在把各字段存下来,交给 mole_host 转成宿主系统通知。

use crate::frameworks::foundation::{NSInteger, NSTimeInterval};
use crate::objc::{
    id, msg, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;

#[derive(Default)]
pub struct UILocalNotificationHostObject {
    /// `NSDate*`
    fire_date: id,
    /// `NSTimeZone*`
    time_zone: id,
    /// `NSString*`
    alert_body: id,
    /// `NSString*`
    alert_action: id,
    /// `NSString*`
    sound_name: id,
    /// `NSString*`
    alert_launch_image: id,
    /// `NSDictionary*`
    user_info: id,
    badge_number: NSInteger,
}
impl HostObject for UILocalNotificationHostObject {}

/// 换掉一个持有的对象字段:新值 retain、旧值 release(与 @property (retain) 一致)。
fn replace_field(
    env: &mut Environment,
    this: id,
    value: id,
    field: fn(&mut UILocalNotificationHostObject) -> &mut id,
) {
    retain(env, value);
    let old = std::mem::replace(
        field(env.objc.borrow_mut::<UILocalNotificationHostObject>(this)),
        value,
    );
    release(env, old);
}

/// 交给宿主排的一条通知:多少秒后触发、正文、按钮文字。
pub struct PendingNotification {
    pub delay_secs: f64,
    pub body: String,
    pub action: String,
}

/// 从游戏的 UILocalNotification 取出宿主需要的内容。没有触发时间或已过时返回 None(原版对过时的通知立即投递,
/// 本游戏只预约未来的时间,这里直接跳过)。
pub fn to_pending(env: &mut Environment, notif: id) -> Option<PendingNotification> {
    let (fire_date, body, action) = {
        let host = env.objc.borrow::<UILocalNotificationHostObject>(notif);
        (host.fire_date, host.alert_body, host.alert_action)
    };
    if fire_date == nil {
        return None;
    }
    let fire: NSTimeInterval = msg![env; fire_date timeIntervalSinceReferenceDate];
    let now = crate::frameworks::core_foundation::time::cf_absolute_time_now();
    let delay_secs = fire - now;
    if !delay_secs.is_finite() || delay_secs <= 0.0 {
        return None;
    }
    let to_string = |env: &mut Environment, s: id| -> String {
        if s == nil {
            String::new()
        } else {
            crate::frameworks::foundation::ns_string::to_rust_string(env, s).into_owned()
        }
    };
    let body = to_string(env, body);
    if body.is_empty() {
        return None;
    }
    let action = to_string(env, action);
    Some(PendingNotification {
        delay_secs,
        body,
        action,
    })
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UILocalNotification: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UILocalNotificationHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())dealloc {
    let host = env.objc.borrow::<UILocalNotificationHostObject>(this);
    let fields = [
        host.fire_date,
        host.time_zone,
        host.alert_body,
        host.alert_action,
        host.sound_name,
        host.alert_launch_image,
        host.user_info,
    ];
    for field in fields {
        release(env, field);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

- (())setFireDate:(id)date { // NSDate *
    replace_field(env, this, date, |h| &mut h.fire_date);
}
- (id)fireDate {
    env.objc.borrow::<UILocalNotificationHostObject>(this).fire_date
}
- (())setTimeZone:(id)time_zone { // NSTimeZone *
    replace_field(env, this, time_zone, |h| &mut h.time_zone);
}
- (id)timeZone {
    env.objc.borrow::<UILocalNotificationHostObject>(this).time_zone
}
- (())setAlertBody:(id)body { // NSString *
    replace_field(env, this, body, |h| &mut h.alert_body);
}
- (id)alertBody {
    env.objc.borrow::<UILocalNotificationHostObject>(this).alert_body
}
- (())setAlertAction:(id)action { // NSString *
    replace_field(env, this, action, |h| &mut h.alert_action);
}
- (id)alertAction {
    env.objc.borrow::<UILocalNotificationHostObject>(this).alert_action
}
- (())setSoundName:(id)name { // NSString *
    replace_field(env, this, name, |h| &mut h.sound_name);
}
- (id)soundName {
    env.objc.borrow::<UILocalNotificationHostObject>(this).sound_name
}
- (())setAlertLaunchImage:(id)name { // NSString *
    replace_field(env, this, name, |h| &mut h.alert_launch_image);
}
- (id)alertLaunchImage {
    env.objc.borrow::<UILocalNotificationHostObject>(this).alert_launch_image
}
- (())setUserInfo:(id)info { // NSDictionary *
    replace_field(env, this, info, |h| &mut h.user_info);
}
- (id)userInfo {
    env.objc.borrow::<UILocalNotificationHostObject>(this).user_info
}
- (())setApplicationIconBadgeNumber:(NSInteger)badge_number {
    env.objc.borrow_mut::<UILocalNotificationHostObject>(this).badge_number = badge_number;
}
- (NSInteger)applicationIconBadgeNumber {
    env.objc.borrow::<UILocalNotificationHostObject>(this).badge_number
}

@end

};
