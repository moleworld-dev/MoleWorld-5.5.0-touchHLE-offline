/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Time things including `CFAbsoluteTime`.

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::CFTypeRef;
use crate::frameworks::foundation::{ns_time_zone, NSTimeInterval};
use crate::libc::time::{guest_wall_clock_now, timestamp_to_calendar_date_i64, MAX_HOST_TIMESTAMP};
use crate::mem::SafeRead;
use crate::objc::{id, msg_class, retain};
use crate::{impl_GuestRet_for_large_struct, Environment};
use std::ops::Add;
use std::time::{Duration, SystemTime};

/// Seconds between Unix and Apple's epochs
pub const SECS_FROM_UNIX_TO_APPLE_EPOCHS: u64 = 978_307_200;

/// The absolute reference date is 1 Jan 2001 00:00:00 GMT
pub fn apple_epoch() -> SystemTime {
    SystemTime::UNIX_EPOCH.add(Duration::from_secs(SECS_FROM_UNIX_TO_APPLE_EPOCHS))
}

pub type CFTimeInterval = NSTimeInterval;
pub type CFAbsoluteTime = CFTimeInterval;

#[derive(Copy, Clone, Debug, PartialEq)]
#[repr(C, packed)]
pub struct CFGregorianDate {
    pub year: i32,    // SInt32
    pub month: i8,    // SInt8
    pub day: i8,      // SInt8
    pub hours: i8,    // SInt8
    pub minutes: i8,  // SInt8
    pub seconds: f64, // double
}
unsafe impl SafeRead for CFGregorianDate {}
impl_GuestRet_for_large_struct!(CFGregorianDate);

