/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `time.h` (C) and `sys/time.h` (POSIX)

use crate::dyld::{export_c_func, FunctionExports};
use crate::libc::clocale::{setlocale, LC_CTYPE};
use crate::libc::errno::set_errno;
use crate::libc::stdio::printf::{isspace, isspace_inner};
use crate::mem::{guest_size_of, ConstPtr, GuestUSize, MutPtr, Ptr, SafeRead};
use crate::Environment;
use std::ops::Range;
use std::time::{Duration, Instant, SystemTime};

#[derive(Default)]
pub struct State {
    /// Temporary static storage for the return value of `gmtime` or
    /// `localtime`. The standard allows calls to either to overwrite it.
    gmtime_tmp: Option<MutPtr<tm>>,
}

// time.h (C)

#[allow(non_camel_case_types)]
/// Time in seconds since UNIX epoch (1970-01-01 00:00:00)
pub type time_t = i32;

#[allow(non_camel_case_types)]
type clock_t = u64;

const CLOCKS_PER_SEC: clock_t = 1000000;

fn clock(env: &mut Environment) -> clock_t {
    Instant::now()
        .duration_since(env.startup_time)
        .as_secs()
        .wrapping_mul(CLOCKS_PER_SEC)
}

fn time(env: &mut Environment, out: MutPtr<time_t>) -> time_t {
    // TODO: handle errno properly
    set_errno(env, 0);

    // [扫描修 2026-09-15] 接入时间旅行偏移:改读统一的虚拟墙钟(偏移为 0 时与原先逐位一致)。
    let time64 = guest_wall_clock_now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let time = time64 as time_t;
    if time64 != time as u64 {
        log_once!("Warning: [time] system clock is beyond Y2K38 and might confuse the app");
    }
    if !out.is_null() {
        env.mem.write(out, time);
    }
    time
}

fn tzset(_env: &mut Environment) {
    log!("TODO: tzset()");
}

#[allow(non_camel_case_types)]
#[repr(C, packed)]
#[derive(Copy, Clone, Debug)]
/// `struct tm`, fields count from 0 unless marked otherwise
pub struct tm {
    /// second of the minute
    pub tm_sec: i32,
    /// minute of the hour
    pub tm_min: i32,
    /// hour of the day (24-hour)
    pub tm_hour: i32,
    /// day of the month (**from 1**)
    pub tm_mday: i32,
    /// month of the year
    pub tm_mon: i32,
    /// year with 1900 subtracted from it
    pub tm_year: i32,
    /// day of the week (where Sunday is the first day)
    // [深扫修 2026-09-11] 改 pub:CFAbsoluteTimeGetDayOfWeek / NSCalendar 的
    // weekday 需要读星期几(原先私有,CF 那边只能错拿 day)。
    pub tm_wday: i32,
    /// day of the year
    pub tm_yday: i32,
    /// 1 if daylight saving time is in effect
    tm_isdst: i32,
    /// timezone offset from UTC in seconds
    tm_gmtoff: i32,
    /// abbreviated timezone name (not `const` in C but why not?)
    tm_zone: ConstPtr<u8>,
}
unsafe impl SafeRead for tm {}

impl tm {
    /// A helper function to create a `struct tm` from components.
    /// Right now is used to convert from `DateTime` of `zip` crate.
    ///
    /// Note: Months input is counted from 1.
    pub fn from(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        tm {
            tm_year: (year - 1900).into(),
            tm_mon: (month - 1).into(),
            tm_mday: day.into(),
            tm_hour: hour.into(),
            tm_min: minute.into(),
            tm_sec: second.into(),
            tm_wday: 0,
            tm_yday: 0,
            tm_isdst: 0,
            tm_gmtoff: 0,
            tm_zone: Ptr::null(),
        }
    }
}

// Helpers for timestamp to calendar date conversion, all of these are our own
// original implementation details.
const fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
/// Number of years in a Gregorian calendar cycle (leap year function cycle)
const CYCLE_YEARS: i32 = 400;
/// Lookup table where the index is the number of years since the first year in
/// a Gregorian calendar cycle (400 years), and the value is the number of days
/// between the first day in that year and the first day in the first year.
/// Intended for binary search.
const YEAR_TO_DAY: [i32; CYCLE_YEARS as usize] = calc_year_to_day().0;
/// Number of days in a Gregorian calendar cycle
const CYCLE_DAYS: i32 = calc_year_to_day().1;
const fn calc_year_to_day() -> ([i32; CYCLE_YEARS as usize], i32) {
    let mut table = [0i32; CYCLE_YEARS as usize];
    let mut day = 0;
    let mut year = 0;
    while year < CYCLE_YEARS {
        table[year as usize] = day;
        day += if is_leap_year(year) { 366 } else { 365 };
        year += 1;
    }
    (table, day)
}
/// Lookup table where the index is the number of months since the first month
/// of the year, and the value is the number of days in that month in a non-leap
/// year.
const DAYS_IN_MONTH: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
/// Lookup table where the index is the number of months since the first month
/// in a non-leap year, and the value is the number of days between that month's
/// first day and the first day of the year. Intended for binary search.
const MONTH_TO_DAY_NONLEAP: [i32; 12] = calc_month_to_day(false);
/// [MONTH_TO_DAY_NONLEAP] but for leap years.
const MONTH_TO_DAY_LEAP: [i32; 12] = calc_month_to_day(true);
const fn calc_month_to_day(leap_year: bool) -> [i32; 12] {
    let mut table = [0i32; 12];
    let mut day = 0;
    let mut month = 0;
    while month < 12 {
        table[month] = day;
        day += DAYS_IN_MONTH[month] + ((leap_year && month == 1) as i32);
        month += 1;
    }
    table
}
pub fn timestamp_to_calendar_date(timestamp: time_t) -> tm {
    timestamp_to_calendar_date_i64(timestamp.into())
}

/// [深扫修 2026-09-11] 宿主内部用的 i64 时间戳上下限(约 ±3170 万年)。
/// 根因:原实现只吃 guest ABI 的 `time_t = i32`,CF/NSDate 那边再经
/// `Duration::from_secs_f64` 转换,2001 年以前(负 CF 时间)直接 panic、
/// 2038 年以后被 i32 截断回绕成 1969 年。guest ABI 的 time_t 不能改,
/// 所以只在宿主内部加 i64 版本;夹紧是为了保证年份始终装得进 i32、
/// 后续乘加不溢出(f64 → i64 的极端值会饱和到 i64::MAX)。
pub const MAX_HOST_TIMESTAMP: i64 = 1_000_000_000_000_000;

