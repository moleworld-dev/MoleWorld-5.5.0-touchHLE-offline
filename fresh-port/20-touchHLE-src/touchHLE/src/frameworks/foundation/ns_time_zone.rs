/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSTimeZone`.
//!
//! [深扫修 2026-09-11] 原实现 systemTimeZone/localTimeZone 固定 "GMT"、
//! secondsFromGMT 恒 0,整个模拟环境都当 UTC,中国玩家看到的钟点慢 8 小时。
//! 现在区分两种时区:
//! - `Local`:本地时区,systemTimeZone/localTimeZone/defaultTimeZone 返回它。
//!   [审查修 2026-09-13] 默认固定北京时间 Asia/Shanghai(+8);MOLE_TZ=host 时跟随宿主
//!   (按具体日期查偏移,含夏令时);MOLE_TZ 设成其它值时为对应的固定偏移;
//! - `Fixed(秒)`:固定偏移,timeZoneWithName:@"GMT"、timeZoneForSecondsFromGMT: 等返回它
//!   (SDK 显式设 GMT 时语义保持正确)。
//! 偏移来源统一在 `libc::time::local_utc_offset_at`,与 CFTimeZone/NSCalendar/
//! NSDateFormatter/libc localtime 共用,保证全栈一致。

use crate::frameworks::core_foundation::time::cf_absolute_time_to_unix_floor;
use crate::frameworks::foundation::{ns_string, NSInteger, NSTimeInterval};
use crate::libc::time::{
    format_gmt_offset_name, host_now_unix_secs, local_time_zone_name, local_utc_offset_at,
    parse_utc_offset_name,
};
use crate::objc::{
    autorelease, id, nil, release, retain, Class, ClassExports, HostObject, NSZonePtr,
};
use crate::Environment;
use crate::{msg, objc_classes};

#[derive(Default)]
pub struct State {
    system_time_zone: Option<id>,
}

#[derive(Copy, Clone, Debug)]
enum TimeZoneKind {
    /// 本地时区(默认北京时间;MOLE_TZ=host 时跟随宿主,偏移随日期变化)
    Local,
    /// 固定 UTC 偏移(秒)
    Fixed(i32),
}

struct NSTimeZoneHostObject {
    // NSString*
    time_zone: id,
    kind: TimeZoneKind,
}
impl HostObject for NSTimeZoneHostObject {}

fn kind_offset_at(kind: TimeZoneKind, unix_secs: i64) -> i32 {
    match kind {
        TimeZoneKind::Local => local_utc_offset_at(unix_secs),
        TimeZoneKind::Fixed(offset) => offset,
    }
}

/// 读取时区对象的类型;不是我们的 NSTimeZone 宿主对象时返回 None。
fn time_zone_kind(env: &Environment, tz: id) -> Option<TimeZoneKind> {
    if tz == nil {
        return None;
    }
    env.objc
        .get_host_object(tz)
        .and_then(|obj| obj.as_any().downcast_ref::<NSTimeZoneHostObject>())
        .map(|obj| obj.kind)
}

/// [深扫修 2026-09-11] 时区对象 `tz`(NSTimeZone*/CFTimeZoneRef)在 unix 秒
/// `unix_secs` 时刻相对 GMT 的偏移(秒)。nil 按 GMT(CF 语义)。
/// 供 CFAbsoluteTimeGetGregorianDate、NSDateFormatter 使用。
pub fn seconds_from_gmt_at_unix(env: &mut Environment, tz: id, unix_secs: i64) -> i32 {
    if tz == nil {
        return 0;
    }
    match time_zone_kind(env, tz) {
        Some(kind) => kind_offset_at(kind, unix_secs),
        None => {
            // 不是宿主实现的时区对象(理论上不会发生),退回问它自己。
            let seconds: NSInteger = msg![env; tz secondsFromGMT];
            seconds
        }
    }
}

/// [深扫修 2026-09-11] 已知 `tz` 时区下的"墙钟秒数"(字段按 UTC 合成)时求偏移,
/// 用于 dateFromString: 之类的逆向换算。先估一次,再按修正后的时刻复查一次。
pub fn seconds_from_gmt_for_wall_clock(env: &mut Environment, tz: id, local_secs: i64) -> i32 {
    let guess = seconds_from_gmt_at_unix(env, tz, local_secs);
    seconds_from_gmt_at_unix(env, tz, local_secs - guess as i64)
}

/// 分配一个时区对象。`name` 必须是调用方已持有的 +1 引用,所有权转交给对象。
fn alloc_time_zone(env: &mut Environment, class: Class, name: id, kind: TimeZoneKind) -> id {
    let host_object = Box::new(NSTimeZoneHostObject {
        time_zone: name,
        kind,
    });
    env.objc.alloc_object(class, host_object, &mut env.mem)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSTimeZone: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSTimeZoneHostObject {
        time_zone: nil,
        kind: TimeZoneKind::Fixed(0),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)timeZoneWithName:(id)tz_name {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithName:tz_name];
    autorelease(env, new)
}

// [深扫修 2026-09-11] ADCUtil / YMLUtilToolkit 用 timeZoneWithAbbreviation:@"GMT"。
// 与 timeZoneWithName: 共用解析("GMT"/"UTC"/"GMT+0800" 等)。
+ (id)timeZoneWithAbbreviation:(id)abbreviation {
    msg![env; this timeZoneWithName:abbreviation]
}

+ (id)timeZoneForSecondsFromGMT:(NSInteger)seconds {
    let name = format_gmt_offset_name(seconds);
    let name = ns_string::from_rust_string(env, name);
    let new = alloc_time_zone(env, this, name, TimeZoneKind::Fixed(seconds));
    autorelease(env, new)
}

+ (id)localTimeZone {
    // According to docs, `localTimeZone` is not cached in contrast to
    // `systemTimeZone`
    // [深扫修 2026-09-11] 返回本地时区(原先固定 "GMT";[审查修 2026-09-13] 默认北京时间)。
    let name = local_time_zone_name();
    let name = ns_string::from_rust_string(env, name);
    let new = alloc_time_zone(env, this, name, TimeZoneKind::Local);
    autorelease(env, new)
}

+ (id)systemTimeZone {
    if let Some(system_time_zone) = env.framework_state.foundation.ns_time_zone.system_time_zone {
        system_time_zone
    } else {
        // [深扫修 2026-09-11] 本地时区(原先固定 "GMT";[审查修 2026-09-13] 默认北京时间)。单例常驻,不 autorelease;
        // CFTimeZoneCopySystem 会在返回前额外 retain(Copy 规则)。
        let name = local_time_zone_name();
        let name = ns_string::from_rust_string(env, name);
        let new = alloc_time_zone(env, this, name, TimeZoneKind::Local);
        env.framework_state.foundation.ns_time_zone.system_time_zone = Some(new);
        new
    }
}

+ (id)defaultTimeZone {
    // TODO: implement setting a default time zone
    msg![env; this systemTimeZone]
}

- (())dealloc {
    let tz_name = env.objc.borrow_mut::<NSTimeZoneHostObject>(this).time_zone;
    release(env, tz_name);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (id)initWithName:(id)tz_name { // NSString *
    // [深扫修 2026-09-11] 原先 assert_ne!(tz_name, nil) 会让宿主 panic;真机对 nil/未知名字
    // 返回 nil。这里 nil 返回 nil;未知名字为兼容旧行为仍返回对象(按 GMT)。
    if tz_name == nil {
        release(env, this);
        return nil;
    }
    let name_str = ns_string::to_rust_string(env, tz_name).to_string();
    // [审查修 2026-09-13] 只有含 '/' 的 IANA 名才与本地时区名比较并判 Local。
    // 根因:跟随宿主(MOLE_TZ=host)且宿主拿不到 IANA 名(Windows、部分 Linux/Android)时,
    // 本地名是按"当前时刻偏移"现算的 "GMT"/"GMT+0100";冬季零偏移的夏令时地区里,SDK 显式要的
    // "GMT"(MAUtils/YMLUtilToolkit)会被判成跟随宿主夏令时的 Local,结果随运行季节变化,
    // 而真机 GMT 恒为固定 0 偏移。
    // 取舍:不含 '/' 的名字一律走 parse_utc_offset_name 得 Fixed;不能反过来先调它,否则
    // KNOWN_FIXED_ZONES 里的 "Europe/London" 这类宿主 IANA 名会被判 Fixed 而丢夏令时。
    // 默认名 "Asia/Shanghai" 含 '/',仍判 Local(偏移即默认 +8);MOLE_TZ=UTC/+8 合成的
    // "GMT"/"GMT+0800" 判 Fixed,偏移与覆盖值相同,无差异。代价:不含 '/' 的旧式宿主时区名
    // (GB、Eire 等)在跟随宿主模式下失去夏令时。
    let kind = if name_str.contains('/') && name_str == local_time_zone_name() {
        TimeZoneKind::Local
    } else if let Some(offset) = parse_utc_offset_name(&name_str) {
        TimeZoneKind::Fixed(offset)
    } else {
        log!("Warning: NSTimeZone initWithName:{:?} unknown, treating as GMT", name_str);
        TimeZoneKind::Fixed(0)
    };
    retain(env, tz_name);
    let host_object = env.objc.borrow_mut::<NSTimeZoneHostObject>(this);
    host_object.time_zone = tz_name;
    host_object.kind = kind;
    this
}

// NSCopying implementation (NSTimeZone is immutable)
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

- (id)name {
    env.objc.borrow_mut::<NSTimeZoneHostObject>(this).time_zone
}

- (NSInteger)secondsFromGMT {
    // [深扫修 2026-09-11] 按当前时刻返回真实偏移(原先恒 0)。
    let kind = env.objc.borrow::<NSTimeZoneHostObject>(this).kind;
    kind_offset_at(kind, host_now_unix_secs())
}

// [深扫修 2026-09-11] immobUtils getCurrentDate 使用;按给定日期算偏移(夏令时正确)。
- (NSInteger)secondsFromGMTForDate:(id)date { // NSDate *
    let unix_secs = if date == nil {
        host_now_unix_secs()
    } else {
        let ti: NSTimeInterval = msg![env; date timeIntervalSinceReferenceDate];
        cf_absolute_time_to_unix_floor(ti).0
    };
    let kind = env.objc.borrow::<NSTimeZoneHostObject>(this).kind;
    kind_offset_at(kind, unix_secs)
}

@end

};