/// [扫描修 2026-09-15] 当前 CFAbsoluteTime(虚拟墙钟,含时间旅行偏移)。CFAbsoluteTimeGetCurrent 与
/// NSDate 的"现在"都从这里取,保证两者同一时刻口径(-[Farm innerupdate:] 用 CFAbsoluteTimeGetCurrent,
/// 存档与界面多用 NSDate,口径不一会让作物经过时间错乱)。偏移为 0 时数值与原实现
/// `SystemTime::now().duration_since(apple_epoch())` 一致;宿主时钟早于 2001 时返回负数而不是 panic。
pub fn cf_absolute_time_now() -> CFAbsoluteTime {
    match guest_wall_clock_now().duration_since(apple_epoch()) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// Absolute time is measured in seconds relative to the absolute reference date
/// of Jan 1 2001 00:00:00 GMT.
fn CFAbsoluteTimeGetCurrent(_env: &mut Environment) -> CFAbsoluteTime {
    // [扫描修 2026-09-15] 接入时间旅行偏移(见 cf_absolute_time_now)。
    cf_absolute_time_now()
}

type CFTimeZoneRef = CFTypeRef;

/// [深扫修 2026-09-11] 返回本地(系统)时区,遵守 Copy 规则 +1 引用。
/// 根因:原先恒返回 nil(= GMT),DailySignLayer/ActionCenterControl getServerTime
/// 用它拆日期,签到日界比北京时间晚 8 小时。游戏在 0x39a734/0x3d9ea4 会 CFRelease,
/// 所以必须对缓存的 systemTimeZone 单例先 retain 再返回,否则第二次打开签到页
/// 就把单例释放成野指针。[审查修 2026-09-13] 本地时区默认北京时间(+8),
/// MOLE_TZ=host 跟随宿主,MOLE_TZ=UTC 可一键回退到 GMT 行为。
fn CFTimeZoneCopySystem(env: &mut Environment) -> CFTimeZoneRef {
    let tz: id = msg_class![env; NSTimeZone systemTimeZone];
    retain(env, tz)
}

/// [深扫修 2026-09-11] CFAbsoluteTime(f64,自 2001 起)→ (unix 秒 floor 值, 小数部分)。
/// 根因:原实现走 `Duration::from_secs_f64`,负数(2001 年以前)/NaN/±inf 直接 panic
/// 整个模拟器;再 `as time_t` 按 i32 截断,2038 以后回绕成 1969。现在全程 i64,
/// 先判 is_finite(非有限值按参考日期 2001-01-01 处理),并夹紧到宿主可表示范围。
pub fn cf_absolute_time_to_unix_floor(at: CFAbsoluteTime) -> (i64, f64) {
    if !at.is_finite() {
        return (SECS_FROM_UNIX_TO_APPLE_EPOCHS as i64, 0.0);
    }
    let limit = MAX_HOST_TIMESTAMP as f64;
    let floor = at.floor().clamp(-limit, limit);
    let frac = (at - at.floor()).clamp(0.0, 0.999_999_999);
    (floor as i64 + SECS_FROM_UNIX_TO_APPLE_EPOCHS as i64, frac)
}

/// [深扫修 2026-09-11] 取 CF 时区对象在 `at` 时刻的 UTC 偏移。nil 按 GMT(CF 语义)。
pub fn cf_time_zone_offset_at(env: &mut Environment, tz: CFTimeZoneRef, at: CFAbsoluteTime) -> i32 {
    if tz.is_null() {
        return 0;
    }
    let (unix_secs, _) = cf_absolute_time_to_unix_floor(at);
    ns_time_zone::seconds_from_gmt_at_unix(env, tz, unix_secs)
}

/// [深扫修 2026-09-11] 按给定 UTC 偏移把 CFAbsoluteTime 拆成公历日期,返回 (日期, 星期几 0=周日)。
pub fn gregorian_date_with_offset(at: CFAbsoluteTime, offset: i32) -> (CFGregorianDate, i32) {
    let (unix_secs, frac) = cf_absolute_time_to_unix_floor(at);
    let tm = timestamp_to_calendar_date_i64(unix_secs.saturating_add(offset as i64));
    (
        CFGregorianDate {
            year: 1900 + tm.tm_year,
            month: (tm.tm_mon + 1) as i8,
            day: tm.tm_mday as i8,
            hours: tm.tm_hour as i8,
            minutes: tm.tm_min as i8,
            seconds: f64::from(tm.tm_sec) + frac,
        },
        tm.tm_wday,
    )
}

pub fn CFAbsoluteTimeGetGregorianDate(
    env: &mut Environment,
    at: CFAbsoluteTime,
    tz: CFTimeZoneRef,
) -> CFGregorianDate {
    // [深扫修 2026-09-11] 删掉 assert!(tz.is_null()):CFTimeZoneCopySystem 现在返回真实
    // 时区对象,保留断言会让第一次打开签到页就 panic。tz 非 nil 时按其偏移拆分。
    let offset = cf_time_zone_offset_at(env, tz, at);
    gregorian_date_with_offset(at, offset).0
}

fn CFAbsoluteTimeGetDayOfWeek(env: &mut Environment, at: CFAbsoluteTime, tz: CFTimeZoneRef) -> i32 {
    // [深扫修 2026-09-11] 原实现返回的是 `.day`(几号)而不是星期几。
    // CF 语义:1 = 周一 … 7 = 周日。
    let offset = cf_time_zone_offset_at(env, tz, at);
    let wday = gregorian_date_with_offset(at, offset).1; // 0 = 周日
    if wday == 0 {
        7
    } else {
        wday
    }
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFAbsoluteTimeGetCurrent()),
    export_c_func!(CFTimeZoneCopySystem()),
    export_c_func!(CFAbsoluteTimeGetGregorianDate(_, _)),
    export_c_func!(CFAbsoluteTimeGetDayOfWeek(_, _)),
];

pub const CONSTANTS: ConstantExports = &[(
    // kCFAbsoluteTimeIntervalSince1970 is a CFTimeInterval (double): the number of seconds between the
    // Unix epoch (1 Jan 1970) and CoreFoundation's absolute reference date (1 Jan 2001). Without it the
    // non-lazy symbol pointer stays null, and code that reads it — e.g. the game's
    // -[NetworkManager parseDailyTaskListWithSceneId:pos:len:], which converts a server timestamp via
    // `serverTime - kCFAbsoluteTimeIntervalSince1970` — does `VLDR Dn, [0x0]` → null-page access → crash
    // (observed on entering the village, when the server sends the daily-task list).
    "_kCFAbsoluteTimeIntervalSince1970",
    HostConstant::Custom(|env| {
        env.mem
            .alloc_and_write(SECS_FROM_UNIX_TO_APPLE_EPOCHS as f64)
            .cast()
            .cast_const()
    }),
)];