/// [深扫修 2026-09-11] `timestamp_to_calendar_date` 的 i64 版本(UTC 拆分)。
/// 支持 1970/2001 以前与 2038 以后;超出 ±[MAX_HOST_TIMESTAMP] 时夹紧。
pub fn timestamp_to_calendar_date_i64(timestamp: i64) -> tm {
    let seconds_since_unix_epoch: i64 = timestamp.clamp(-MAX_HOST_TIMESTAMP, MAX_HOST_TIMESTAMP);

    // The easy bit: seconds, minutes, hours and days don't vary in length in
    // UNIX time.

    const MINUTE_SECONDS: i32 = 60;
    const HOUR_SECONDS: i32 = MINUTE_SECONDS * 60;
    const DAY_SECONDS: i32 = HOUR_SECONDS * 24;

    let days_since_unix_epoch: i64 = seconds_since_unix_epoch.div_euclid(DAY_SECONDS as i64);
    let second_in_day: i32 = seconds_since_unix_epoch.rem_euclid(DAY_SECONDS as i64) as i32;

    let tm_sec = second_in_day % MINUTE_SECONDS;
    let tm_min = (second_in_day % HOUR_SECONDS) / MINUTE_SECONDS;
    let tm_hour = second_in_day / HOUR_SECONDS;

    // The hard bit: months and hence years vary in length.

    // UNIX time starts on 1970-01-01. The pattern of leap and non-leap years
    // in the Gregorian calendar resets when the year is a multiple of 400, e.g.
    // the year 2000, so let's adjust the epoch to make things easier.
    let days_since_y2k: i64 = days_since_unix_epoch - 10957;
    let cycles_since_y2k: i64 = days_since_y2k.div_euclid(CYCLE_DAYS as i64);
    let day_in_cycle: i32 = days_since_y2k.rem_euclid(CYCLE_DAYS as i64) as i32;

    let year_in_cycle: i32 = (YEAR_TO_DAY.partition_point(|&day| day <= day_in_cycle) - 1) as _;
    let year: i32 = (2000 + cycles_since_y2k * CYCLE_YEARS as i64 + year_in_cycle as i64) as i32;
    let day_in_year = day_in_cycle - YEAR_TO_DAY[usize::try_from(year_in_cycle).unwrap()];
    let is_leap_year = is_leap_year(year_in_cycle);
    assert!(day_in_year < (365 + is_leap_year as i32));

    let month_to_day = if is_leap_year {
        &MONTH_TO_DAY_LEAP
    } else {
        &MONTH_TO_DAY_NONLEAP
    };
    let month_in_year: i32 = (month_to_day.partition_point(|&day| day <= day_in_year) - 1) as _;
    let day_in_month = day_in_year - month_to_day[usize::try_from(month_in_year).unwrap()];
    assert!(day_in_month < DAYS_IN_MONTH[month_in_year as usize] + is_leap_year as i32);

    // 0 = Sunday, 1970-01-01 was a Thursday
    let day_of_the_week = (4 + days_since_unix_epoch).rem_euclid(7) as i32;

    tm {
        tm_sec,
        tm_min,
        tm_hour,
        tm_mday: day_in_month + 1,
        tm_mon: month_in_year,
        tm_year: year - 1900,
        tm_wday: day_of_the_week,
        tm_yday: day_in_year,
        // This function always returns UTC
        tm_isdst: 0,
        tm_gmtoff: 0,
        // TODO: this probably shouldn't be NULL?
        tm_zone: Ptr::null(),
    }
}
#[cfg(test)]
#[test]
fn test_timestamp_to_calendar_date() {
    fn do_test(expected: &str, timestamp: time_t) {
        let tm {
            tm_year,
            tm_mon,
            tm_mday,
            tm_hour,
            tm_min,
            tm_sec,
            tm_wday,
            ..
        } = timestamp_to_calendar_date(timestamp);
        let wday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][tm_wday as usize];
        assert_eq!(
            expected,
            &format!(
                "{}, {:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                wday,
                tm_year + 1900,
                tm_mon + 1,
                tm_mday,
                tm_hour,
                tm_min,
                tm_sec
            )
        );
    }
    // Random tests generated with this JavaScript:
    //
    //   for (i = 0; i < 12; i++) {
    //     let timestamp = (Math.random() * 2 ** 32) | 0;
    //     console.log(
    //       "do_test(\"" +
    //       (new Date(timestamp * 1000)).toUTCString().substr(0, 5) +
    //       (new Date(timestamp * 1000)).toISOString().substr(0, 19) +
    //       "\", " + timestamp + ");"
    //     );
    //   }
    do_test("Mon, 2006-02-20T01:27:52", 1140398872);
    do_test("Tue, 2036-12-16T06:40:54", 2113022454);
    do_test("Thu, 1922-03-02T06:22:31", -1509557849);
    do_test("Wed, 1990-07-25T13:02:43", 648910963);
    do_test("Wed, 1912-12-18T20:42:53", -1799896627);
    do_test("Wed, 1990-03-28T04:47:24", 638599644);
    do_test("Thu, 2034-03-30T18:44:51", 2027357091);
    do_test("Sun, 2022-01-09T21:41:51", 1641764511);
    do_test("Fri, 2018-04-13T17:03:50", 1523639030);
    do_test("Thu, 1973-08-30T10:11:33", 115553493);
    do_test("Fri, 2005-05-27T19:45:47", 1117223147);
    do_test("Sat, 1955-03-26T20:47:45", -466053135);
}

pub fn calendar_date_to_timestamp(tm: tm) -> time_t {
    calendar_date_to_timestamp_i64(tm).try_into().unwrap()
}

