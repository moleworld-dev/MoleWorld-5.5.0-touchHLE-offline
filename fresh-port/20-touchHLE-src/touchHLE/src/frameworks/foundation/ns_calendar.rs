/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSCalendar` and `NSDateComponents`.
//!
//! [深扫修 2026-09-11] 新增。根因:touchHLE 完全没有 NSCalendar/NSDateComponents,
//! `[NSCalendar currentCalendar]` 走 unimplemented-class 分支返回 nil,之后
//! components:fromDate: / hour / minute / second 全是发给 nil 的消息,恒为 0。
//! 摩尔庄园昼夜系统 -[CommonEffectController init/resetSecondsInToday/checkIsNightComing]
//! (0x3212ac/0x322440/0x32268c)据此算出 hour=0,`hour-7 < 12` 按无符号比较不成立 →
//! 恒判夜晚,且计时从 00:00:00 起跑,要连续 7 小时才天亮。
//!
//! 只实现二进制里真正出现的选择子(selref + xref 核实):
//! - 游戏:currentCalendar、components:fromDate:(unit 0xe0)、hour/minute/second;
//! - SDK:initWithCalendarIdentifier:(TMPush/Facebook/immobViewZipArchive,传的是
//!   _NSGregorianCalendar 常量)、dateFromComponents:(immob Date1980、IMAdRequest)、
//!   components:fromDate:toDate:options:(Facebook shouldExtendAccessToken)、
//!   NSDateComponents 的 year/month/day/hour/minute/second 与 setter;weekday 按要求一并提供。
//! dateByAddingComponents:toDate:options: 等在二进制中无 selref,不实现。
//!
//! 一律按公历、按本地时区(`libc::time::local_utc_offset_at`)计算。
//! [审查修 2026-09-13] 本地时区默认固定北京时间(+8),MOLE_TZ=host 时跟随宿主,
//! MOLE_TZ 设成其它值时为对应的固定偏移。

use super::ns_string;
use super::{NSInteger, NSTimeInterval, NSUInteger};
use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_foundation::time::{
    cf_absolute_time_to_unix_floor, SECS_FROM_UNIX_TO_APPLE_EPOCHS,
};
use crate::libc::time::{
    civil_to_timestamp_i64, days_in_month, local_utc_offset_at, local_utc_offset_for_wall_clock,
    timestamp_to_calendar_date_i64, tm,
};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};
use crate::Environment;

/// iOS SDK 中 `NSGregorianCalendar` 的值就是 @"gregorian"。
const NSGregorianCalendar: &str = "gregorian";

pub const CONSTANTS: ConstantExports = &[(
    // [深扫修 2026-09-11] 原先日志报 unhandled non-lazy symbol "_NSGregorianCalendar"
    // at 0x9c839c,SDK 读到 nil 传给 initWithCalendarIdentifier:。
    "_NSGregorianCalendar",
    HostConstant::NSString(NSGregorianCalendar),
)];

// NSCalendarUnit 位
const NSEraCalendarUnit: NSUInteger = 1 << 1;
const NSYearCalendarUnit: NSUInteger = 1 << 2;
const NSMonthCalendarUnit: NSUInteger = 1 << 3;
const NSDayCalendarUnit: NSUInteger = 1 << 4;
const NSHourCalendarUnit: NSUInteger = 1 << 5;
const NSMinuteCalendarUnit: NSUInteger = 1 << 6;
const NSSecondCalendarUnit: NSUInteger = 1 << 7;
const NSWeekdayCalendarUnit: NSUInteger = 1 << 9;

/// 未请求/未设置的分量(= NSIntegerMax),与原版一致。
pub const NSUndefinedDateComponent: NSInteger = 0x7fffffff;

struct NSCalendarHostObject {
    /// `NSString *`, retained
    identifier: id,
}
impl HostObject for NSCalendarHostObject {}

#[derive(Copy, Clone)]
struct NSDateComponentsHostObject {
    era: NSInteger,
    year: NSInteger,
    month: NSInteger,
    day: NSInteger,
    hour: NSInteger,
    minute: NSInteger,
    second: NSInteger,
    weekday: NSInteger,
}
impl HostObject for NSDateComponentsHostObject {}
impl Default for NSDateComponentsHostObject {
    fn default() -> Self {
        NSDateComponentsHostObject {
            era: NSUndefinedDateComponent,
            year: NSUndefinedDateComponent,
            month: NSUndefinedDateComponent,
            day: NSUndefinedDateComponent,
            hour: NSUndefinedDateComponent,
            minute: NSUndefinedDateComponent,
            second: NSUndefinedDateComponent,
            weekday: NSUndefinedDateComponent,
        }
    }
}

/// NSDate → (unix 秒 floor, 小数秒, 本地时区拆出的字段)
fn local_fields_for_date(env: &mut Environment, date: id) -> (i64, f64, tm) {
    let ti: NSTimeInterval = msg![env; date timeIntervalSinceReferenceDate];
    let (unix_secs, frac) = cf_absolute_time_to_unix_floor(ti);
    let offset = local_utc_offset_at(unix_secs);
    let fields = timestamp_to_calendar_date_i64(unix_secs.saturating_add(offset as i64));
    (unix_secs, frac, fields)
}

/// 本地墙钟字段(year 为真实年份,month 从 1 数起)→ unix 秒
fn local_wall_clock_to_unix(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> i64 {
    let local_secs = civil_to_timestamp_i64(year, month - 1, day, hour, minute, second);
    local_secs - local_utc_offset_for_wall_clock(local_secs) as i64
}

fn date_from_unix(env: &mut Environment, unix_secs: i64, frac: f64) -> id {
    let ti: NSTimeInterval = (unix_secs - SECS_FROM_UNIX_TO_APPLE_EPOCHS as i64) as f64 + frac;
    msg_class![env; NSDate dateWithTimeIntervalSinceReferenceDate:ti]
}

/// 在本地字段 `start` 上加 `months` 个月(日子夹到目标月末,时分秒保持),返回 unix 秒。
fn add_months_local(start: &tm, months: i64) -> i64 {
    let month0 = start.tm_mon as i64 + months;
    let year = start.tm_year as i64 + 1900 + month0.div_euclid(12);
    let month = month0.rem_euclid(12) + 1;
    let day = (start.tm_mday as i64).min(days_in_month(year, month));
    local_wall_clock_to_unix(
        year,
        month,
        day,
        start.tm_hour as i64,
        start.tm_min as i64,
        start.tm_sec as i64,
    )
}

fn new_components(env: &mut Environment, fields: NSDateComponentsHostObject) -> id {
    let comps: id = msg_class![env; NSDateComponents alloc];
    let comps: id = msg![env; comps init];
    *env.objc.borrow_mut::<NSDateComponentsHostObject>(comps) = fields;
    autorelease(env, comps)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSCalendar: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSCalendarHostObject {
        identifier: nil,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)currentCalendar {
    // 真机上每次返回一个新的 autoreleased 日历,这里同样不缓存(也就无需全局状态)。
    let identifier = ns_string::get_static_str(env, NSGregorianCalendar);
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCalendarIdentifier:identifier];
    autorelease(env, new)
}

- (id)initWithCalendarIdentifier:(id)identifier { // NSString *
    // _NSGregorianCalendar 现已导出;但为防旧存档/其它路径传 nil,nil 一律当公历容忍。
    let identifier = if identifier == nil {
        log_dbg!("[NSCalendar initWithCalendarIdentifier:nil] treating as gregorian");
        ns_string::get_static_str(env, NSGregorianCalendar)
    } else {
        let name = ns_string::to_rust_string(env, identifier);
        if name != NSGregorianCalendar {
            log!("Warning: NSCalendar identifier {:?} not supported, using gregorian", name);
        }
        identifier
    };
    retain(env, identifier);
    env.objc.borrow_mut::<NSCalendarHostObject>(this).identifier = identifier;
    this
}

- (())dealloc {
    let identifier = env.objc.borrow::<NSCalendarHostObject>(this).identifier;
    release(env, identifier);
    env.objc.dealloc_object(this, &mut env.mem)
}

// NSCopying implementation(本实现无可变状态)
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

- (id)calendarIdentifier {
    env.objc.borrow::<NSCalendarHostObject>(this).identifier
}

- (id)components:(NSUInteger)unit_flags
        fromDate:(id)date { // NSDate *
    if date == nil {
        return nil;
    }
    let (_, _, fields) = local_fields_for_date(env, date);
    let mut comps = NSDateComponentsHostObject::default();
    if unit_flags & NSEraCalendarUnit != 0 {
        comps.era = if fields.tm_year + 1900 > 0 { 1 } else { 0 };
    }
    if unit_flags & NSYearCalendarUnit != 0 {
        comps.year = fields.tm_year + 1900;
    }
    if unit_flags & NSMonthCalendarUnit != 0 {
        comps.month = fields.tm_mon + 1;
    }
    if unit_flags & NSDayCalendarUnit != 0 {
        comps.day = fields.tm_mday;
    }
    if unit_flags & NSHourCalendarUnit != 0 {
        comps.hour = fields.tm_hour;
    }
    if unit_flags & NSMinuteCalendarUnit != 0 {
        comps.minute = fields.tm_min;
    }
    if unit_flags & NSSecondCalendarUnit != 0 {
        comps.second = fields.tm_sec;
    }
    if unit_flags & NSWeekdayCalendarUnit != 0 {
        // NSCalendar:1 = 周日 … 7 = 周六;tm_wday:0 = 周日
        comps.weekday = fields.tm_wday + 1;
    }
    log_dbg!(
        "[(NSCalendar*){:?} components:{:#x} fromDate:{:?}] => {}-{}-{} {}:{}:{}",
        this, unit_flags, date, comps.year, comps.month, comps.day, comps.hour, comps.minute, comps.second
    );
    new_components(env, comps)
}

- (id)dateFromComponents:(id)components { // NSDateComponents *
    if components == nil {
        return nil;
    }
    let comps = *env.objc.borrow::<NSDateComponentsHostObject>(components);
    // 未设置的分量取原版默认:年 1、月 1、日 1、时分秒 0
    let or_default = |v: NSInteger, d: i64| -> i64 {
        if v == NSUndefinedDateComponent { d } else { v as i64 }
    };
    let unix_secs = local_wall_clock_to_unix(
        or_default(comps.year, 1),
        or_default(comps.month, 1),
        or_default(comps.day, 1),
        or_default(comps.hour, 0),
        or_default(comps.minute, 0),
        or_default(comps.second, 0),
    );
    date_from_unix(env, unix_secs, 0.0)
}

// Facebook shouldExtendAccessToken 用 unit 0x20(hour) 算两个日期相差几小时。
- (id)components:(NSUInteger)unit_flags
        fromDate:(id)start_date // NSDate *
          toDate:(id)end_date // NSDate *
         options:(NSUInteger)_options {
    if start_date == nil || end_date == nil {
        return nil;
    }
    let start_ti: NSTimeInterval = msg![env; start_date timeIntervalSinceReferenceDate];
    let end_ti: NSTimeInterval = msg![env; end_date timeIntervalSinceReferenceDate];
    // 统一按 start <= end 计算,最后再整体取负
    let (negative, from_date, to_date) = if end_ti < start_ti {
        (true, end_date, start_date)
    } else {
        (false, start_date, end_date)
    };
    let (from_unix, from_frac, from_fields) = local_fields_for_date(env, from_date);
    let (to_unix, to_frac, to_fields) = local_fields_for_date(env, to_date);

    let mut comps = NSDateComponentsHostObject::default();
    // cursor:已被年/月吃掉之后的起点(unix 秒)
    let mut cursor_unix = from_unix;
    if unit_flags & (NSYearCalendarUnit | NSMonthCalendarUnit) != 0 {
        let mut months = (to_fields.tm_year as i64 - from_fields.tm_year as i64) * 12
            + (to_fields.tm_mon as i64 - from_fields.tm_mon as i64);
        // 加满 months 个月后超过终点,就少算一个月
        loop {
            if months <= 0 {
                break;
            }
            let cand = add_months_local(&from_fields, months);
            if cand > to_unix || (cand == to_unix && from_frac > to_frac) {
                months -= 1;
            } else {
                break;
            }
        }
        let months = months.max(0);
        let years = if unit_flags & NSYearCalendarUnit != 0 { months / 12 } else { 0 };
        let rest_months = if unit_flags & NSMonthCalendarUnit != 0 { months - years * 12 } else { 0 };
        if unit_flags & NSYearCalendarUnit != 0 {
            comps.year = years as NSInteger;
        }
        if unit_flags & NSMonthCalendarUnit != 0 {
            comps.month = rest_months as NSInteger;
        }
        cursor_unix = add_months_local(&from_fields, years * 12 + rest_months);
    }
    // 剩余的整秒数(不足 1 秒的小数截掉)
    let mut remaining: i64 = (to_unix - cursor_unix) + ((to_frac - from_frac).floor() as i64);
    remaining = remaining.max(0);
    let mut take = |unit: NSUInteger, secs: i64, slot: &mut NSInteger| {
        if unit_flags & unit != 0 {
            let n = remaining / secs;
            remaining -= n * secs;
            *slot = n.clamp(i32::MIN as i64 + 1, i32::MAX as i64 - 1) as NSInteger;
        }
    };
    take(NSDayCalendarUnit, 86400, &mut comps.day);
    take(NSHourCalendarUnit, 3600, &mut comps.hour);
    take(NSMinuteCalendarUnit, 60, &mut comps.minute);
    take(NSSecondCalendarUnit, 1, &mut comps.second);
    if negative {
        for slot in [
            &mut comps.year,
            &mut comps.month,
            &mut comps.day,
            &mut comps.hour,
            &mut comps.minute,
            &mut comps.second,
        ] {
            if *slot != NSUndefinedDateComponent {
                *slot = -*slot;
            }
        }
    }
    new_components(env, comps)
}

@end

@implementation NSDateComponents: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<NSDateComponentsHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (NSInteger)era {
    env.objc.borrow::<NSDateComponentsHostObject>(this).era
}
- (NSInteger)year {
    env.objc.borrow::<NSDateComponentsHostObject>(this).year
}
- (NSInteger)month {
    env.objc.borrow::<NSDateComponentsHostObject>(this).month
}
- (NSInteger)day {
    env.objc.borrow::<NSDateComponentsHostObject>(this).day
}
- (NSInteger)hour {
    env.objc.borrow::<NSDateComponentsHostObject>(this).hour
}
- (NSInteger)minute {
    env.objc.borrow::<NSDateComponentsHostObject>(this).minute
}
- (NSInteger)second {
    env.objc.borrow::<NSDateComponentsHostObject>(this).second
}
- (NSInteger)weekday {
    env.objc.borrow::<NSDateComponentsHostObject>(this).weekday
}

- (())setEra:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).era = value;
}
- (())setYear:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).year = value;
}
- (())setMonth:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).month = value;
}
- (())setDay:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).day = value;
}
- (())setHour:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).hour = value;
}
- (())setMinute:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).minute = value;
}
- (())setSecond:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).second = value;
}
- (())setWeekday:(NSInteger)value {
    env.objc.borrow_mut::<NSDateComponentsHostObject>(this).weekday = value;
}

@end

};