/// [深扫修 2026-09-11] 公历日期 → 自 1970-01-01 起的天数(i64,支持任意正负年份)。
/// `month` 从 1 数起、必须在 1..=12;`day` 从 1 数起(越界的日子由调用方线性相加)。
/// 算法为 Howard Hinnant 的 days_from_civil(按 400 年周期取欧几里得除法)。
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400); // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// [深扫修 2026-09-11] `calendar_date_to_timestamp` 的 i64 版本(按 UTC 解释字段)。
/// 根因:原实现逐年循环 + `try_into().unwrap()`,2038 以后/过早的年份直接 panic,
/// 且 tm_mon 越界会数组越界 panic;mktime 语义要求能规范化越界字段。
/// 这里月份按 12 进位规范化,日/时/分/秒线性相加(天然规范化)。
pub fn calendar_date_to_timestamp_i64(tm: tm) -> i64 {
    civil_to_timestamp_i64(
        tm.tm_year as i64 + 1900,
        tm.tm_mon as i64,
        tm.tm_mday as i64,
        tm.tm_hour as i64,
        tm.tm_min as i64,
        tm.tm_sec as i64,
    )
}

/// [深扫修 2026-09-11] 公历字段 → 秒数(按 UTC 解释)。`year` 为真实年份,
/// `month0` 从 0 数起(按 12 进位规范化),`mday` 从 1 数起;日/时/分/秒可越界,线性相加。
/// 各参数在 i32 范围内时不会溢出 i64。供 NSCalendar/NSDateFormatter 的逆向换算使用
/// (`tm` 有私有字段,外部模块无法直接构造)。
pub fn civil_to_timestamp_i64(
    year: i64,
    month0: i64,
    mday: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> i64 {
    let year = year + month0.div_euclid(12);
    let month = month0.rem_euclid(12) + 1;
    let days = days_from_civil(year, month, 1) + (mday - 1);
    days * 86400 + hour * 3600 + minute * 60 + second
}

/// [深扫修 2026-09-11] 某年某月(month 从 1 数起)的天数。
pub fn days_in_month(year: i64, month: i64) -> i64 {
    let (ny, nm) = if month >= 12 { (year + 1, 1) } else { (year, month + 1) };
    days_from_civil(ny, nm, 1) - days_from_civil(year, month, 1)
}

// ---------------------------------------------------------------------------
// [深扫修 2026-09-11] 本地时区([审查修 2026-09-13] 默认北京时间)
//
// 根因:touchHLE 原先整条时区栈都当 UTC —— CFTimeZoneCopySystem 返回 nil、
// NSTimeZone 固定 "GMT"、localtime 直接转 gmtime。真机上游戏(以及 SDK)
// 用设备本地时区拆日期;中国玩家在 touchHLE 里看到的钟点整整慢 8 小时
// (昼夜系统、签到日界、留言时间全错)。
//
// 修法:统一从这里取"本地时区在某个时刻的 UTC 偏移",所有 guest 可见的
// 本地时间(NSTimeZone/CFTimeZone/NSCalendar/NSDateFormatter/libc localtime)
// 都走它,保证全栈一致。偏移来源由环境变量 MOLE_TZ 决定(见 `mole_tz_override`):
// - 未设置或空白(默认):[审查修 2026-09-13] 固定北京时间 Asia/Shanghai(+8,无夏令时)。
//   原先默认跟随宿主;宿主不在 +8(虚拟机/Docker/Android 模拟器常默认 UTC、海外宿主)时
//   签到日界、活动小时、昼夜整体错开,而游戏自身与原版服务器都按北京时间跨日。
// - "host"/"local"/"system"(不区分大小写):跟随宿主本地时区(真机"按设备时区"语义):
//   - unix(macOS/iOS/Linux/Android):libc::localtime_r 的 tm_gmtoff(按具体日期,含夏令时)。
//   - Windows:libc::localtime_s 拆出本地字段,与 UTC 秒数相减得偏移。
//   - 其它平台或查询失败:回退北京时间 +8([审查修 2026-09-13] 原先回退 UTC)。
// - 其它值为固定偏移(无夏令时):
//   "UTC"/"GMT"/"Z" → 0;"+8"、"+0800"、"+08:00"、"UTC+8"、"GMT-05:30" 等 → 对应偏移;
//   以及少量常见 IANA 名(Asia/Shanghai 等)。MOLE_TZ=UTC 即可一键回退最早的 UTC 行为。
//   无法识别的值回退默认北京时间并写日志([审查修 2026-09-13] 不再回退宿主)。
// ---------------------------------------------------------------------------

/// 少量常见 IANA 时区名 → 固定偏移(仅用于 MOLE_TZ 与 NSTimeZone
/// timeZoneWithName:,不做夏令时)。
const KNOWN_FIXED_ZONES: &[(&str, i32)] = &[
    ("Asia/Shanghai", 28800),
    ("Asia/Chongqing", 28800),
    ("Asia/Chungking", 28800),
    ("Asia/Harbin", 28800),
    ("Asia/Urumqi", 28800),
    ("PRC", 28800),
    ("Asia/Hong_Kong", 28800),
    ("Hongkong", 28800),
    ("Asia/Macau", 28800),
    ("Asia/Taipei", 28800),
    ("Asia/Singapore", 28800),
    ("Asia/Kuala_Lumpur", 28800),
    ("Asia/Manila", 28800),
    ("Asia/Tokyo", 32400),
    ("Asia/Seoul", 32400),
    ("Etc/UTC", 0),
    ("Etc/GMT", 0),
    ("Europe/London", 0),
];

/// 解析时区名/偏移串为 UTC 偏移秒数。无法识别时返回 None。
/// 支持:"UTC"/"GMT"/"Z";"+8"、"+08"、"+0800"、"+08:00"、"-0530";
/// 前缀 "UTC"/"GMT" 加上述偏移;以及 [KNOWN_FIXED_ZONES] 中的名字。
pub fn parse_utc_offset_name(name: &str) -> Option<i32> {
    let name = name.trim();
    if let Some(&(_, off)) = KNOWN_FIXED_ZONES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
    {
        return Some(off);
    }
    let upper = name.to_ascii_uppercase();
    let rest = if let Some(r) = upper.strip_prefix("UTC") {
        r
    } else if let Some(r) = upper.strip_prefix("GMT") {
        r
    } else {
        upper.as_str()
    };
    if rest.is_empty() || rest == "Z" {
        return if upper.starts_with("UTC") || upper.starts_with("GMT") || rest == "Z" {
            Some(0)
        } else {
            None
        };
    }
    let (sign, digits) = match rest.as_bytes()[0] {
        b'+' => (1, &rest[1..]),
        b'-' => (-1, &rest[1..]),
        _ => return None,
    };
    if !digits.is_ascii() {
        return None; // 防止下面按字节切片落在多字节字符中间而 panic
    }
    let (hours, minutes): (i32, i32) = if let Some((h, m)) = digits.split_once(':') {
        (h.parse().ok()?, m.parse().ok()?)
    } else if digits.len() <= 2 {
        (digits.parse().ok()?, 0)
    } else if digits.len() == 4 {
        (digits[..2].parse().ok()?, digits[2..].parse().ok()?)
    } else {
        return None;
    };
    if !(0..=18).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

/// 把偏移格式化成 "GMT+0800" 这种形式(真机上无 IANA 名时的写法)。
pub fn format_gmt_offset_name(offset: i32) -> String {
    if offset == 0 {
        return "GMT".to_string();
    }
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.unsigned_abs();
    format!("GMT{}{:02}{:02}", sign, abs / 3600, (abs % 3600) / 60)
}

/// [审查修 2026-09-13] 默认本地时区:北京时间(固定 +8,无夏令时;上海没有夏令时,固定偏移无损)。
const DEFAULT_TZ_OFFSET: i32 = 28800;
/// [审查修 2026-09-13] 默认本地时区名。含 '/',NSTimeZone initWithName: 用它判 Local。
const DEFAULT_TZ_NAME: &str = "Asia/Shanghai";

/// MOLE_TZ 覆盖:Some((偏移, 名字)) 表示固定偏移;None 表示跟随宿主。
///
/// [审查修 2026-09-13] 默认值从"跟随宿主"改为北京时间:
/// - 未设置 / 空白 → Some((28800, "Asia/Shanghai"));
/// - "host" / "local" / "system"(不区分大小写)→ None,跟随宿主;
/// - 其它值照旧解析;无法识别(含非 UTF-8)→ 回退默认北京时间并写日志,不再回退宿主。
///
/// 根因:原先 MOLE_TZ 未设置时 `std::env::var(..).ok()?` 直接返回 None = 跟随宿主,
/// 而所有启动器/默认选项都没设 MOLE_TZ。宿主不在 +8 时(虚拟机、Docker、默认 UTC 的
/// Android 模拟器、海外宿主),DailySignLayer/ActionCenterControl getServerTime 拆出的
/// 签到日界、活动小时和 CommonEffectController 昼夜全都错开;开发机在 +8,实测发现不了。
/// 取舍:游戏自身按北京时间算日界 —— 火焰大战倒计时硬编码北京零点 (now+28800)/86400,
/// 二进制里还有十余处 +0x7080(28800),如 0x3c2ebe 对 [NewSceneTimer getCurrentServerTime]
/// 加 28800;原版服务器也按北京时间跨日。所以默认固定 +8 与游戏/服务器一致;需要真机
/// "按设备时区"语义时显式设 MOLE_TZ=host。
fn mole_tz_override() -> Option<(i32, String)> {
    static OVERRIDE: std::sync::OnceLock<Option<(i32, String)>> = std::sync::OnceLock::new();
    OVERRIDE
        .get_or_init(|| {
            let default = Some((DEFAULT_TZ_OFFSET, DEFAULT_TZ_NAME.to_string()));
            let raw = match std::env::var("MOLE_TZ") {
                Ok(raw) => raw,
                Err(std::env::VarError::NotPresent) => {
                    log!(
                        "MOLE_TZ not set: using default time zone {} (UTC+8, set MOLE_TZ=host to follow host)",
                        DEFAULT_TZ_NAME
                    );
                    return default;
                }
                Err(std::env::VarError::NotUnicode(_)) => {
                    log!(
                        "Warning: MOLE_TZ is not valid UTF-8, falling back to default time zone {} (UTC+8)",
                        DEFAULT_TZ_NAME
                    );
                    return default;
                }
            };
            let raw = raw.trim().to_string();
            if raw.is_empty() {
                log!(
                    "MOLE_TZ is empty: using default time zone {} (UTC+8, set MOLE_TZ=host to follow host)",
                    DEFAULT_TZ_NAME
                );
                return default;
            }
            if matches!(raw.to_ascii_lowercase().as_str(), "host" | "local" | "system") {
                log!("MOLE_TZ={:?}: following host time zone", raw);
                return None;
            }
            match parse_utc_offset_name(&raw) {
                Some(off) => {
                    // 带 '/' 的是 IANA 名,原样作为名字;其余统一成 GMT±hhmm。
                    let name = if raw.contains('/') {
                        raw
                    } else {
                        format_gmt_offset_name(off)
                    };
                    log!("MOLE_TZ={:?}: using fixed time zone offset {}s", name, off);
                    Some((off, name))
                }
                None => {
                    log!(
                        "Warning: MOLE_TZ={:?} not understood, falling back to default time zone {} (UTC+8)",
                        raw,
                        DEFAULT_TZ_NAME
                    );
                    default
                }
            }
        })
        .clone()
}

/// 直接问宿主 C 库:unix 秒 `unix_secs` 这一刻本地时区相对 UTC 的偏移。
#[cfg(unix)]
fn host_libc_utc_offset_at(unix_secs: i64) -> Option<i32> {
    // 32 位宿主(如 armv7 Android)time_t 可能是 i32,超范围就放弃,交给调用方回退。
    let t: ::libc::time_t = unix_secs.try_into().ok()?;
    // SAFETY: tm 是纯 POD(含一个裸指针字段),全 0 是合法值;localtime_r 线程安全。
    let mut out: ::libc::tm = unsafe { std::mem::zeroed() };
    let res = unsafe { ::libc::localtime_r(&t, &mut out) };
    if res.is_null() {
        return None;
    }
    Some(out.tm_gmtoff as i32)
}
#[cfg(windows)]
fn host_libc_utc_offset_at(unix_secs: i64) -> Option<i32> {
    // Windows 的 tm 没有 tm_gmtoff:拆出本地字段后按 UTC 重新合成秒数,差值即偏移。
    // localtime_s 对负时间戳返回 EINVAL,交给调用方回退到"当前偏移"。
    let t: ::libc::time_t = unix_secs.try_into().ok()?;
    let mut out: ::libc::tm = unsafe { std::mem::zeroed() };
    let err = unsafe { ::libc::localtime_s(&mut out, &t) };
    if err != 0 {
        return None;
    }
    let local = days_from_civil(
        out.tm_year as i64 + 1900,
        out.tm_mon as i64 + 1,
        out.tm_mday as i64,
    ) * 86400
        + out.tm_hour as i64 * 3600
        + out.tm_min as i64 * 60
        + out.tm_sec as i64;
    i32::try_from(local - unix_secs).ok()
}
#[cfg(not(any(unix, windows)))]
fn host_libc_utc_offset_at(_unix_secs: i64) -> Option<i32> {
    None
}

/// [扫描修 2026-09-15] 开发者「时间旅行」偏移(秒,只增不减)。guest 能看到的所有墙钟时间源都要加上它;
/// mach_absolute_time / Instant 这类单调时钟不受影响。在线模式由调用方拒绝设置。
static TIME_OFFSET_SECS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// 当前时间旅行偏移(秒)。
pub fn time_offset_secs() -> i64 {
    TIME_OFFSET_SECS.load(std::sync::atomic::Ordering::Relaxed)
}

/// 增加时间旅行偏移(只接受正数,不允许往回拨)。
pub fn add_time_offset_secs(delta: i64) {
    if delta > 0 {
        TIME_OFFSET_SECS.fetch_add(delta, std::sync::atomic::Ordering::Relaxed);
        // [扫描修 2026-09-15] 记一次"系统时间跳变",主线程 run loop 据此给 app delegate 补发
        // applicationSignificantTimeChange:(见 TIME_JUMP_GENERATION 的说明)。
        TIME_JUMP_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// [扫描修 2026-09-15] 时间旅行跳变代数:偏移每真正变化一次就 +1,初值 0(从未跳变)。
///
/// 根因:本游戏 cocos2d 的帧 dt 不是单调时钟,而是 gettimeofday —— -[CCDirector calculateDeltaTime]
/// @0x2c7d74 在 0x2c7d82 调 _gettimeofday,dt = MAX(0, now − lastUpdate_),release 版没有 0.2 秒上限。
/// 墙钟一跳 N 小时,下一帧 dt 就是 N×3600 秒,所有 update:/动作/粒子吃到一个巨大 dt。
/// 真机上用户改系统时间时,iOS 会给 app delegate 发 applicationSignificantTimeChange:,而本游戏的
/// -[iMoleVillageAppDelegate applicationSignificantTimeChange:]@0x11a80 正是
/// [[CCDirector sharedDirector] setNextDeltaTimeZero:YES]。所以 NSRunLoop 主循环比较这个代数,
/// 变化时补发该回调(忠实复现真机行为),下一帧 dt 归零、不跳帧。
static TIME_JUMP_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [扫描修 2026-09-15] 当前时间旅行跳变代数(见 TIME_JUMP_GENERATION)。
pub fn time_jump_generation() -> u64 {
    TIME_JUMP_GENERATION.load(std::sync::atomic::Ordering::Relaxed)
}

/// [扫描修 2026-09-15] guest 可见的「虚拟墙钟」= 宿主 `SystemTime::now()` + 时间旅行偏移。
///
/// 根因:作物成熟(-[Farm innerupdate:] 读 CFAbsoluteTimeGetCurrent)、冷却、签到日界、黄金岛计时都读墙钟,
/// 而 time()/gettimeofday/ftime/CFAbsoluteTimeGetCurrent/NSDate/runUntilDate:/pthread_cond_timedwait
/// 原先各自直接调 `SystemTime::now()`,开发工具的时间偏移无处生效;只改其中几处又会让 NSDate 与
/// CFAbsoluteTime 相差 N 小时,游戏算出的经过时间错乱。所以 guest 可见的墙钟一律从这里取。
///
/// 取舍:
/// - 偏移为 0 时直接返回 `SystemTime::now()`,与改动前逐位一致;
/// - 单调时钟(mach_absolute_time、CACurrentMediaTime、systemUptime、clock()、NSTimer/
///   performSelector:afterDelay:/CADisplayLink 的 `Instant` 截止时间)绝不加偏移,
///   偏移跳变不会让已排队的计时器全部立即触发或永不触发;
/// - `host_now_unix_secs` 保持宿主真实时间不变(名字即语义,别的模块可能按宿主时间使用它);
/// - 偏移按设计只增不减,这里仍按有符号处理,溢出时退回宿主时间,不 panic。
pub fn guest_wall_clock_now() -> SystemTime {
    let now = SystemTime::now();
    let offset = time_offset_secs();
    if offset == 0 {
        return now;
    }
    let shift = Duration::from_secs(offset.unsigned_abs());
    let shifted = if offset > 0 {
        now.checked_add(shift)
    } else {
        now.checked_sub(shift)
    };
    shifted.unwrap_or(now)
}

/// [扫描修 2026-09-15] 虚拟墙钟的 unix 秒(i64,与 host_now_unix_secs 同口径;早于 1970 返回负数不 panic)。
pub fn guest_now_unix_secs() -> i64 {
    match guest_wall_clock_now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// [扫描修 2026-09-15] 虚拟墙钟的 unix 秒(f64,含小数;早于 1970 返回负数不 panic)。
pub fn guest_now_unix_secs_f64() -> f64 {
    match guest_wall_clock_now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs_f64(),
        Err(e) => -e.duration().as_secs_f64(),
    }
}

/// 宿主当前 unix 秒(宿主时钟早于 1970 也不 panic)。
pub fn host_now_unix_secs() -> i64 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

/// 本地时区在 unix 秒 `unix_secs` 时刻的 UTC 偏移(秒)。MOLE_TZ 决定来源:默认北京时间 +8,
/// MOLE_TZ=host 时跟随宿主。跟随宿主时,宿主查不到该时刻(超出 time_t 范围等)
/// 就退回当前偏移,再不行按北京时间 +8。
pub fn local_utc_offset_at(unix_secs: i64) -> i32 {
    if let Some((off, _)) = mole_tz_override() {
        return off;
    }
    // [审查修 2026-09-13] 两次宿主查询都失败时原先 unwrap_or(0) 退回 UTC,与"默认北京时间"
    // 的约定不一致,改为退回 +8。实际只在非 unix/windows 宿主或 time_t 越界等极端情况走到。
    host_libc_utc_offset_at(unix_secs)
        .or_else(|| host_libc_utc_offset_at(host_now_unix_secs()))
        .unwrap_or(DEFAULT_TZ_OFFSET)
}

/// 已知"本地墙钟秒数"(按 UTC 合成的字段秒数)时,求对应的 UTC 偏移。
/// 用于 mktime / dateFromComponents: / dateFromString: 的逆向换算:先按
/// 墙钟估一次偏移,再用修正后的时刻复查一次(夏令时切换附近也能收敛)。
pub fn local_utc_offset_for_wall_clock(local_secs: i64) -> i32 {
    let guess = local_utc_offset_at(local_secs);
    local_utc_offset_at(local_secs - guess as i64)
}

/// 本地时区名。MOLE_TZ 覆盖优先([审查修 2026-09-13] 未设置时默认 "Asia/Shanghai");
/// MOLE_TZ=host 跟随宿主时,unix 读 TZ 环境变量或 /etc/localtime 符号链接里 "zoneinfo/"
/// 之后的 IANA 名;都拿不到时用 "GMT+0800" 形式。
pub fn local_time_zone_name() -> String {
    if let Some((_, name)) = mole_tz_override() {
        return name;
    }
    static HOST_NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let host_name = HOST_NAME.get_or_init(host_time_zone_name_uncached);
    match host_name {
        Some(name) => name.clone(),
        None => format_gmt_offset_name(local_utc_offset_at(host_now_unix_secs())),
    }
}

#[cfg(unix)]
fn host_time_zone_name_uncached() -> Option<String> {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':').trim();
        if tz.contains('/') && !tz.starts_with('/') {
            return Some(tz.to_string());
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy().into_owned();
        if let Some(idx) = target.find("zoneinfo/") {
            let name = &target[idx + "zoneinfo/".len()..];
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}
#[cfg(not(unix))]
fn host_time_zone_name_uncached() -> Option<String> {
    None
}

#[cfg(test)]
#[test]
fn test_i64_calendar_helpers() {
    // 2038 以后与 1970 以前都要能往返
    for &ts in &[
        -62_135_596_800i64, // 0001-01-01
        -978_307_200,
        -86_400,
        0,
        978_307_200,
        4_294_880_896,
        253_402_300_799, // 9999-12-31T23:59:59
    ] {
        let t = timestamp_to_calendar_date_i64(ts);
        assert_eq!(calendar_date_to_timestamp_i64(t), ts);
    }
    let t = timestamp_to_calendar_date_i64(4_294_880_896);
    assert_eq!((t.tm_year + 1900, t.tm_mon + 1, t.tm_mday), (2106, 2, 6));
    assert_eq!(parse_utc_offset_name("+08:00"), Some(28800));
    assert_eq!(parse_utc_offset_name("GMT-0530"), Some(-19800));
    assert_eq!(parse_utc_offset_name("UTC"), Some(0));
    assert_eq!(parse_utc_offset_name("Asia/Shanghai"), Some(28800));
    assert_eq!(parse_utc_offset_name("Mars/Base"), None);
}

#[cfg(test)]
#[test]
fn test_calendar_date_to_timestamp() {
    fn do_roundtrip_test(timestamp: time_t) {
        let tm_struct = timestamp_to_calendar_date(timestamp);
        let roundtripped = calendar_date_to_timestamp(tm_struct);
        assert_eq!(
            roundtripped, timestamp,
            "Roundtrip failed: original={timestamp}, after converting to tm and back={roundtripped}"
        );
    }

    let test_timestamps = [
        1140398872,  // Mon, 2006-02-20T01:27:52
        2113022454,  // Tue, 2036-12-16T06:40:54
        -1509557849, // Thu, 1922-03-02T06:22:31
        648910963,   // Wed, 1990-07-25T13:02:43
        -1799896627, // Wed, 1912-12-18T20:42:53
        638599644,   // Wed, 1990-03-28T04:47:24
        2027357091,  // Thu, 2034-03-30T18:44:51
        1641764511,  // Sun, 2022-01-09T21:41:51
        1523639030,  // Fri, 2018-04-13T17:03:50
        115553493,   // Thu, 1973-08-30T10:11:33
        1117223147,  // Fri, 2005-05-27T19:45:47
        -466053135,  // Sat, 1955-03-26T20:47:45
    ];

    for &timestamp in &test_timestamps {
        do_roundtrip_test(timestamp);
    }
}

#[cfg(test)]
#[test]
fn test_calendar_date_to_timestamp_known_dates() {
    // 1970-01-01T00:00:00 UTC
    let tm_epoch = tm {
        tm_year: 1970 - 1900,
        tm_mon: 0, // January
        tm_mday: 1,
        tm_hour: 0,
        tm_min: 0,
        tm_sec: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: Ptr::null(),
    };
    assert_eq!(calendar_date_to_timestamp(tm_epoch), 0);

    // 1970-01-02T00:00:00 UTC
    let tm_next_day = tm {
        tm_year: 1970 - 1900,
        tm_mon: 0, // January
        tm_mday: 2,
        tm_hour: 0,
        tm_min: 0,
        tm_sec: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: Ptr::null(),
    };
    assert_eq!(calendar_date_to_timestamp(tm_next_day), 86400);

    // 1972-03-01T00:00:00 UTC (a leap year)
    let tm_leap = tm {
        tm_year: 1972 - 1900,
        tm_mon: 2, // March
        tm_mday: 1,
        tm_hour: 0,
        tm_min: 0,
        tm_sec: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: Ptr::null(),
    };
    assert_eq!(calendar_date_to_timestamp(tm_leap), 68256000);

    // 1955-03-26T20:47:45
    let tm_before_epoch = tm {
        tm_year: 1955 - 1900,
        tm_mon: 2, // March
        tm_mday: 26,
        tm_hour: 20,
        tm_min: 47,
        tm_sec: 45,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
        tm_gmtoff: 0,
        tm_zone: Ptr::null(),
    };
    assert_eq!(calendar_date_to_timestamp(tm_before_epoch), -466053135);
}

fn gmtime_r(env: &mut Environment, timestamp: ConstPtr<time_t>, res: MutPtr<tm>) -> MutPtr<tm> {
    let timestamp = env.mem.read(timestamp);
    let calendar_date = timestamp_to_calendar_date(timestamp);
    env.mem.write(res, calendar_date);
    res
}
fn gmtime(env: &mut Environment, timestamp: ConstPtr<time_t>) -> MutPtr<tm> {
    let tmp = *env
        .libc_state
        .time
        .gmtime_tmp
        .get_or_insert_with(|| env.mem.alloc(guest_size_of::<tm>()).cast());
    gmtime_r(env, timestamp, tmp)
}

/// [深扫修 2026-09-11] 按本地时区拆分 unix 秒(宿主内部 i64)。
/// 根因:localtime/localtime_r 原先直接转调 gmtime,本地时间恒为 UTC。
/// 现在加上 [local_utc_offset_at] 的偏移,并填好 tm_gmtoff;tm_zone 仍为 NULL
/// (strftime 的 %Z 依赖这一点,按 tm_gmtoff 输出名字)。
fn local_calendar_date_i64(timestamp: i64) -> tm {
    let offset = local_utc_offset_at(timestamp);
    let mut res = timestamp_to_calendar_date_i64(timestamp.saturating_add(offset as i64));
    res.tm_gmtoff = offset;
    res
}

fn localtime_r(env: &mut Environment, timestamp: ConstPtr<time_t>, res: MutPtr<tm>) -> MutPtr<tm> {
    // [深扫修 2026-09-11] 不再假设本地时间 = UTC(见 local_calendar_date_i64)。
    let timestamp = env.mem.read(timestamp);
    let calendar_date = local_calendar_date_i64(timestamp.into());
    env.mem.write(res, calendar_date);
    res
}
fn localtime(env: &mut Environment, timestamp: ConstPtr<time_t>) -> MutPtr<tm> {
    // This doesn't have to be a unique temporary, gmtime and localtime are
    // allowed to share it.
    let tmp = *env
        .libc_state
        .time
        .gmtime_tmp
        .get_or_insert_with(|| env.mem.alloc(guest_size_of::<tm>()).cast());
    localtime_r(env, timestamp, tmp)
}

fn mktime(env: &mut Environment, tm: MutPtr<tm>) -> time_t {
    // [深扫修 2026-09-11] mktime 的字段是本地墙钟时间:先按 UTC 合成(i64,自动规范化
    // 越界字段),再减去该墙钟对应的本地偏移。按 C 标准把规范化后的字段(含
    // tm_wday/tm_yday/tm_gmtoff)写回;结果超出 guest time_t(i32)时返回 -1。
    let tm_value = env.mem.read(tm);
    let local_secs = calendar_date_to_timestamp_i64(tm_value);
    let offset = local_utc_offset_for_wall_clock(local_secs);
    let utc_secs = local_secs - offset as i64;
    let res: time_t = match utc_secs.try_into() {
        Ok(res) => {
            env.mem.write(tm, local_calendar_date_i64(utc_secs));
            res
        }
        Err(_) => -1,
    };
    log_dbg!("mktime({:?}) => {}", tm_value, res);
    res
}

// sys/time.h (POSIX)

#[allow(non_camel_case_types)]
type suseconds_t = i32;

#[allow(non_camel_case_types)]
#[derive(Debug)]
#[repr(C, packed)]
pub(super) struct timeval {
    pub(super) tv_sec: time_t,
    pub(super) tv_usec: suseconds_t,
}
unsafe impl SafeRead for timeval {}

#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, Default)]
#[repr(C, packed)]
pub struct timespec {
    pub tv_sec: time_t,
    pub tv_nsec: i32,
}
unsafe impl SafeRead for timespec {}

#[allow(non_camel_case_types)]
#[repr(C, packed)]
struct timezone {
    tz_minuteswest: i32,
    tz_dsttime: i32,
}
unsafe impl SafeRead for timezone {}

fn gettimeofday(
    env: &mut Environment,
    timeval_ptr: MutPtr<timeval>,
    timezone_ptr: MutPtr<timezone>,
) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    if !timezone_ptr.is_null() {
        // [深扫修 2026-09-11] 与 localtime 一致地报告本地时区(分钟,西为正)。
        // [扫描修 2026-09-15] 按虚拟墙钟的"现在"查偏移,与下面返回的 tv_sec 同一时刻口径。
        let offset = local_utc_offset_at(guest_now_unix_secs());
        env.mem.write(
            timezone_ptr,
            timezone {
                tz_minuteswest: -offset / 60,
                tz_dsttime: 0,
            },
        );
    }

    if timeval_ptr.is_null() {
        return 0; // success
    }

    // [扫描修 2026-09-15] 接入时间旅行偏移。注意本游戏 cocos2d 的帧 dt 就是用它算的
    // (-[CCDirector calculateDeltaTime]@0x2c7d74,无上限),跳变造成的一帧巨大 dt 由 NSRunLoop 主循环
    // 补发 applicationSignificantTimeChange:(→ setNextDeltaTimeZero:YES)消掉,见 TIME_JUMP_GENERATION。
    let time = guest_wall_clock_now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();

    let time_s_64: u64 = time.as_secs();
    let tv_sec = time_s_64 as time_t;
    if time_s_64 != tv_sec as u64 {
        log_once!("Warning: [gettimeofday] system clock is beyond Y2K38 and might confuse the app");
    }
    let tv_usec: suseconds_t = time.subsec_micros().try_into().unwrap();

    env.mem.write(timeval_ptr, timeval { tv_sec, tv_usec });

    0 // success
}

fn nanosleep(env: &mut Environment, rqtp: ConstPtr<timespec>, _rmtp: MutPtr<timespec>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let t = env.mem.read(rqtp);
    let tv_sec = t.tv_sec;
    let tv_nsec = t.tv_nsec;
    log_dbg!("nanosleep {} {}", tv_sec, tv_nsec);
    let total_sleep = Duration::from_secs(tv_sec.try_into().unwrap())
        + Duration::from_nanos(tv_nsec.try_into().unwrap());
    env.sleep(total_sleep);
    0 // success
}

fn strptime(
    env: &mut Environment,
    buffer: ConstPtr<u8>,
    format: ConstPtr<u8>,
    time_ptr: MutPtr<tm>,
) -> MutPtr<u8> {
    log_dbg!(
        "strptime({:?}, {:?})",
        env.mem.cstr_at_utf8(buffer),
        env.mem.cstr_at_utf8(format)
    );

    let mut time_val = env.mem.read(time_ptr);

    let mut conversation_failed = false;
    let mut buffer_char_idx = 0;
    let mut format_char_idx = 0;
    loop {
        let c = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        if c == b'\0' {
            break;
        }
        if c != b'%' {
            let mut cc = env.mem.read(buffer + buffer_char_idx);
            if isspace(env, format + format_char_idx - 1) {
                // "All ordinary characters are matched exactly with the buffer
                // , where white space in the format string will match any
                // amount of white space in the buffer."
                while isspace_inner(cc) {
                    buffer_char_idx += 1;
                    cc = env.mem.read(buffer + buffer_char_idx);
                }
                continue;
            }
            if c != cc {
                conversation_failed = true;
                break;
            }
            buffer_char_idx += 1;
            continue;
        }

        let specifier = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        let mut parse_2_digits = |range: Range<i32>| -> Result<i32, ()> {
            let mut num: i32 = 0;
            let mut chars_count = 0;
            while let c @ b'0'..=b'9' = env.mem.read(buffer + buffer_char_idx) {
                if chars_count >= 2 {
                    break;
                }
                num = num * 10 + (c - b'0') as i32;
                buffer_char_idx += 1;
                chars_count += 1;
            }
            if chars_count != 2 {
                Err(())
            } else {
                assert!(range.contains(&num));
                Ok(num)
            }
        };

        match specifier {
            b'H' => match parse_2_digits(0..24) {
                Ok(hour) => {
                    time_val.tm_hour = hour;
                }
                Err(_) => {
                    conversation_failed = true;
                    break;
                }
            },
            b'M' => match parse_2_digits(0..60) {
                Ok(minute) => {
                    time_val.tm_min = minute;
                }
                Err(_) => {
                    conversation_failed = true;
                    break;
                }
            },
            b'S' => match parse_2_digits(0..61) {
                Ok(second) => {
                    time_val.tm_sec = second;
                }
                Err(_) => {
                    conversation_failed = true;
                    break;
                }
            },
            _ => unimplemented!(
                "Format character '{}'. Formatted up to index {}",
                specifier as char,
                format_char_idx
            ),
        }
    }

    env.mem.write(time_ptr, time_val);

    if conversation_failed {
        Ptr::null()
    } else {
        (buffer + buffer_char_idx).cast_mut()
    }
}

fn strftime(
    env: &mut Environment,
    s: MutPtr<u8>,
    max_size: GuestUSize,
    format: ConstPtr<u8>,
    time_ptr: ConstPtr<tm>,
) -> GuestUSize {
    log_dbg!(
        "strftime({:?}, {}, {:?}, {:?})",
        s,
        max_size,
        env.mem.cstr_at_utf8(format),
        time_ptr
    );

    // TODO: support other locales
    let ctype_locale = setlocale(env, LC_CTYPE, Ptr::null());
    assert_eq!(env.mem.read(ctype_locale), b'C');

    let time_val = env.mem.read(time_ptr);

    let mut res = Vec::<u8>::new();

    let mut format_char_idx = 0;
    loop {
        let c = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        if c == b'\0' {
            break;
        }
        if c != b'%' {
            res.push(c);
            continue;
        }

        let specifier = env.mem.read(format + format_char_idx);
        format_char_idx += 1;

        match specifier {
            b'm' => {
                let month = time_val.tm_mon + 1;
                assert!((1..=12).contains(&month));
                let formatted_month = format!("{:02}", month);
                res.extend_from_slice(formatted_month.as_bytes());
            }
            b'd' => {
                let day = time_val.tm_mday; // from 1
                assert!((1..=31).contains(&day));
                let formatted_day = format!("{:02}", day);
                res.extend_from_slice(formatted_day.as_bytes());
            }
            b'H' => {
                let hour = time_val.tm_hour;
                assert!((0..24).contains(&hour));
                let formatted_hour = format!("{:02}", hour);
                res.extend_from_slice(formatted_hour.as_bytes());
            }
            b'M' => {
                let minute = time_val.tm_min;
                assert!((0..60).contains(&minute));
                let formatted_minute = format!("{:02}", minute);
                res.extend_from_slice(formatted_minute.as_bytes());
            }
            b'I' => {
                let hour12 = time_val.tm_hour % 12;
                let hour = if hour12 == 0 { 12 } else { hour12 };
                assert!((1..=12).contains(&hour));
                let formatted_hour = format!("{:02}", hour);
                res.extend_from_slice(formatted_hour.as_bytes());
            }
            b'p' => {
                let hour = time_val.tm_hour;
                let formatted = if hour < 12 { "AM" } else { "PM" };
                res.extend_from_slice(formatted.as_bytes());
            }
            b'a' => {
                let wday = time_val.tm_wday;
                assert!((0..7).contains(&wday));
                let wday_str = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][wday as usize];
                res.extend_from_slice(wday_str.as_bytes());
            }
            b'b' => {
                let mon = time_val.tm_mon;
                assert!((0..12).contains(&mon));
                let mon_str = [
                    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov",
                    "Dec",
                ][mon as usize];
                res.extend_from_slice(mon_str.as_bytes());
            }
            b'Y' => {
                let year = time_val.tm_year + 1900;
                assert!((0..=9999).contains(&year)); // TODO
                let formatted_year = format!("{}", year);
                res.extend_from_slice(formatted_year.as_bytes());
            }
            b'S' => {
                let seconds = time_val.tm_sec;
                assert!((0..=60).contains(&seconds));
                let formatted_seconds = format!("{:02}", seconds);
                res.extend_from_slice(formatted_seconds.as_bytes());
            }
            b'Z' => {
                assert!(time_val.tm_zone.is_null()); // TODO

                // [深扫修 2026-09-11] localtime 现在会填 tm_gmtoff,按它输出
                // "GMT" 或 "GMT+0800",避免本地时间配上 "GMT" 字样。
                let gmtoff = time_val.tm_gmtoff;
                res.extend_from_slice(format_gmt_offset_name(gmtoff).as_bytes());
            }
            _ => unimplemented!(
                "Format character '{}'. Formatted up to index {}",
                specifier as char,
                format_char_idx
            ),
        }
    }

    let middle = if ((max_size - 1) as usize) < res.len() {
        &res[..(max_size - 1) as usize]
    } else {
        &res[..]
    };

    let dest_slice = env.mem.bytes_at_mut(s, max_size);
    for (i, &byte) in middle.iter().chain(b"\0".iter()).enumerate() {
        dest_slice[i] = byte;
    }

    res.len().try_into().unwrap()
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(clock()),
    export_c_func!(time(_)),
    export_c_func!(tzset()),
    export_c_func!(gmtime_r(_, _)),
    export_c_func!(gmtime(_)),
    export_c_func!(mktime(_)),
    export_c_func!(localtime_r(_, _)),
    export_c_func!(localtime(_)),
    export_c_func!(gettimeofday(_, _)),
    export_c_func!(nanosleep(_, _)),
    export_c_func!(strptime(_, _, _)),
    export_c_func!(strftime(_, _, _, _)),
];
