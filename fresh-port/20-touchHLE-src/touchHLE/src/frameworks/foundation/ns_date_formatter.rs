/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSDateFormatter`.
//!
//! Resources:
//! - Apple's [Introduction to Data Formatting Programming Guide For Cocoa](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/DataFormatting/DataFormatting.html)
//! - [Unicode Technical Standard #35](https://unicode.org/reports/tr35/tr35-10.html#Date_Format_Patterns)
//!
//! [深扫修 2026-09-11] 原实现没有时区概念:stringFromDate: 直接按 UTC 拆字段,
//! setTimeZone: / dateFromString: 未实现(发过去是 no-op/nil)。于是:
//! - 游戏的好友留言板(-[MessageViewController configureCell:forIndexPath:] 0x1a7f54
//!   "MM/dd/yyyy hh:mm:ss")、环游世界/免费贝壳面板("YYYY-MM-dd HH:mm:ss")的时间比
//!   北京时间慢 8 小时;
//! - -[GuessWorldCupMainLayer secondsFromNowToFutureDate:] 用 "yyyy-MM-dd HH:mm:ss"
//!   做 dateFromString: 得 nil,倒计时算不出来;
//! - SDK(MAUtils/YMLUtilToolkit 等)显式设 GMT 的格式化被忽略。
//! 修法:宿主对象增加 time_zone 字段(None = 本地时区,与真机默认一致),
//! setTimeZone: 真正生效;stringFromDate: 与 dateFromString: 共用同一套
//! 模式解析与同一个时区字段,保证往返不漂移。

use crate::frameworks::core_foundation::time::{
    cf_absolute_time_to_unix_floor, SECS_FROM_UNIX_TO_APPLE_EPOCHS,
};
use crate::frameworks::foundation::{ns_string, ns_time_zone, NSTimeInterval};
use crate::libc::time::{
    civil_to_timestamp_i64, days_in_month, format_gmt_offset_name, local_utc_offset_at,
    local_utc_offset_for_wall_clock, timestamp_to_calendar_date_i64,
};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};

struct NSDateFormatterHostObject {
    date_format: Option<id>,
    /// [深扫修 2026-09-11] `NSTimeZone *`(retained)。None 表示用本地时区。
    time_zone: Option<id>,
}
impl HostObject for NSDateFormatterHostObject {}

// 用 static(而非 const),保证 `.iter().map(|s| &s[..3])` 借出的是 'static 引用。
static MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
static WEEKDAY_NAMES: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

/// 日期模式里的一个单元:字母字段(字母, 重复次数)或字面文本。
enum FormatToken {
    Field(char, usize),
    Literal(String),
}

/// 按 UTS #35 切分模式:连续相同字母为一个字段,单引号内为字面文本('' 表示单引号本身)。
fn tokenize_date_format(format: &str) -> Vec<FormatToken> {
    let chars: Vec<char> = format.chars().collect();
    let mut tokens = Vec::new();
    let mut literal = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            if i + 1 < chars.len() && chars[i + 1] == '\'' {
                literal.push('\'');
                i += 2;
                continue;
            }
            i += 1;
            while i < chars.len() {
                if chars[i] == '\'' {
                    if i + 1 < chars.len() && chars[i + 1] == '\'' {
                        literal.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                literal.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if c.is_ascii_alphabetic() {
            let mut n = 1;
            while i + n < chars.len() && chars[i + n] == c {
                n += 1;
            }
            if !literal.is_empty() {
                tokens.push(FormatToken::Literal(std::mem::take(&mut literal)));
            }
            tokens.push(FormatToken::Field(c, n));
            i += n;
            continue;
        }
        literal.push(c);
        i += 1;
    }
    if !literal.is_empty() {
        tokens.push(FormatToken::Literal(literal));
    }
    tokens
}

fn pad_number(value: i64, width: usize) -> String {
    if value < 0 {
        format!("-{:0width$}", -value, width = width)
    } else {
        format!("{:0width$}", value, width = width)
    }
}

/// "+0800" / "+08:00" 形式的偏移
fn format_offset_digits(offset: i32, colon: bool) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    let abs = offset.unsigned_abs();
    if colon {
        format!("{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
    } else {
        format!("{}{:02}{:02}", sign, abs / 3600, (abs % 3600) / 60)
    }
}

/// 按模式把"本地字段"格式化成字符串。未支持的字母原样输出(纯外观,不崩)。
fn format_date(tokens: &[FormatToken], unix_secs: i64, frac: f64, offset: i32) -> String {
    let t = timestamp_to_calendar_date_i64(unix_secs.saturating_add(offset as i64));
    let year = t.tm_year as i64 + 1900;
    let month = t.tm_mon as i64 + 1;
    let day = t.tm_mday as i64;
    let hour = t.tm_hour as i64;
    let minute = t.tm_min as i64;
    let second = t.tm_sec as i64;
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };

    let mut out = String::new();
    for token in tokens {
        match *token {
            FormatToken::Literal(ref s) => out.push_str(s),
            FormatToken::Field(c, n) => match c {
                // 真机 "YYYY" 是按周计算的年份;这些界面上差异可忽略,按日历年处理。
                'y' | 'Y' | 'u' => {
                    if n == 2 {
                        out.push_str(&pad_number(year.rem_euclid(100), 2));
                    } else {
                        out.push_str(&pad_number(year, n));
                    }
                }
                'M' | 'L' => match n {
                    1 | 2 => out.push_str(&pad_number(month, n)),
                    3 => out.push_str(&MONTH_NAMES[(month - 1) as usize][..3]),
                    _ => out.push_str(MONTH_NAMES[(month - 1) as usize]),
                },
                'd' => out.push_str(&pad_number(day, n.min(2))),
                'D' => out.push_str(&pad_number(t.tm_yday as i64 + 1, n.min(3))),
                'H' => out.push_str(&pad_number(hour, n.min(2))),
                'k' => out.push_str(&pad_number(if hour == 0 { 24 } else { hour }, n.min(2))),
                'h' => out.push_str(&pad_number(hour12, n.min(2))),
                'K' => out.push_str(&pad_number(hour % 12, n.min(2))),
                'm' => out.push_str(&pad_number(minute, n.min(2))),
                's' => out.push_str(&pad_number(second, n.min(2))),
                'S' => {
                    let digits = n.min(9);
                    let scaled = (frac * 10f64.powi(digits as i32)).floor() as i64;
                    out.push_str(&pad_number(scaled, digits));
                }
                // touchHLE 未实现 setAMSymbol:/setPMSymbol:(no-op),用 en_US 默认值。
                'a' => out.push_str(if hour < 12 { "AM" } else { "PM" }),
                'E' => {
                    let name = WEEKDAY_NAMES[t.tm_wday as usize];
                    if n <= 3 {
                        out.push_str(&name[..3]);
                    } else {
                        out.push_str(name);
                    }
                }
                'G' => out.push_str("AD"),
                'Z' => match n {
                    1..=3 => out.push_str(&format_offset_digits(offset, false)),
                    4 => {
                        out.push_str("GMT");
                        if offset != 0 {
                            out.push_str(&format_offset_digits(offset, true));
                        }
                    }
                    _ => {
                        if offset == 0 {
                            out.push('Z');
                        } else {
                            out.push_str(&format_offset_digits(offset, true));
                        }
                    }
                },
                'z' => out.push_str(&format_gmt_offset_name(offset)),
                // 未支持的模式字母:以字面字母呈现(沿用旧实现"不崩"的策略)
                _ => {
                    for _ in 0..n {
                        out.push(c);
                    }
                }
            },
        }
    }
    out
}

/// dateFromString: 解析出的字段
struct ParsedDate {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    frac: f64,
    /// Some(true) = PM
    pm: Option<bool>,
    /// 字符串里显式带的偏移(Z/z 字段)
    offset: Option<i32>,
    /// 小时来自 h/K(12 小时制)
    hour_is_12h: bool,
}

fn is_numeric_field(c: char, n: usize) -> bool {
    match c {
        'y' | 'Y' | 'u' | 'd' | 'D' | 'H' | 'k' | 'h' | 'K' | 'm' | 's' | 'S' => true,
        'M' | 'L' => n <= 2,
        _ => false,
    }
}

/// 在 `input[*pos..]` 上不区分大小写地匹配 `names` 里的某个名字,返回下标。
fn match_name(input: &[char], pos: &mut usize, names: &[&str]) -> Option<usize> {
    // 先试长名字,避免 "May" 之类前缀误配
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(names[i].chars().count()));
    for i in order {
        let name: Vec<char> = names[i].chars().collect();
        if *pos + name.len() <= input.len()
            && input[*pos..*pos + name.len()]
                .iter()
                .zip(name.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
        {
            *pos += name.len();
            return Some(i);
        }
    }
    None
}

fn parse_offset(input: &[char], pos: &mut usize) -> Option<i32> {
    let starts_with = |pos: usize, s: &str| -> bool {
        let s: Vec<char> = s.chars().collect();
        pos + s.len() <= input.len()
            && input[pos..pos + s.len()]
                .iter()
                .zip(s.iter())
                .all(|(a, b)| a.eq_ignore_ascii_case(b))
    };
    let mut had_prefix = false;
    if starts_with(*pos, "GMT") || starts_with(*pos, "UTC") {
        *pos += 3;
        had_prefix = true;
    }
    if *pos < input.len() && (input[*pos] == 'Z' || input[*pos] == 'z') && !had_prefix {
        *pos += 1;
        return Some(0);
    }
    if *pos >= input.len() || (input[*pos] != '+' && input[*pos] != '-') {
        return if had_prefix { Some(0) } else { None };
    }
    let sign = if input[*pos] == '-' { -1 } else { 1 };
    *pos += 1;
    let mut digits = String::new();
    let mut colon_at = None;
    while *pos < input.len() && digits.len() < 4 {
        let c = input[*pos];
        if c.is_ascii_digit() {
            digits.push(c);
        } else if c == ':' && colon_at.is_none() && !digits.is_empty() {
            colon_at = Some(digits.len());
        } else {
            break;
        }
        *pos += 1;
    }
    let (h, m): (i32, i32) = match (colon_at, digits.len()) {
        (Some(ci), _) => (
            digits[..ci].parse().ok()?,
            if digits.len() > ci { digits[ci..].parse().ok()? } else { 0 },
        ),
        (None, 1) | (None, 2) => (digits.parse().ok()?, 0),
        (None, 4) => (digits[..2].parse().ok()?, digits[2..].parse().ok()?),
        _ => return None,
    };
    if h > 18 || m > 59 {
        return None;
    }
    Some(sign * (h * 3600 + m * 60))
}

/// 按模式解析字符串。整串必须匹配(尾部空白除外),否则返回 None(真机返回 nil)。
fn parse_date(tokens: &[FormatToken], input: &str) -> Option<ParsedDate> {
    let input: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    // 缺省字段取 1970-01-01 00:00:00(与 iOS 6 时代 ICU 行为一致)
    let mut parsed = ParsedDate {
        year: 1970,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
        frac: 0.0,
        pm: None,
        offset: None,
        hour_is_12h: false,
    };
    for (idx, token) in tokens.iter().enumerate() {
        match *token {
            FormatToken::Literal(ref s) => {
                for lc in s.chars() {
                    if lc.is_whitespace() {
                        // 空白宽松匹配:0 个或多个
                        while pos < input.len() && input[pos].is_whitespace() {
                            pos += 1;
                        }
                    } else if pos < input.len() && input[pos] == lc {
                        pos += 1;
                    } else {
                        return None;
                    }
                }
            }
            FormatToken::Field(c, n) if is_numeric_field(c, n) => {
                // 紧挨着下一个数字字段时(如 "yyyyMMdd")按模式宽度定长读取,否则贪婪读取
                let next_is_numeric = matches!(
                    tokens.get(idx + 1),
                    Some(&FormatToken::Field(nc, nn)) if is_numeric_field(nc, nn)
                );
                let max_digits = if next_is_numeric {
                    if (c == 'y' || c == 'Y' || c == 'u') && n != 2 { 4 } else { n.max(1) }
                } else {
                    18
                };
                let start = pos;
                while pos < input.len() && pos - start < max_digits && input[pos].is_ascii_digit() {
                    pos += 1;
                }
                if pos == start {
                    return None;
                }
                let digits: String = input[start..pos].iter().collect();
                let value: i64 = digits.parse().ok()?;
                match c {
                    'y' | 'Y' | 'u' => {
                        parsed.year = if n == 2 && digits.len() == 2 {
                            if value < 50 { 2000 + value } else { 1900 + value }
                        } else {
                            value
                        };
                    }
                    'M' | 'L' => parsed.month = value,
                    'd' => parsed.day = value,
                    'D' => {
                        // 一年中的第几天:折算到 1 月 N 日,日子线性相加即可
                        parsed.month = 1;
                        parsed.day = value;
                    }
                    'H' => parsed.hour = value,
                    'k' => parsed.hour = if value == 24 { 0 } else { value },
                    'h' => {
                        parsed.hour = if value == 12 { 0 } else { value };
                        parsed.hour_is_12h = true;
                    }
                    'K' => {
                        parsed.hour = value;
                        parsed.hour_is_12h = true;
                    }
                    'm' => parsed.minute = value,
                    's' => parsed.second = value,
                    'S' => {
                        parsed.frac = value as f64 / 10f64.powi(digits.len().min(18) as i32);
                    }
                    _ => unreachable!(),
                }
            }
            FormatToken::Field(c, _) => match c {
                'M' | 'L' => {
                    let full = match_name(&input, &mut pos, &MONTH_NAMES);
                    let idx = match full {
                        Some(i) => i,
                        None => {
                            let abbrs: Vec<&str> = MONTH_NAMES.iter().map(|s| &s[..3]).collect();
                            match_name(&input, &mut pos, &abbrs)?
                        }
                    };
                    parsed.month = idx as i64 + 1;
                }
                'E' => {
                    if match_name(&input, &mut pos, &WEEKDAY_NAMES).is_none() {
                        let abbrs: Vec<&str> = WEEKDAY_NAMES.iter().map(|s| &s[..3]).collect();
                        match_name(&input, &mut pos, &abbrs)?;
                    }
                }
                'a' => {
                    let i = match_name(&input, &mut pos, &["AM", "PM"])?;
                    parsed.pm = Some(i == 1);
                }
                'G' => {
                    match_name(&input, &mut pos, &["AD", "BC"])?;
                }
                'Z' | 'z' | 'X' | 'x' | 'O' => {
                    parsed.offset = Some(parse_offset(&input, &mut pos)?);
                }
                _ => {
                    log!("Warning: NSDateFormatter dateFromString: pattern letter {:?} not supported", c);
                    return None;
                }
            },
        }
    }
    while pos < input.len() && input[pos].is_whitespace() {
        pos += 1;
    }
    if pos != input.len() {
        return None;
    }
    if parsed.hour_is_12h && parsed.pm == Some(true) {
        parsed.hour += 12;
    }
    // 非宽松模式:越界字段返回 nil(D 字段折算过的 day 放宽到 366)
    // [审查修 2026-09-13] 加年份上限,D 字段 day ≤ 366。根因:数值字段后面不是数字字段时贪婪读到
    // 18 位,原先年份与 D 字段的 day 不限范围,传进 days_in_month / civil_to_timestamp_i64 的 i64
    // 乘法(era*146097、days*86400)会溢出:开 overflow-checks 的构建(debug、cargo test)panic
    // 崩整个模拟器,release 静默回绕成错误日期而不是 nil。
    // 年份检查必须排在 days_in_month 之前(靠 || 短路):18 位年份在 days_in_month 里就已溢出。
    // 上限 1_000_000 年远低于溢出阈值(约 2.9e11 年),也在 MAX_HOST_TIMESTAMP(约 ±3170 万年)
    // 以内,stringFromDate: 往返不会被夹紧失真。解析只读数字,年份不会为负,故只设上限。
    let has_doy = tokens.iter().any(|t| matches!(t, FormatToken::Field('D', _)));
    if parsed.year > 1_000_000
        || !(1..=12).contains(&parsed.month)
        || parsed.day < 1
        || (has_doy && parsed.day > 366)
        || (!has_doy && parsed.day > days_in_month(parsed.year, parsed.month))
        || !(0..=24).contains(&parsed.hour)
        || !(0..=59).contains(&parsed.minute)
        || !(0..=60).contains(&parsed.second)
    {
        return None;
    }
    Some(parsed)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSDateFormatter: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSDateFormatterHostObject {
        date_format: None,
        time_zone: None,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())dealloc {
    // [深扫修 2026-09-11] 释放持有的格式串与时区(原先没有 dealloc,两者都泄漏)。
    let &NSDateFormatterHostObject { date_format, time_zone } = env.objc.borrow(this);
    if let Some(date_format) = date_format {
        release(env, date_format);
    }
    if let Some(time_zone) = time_zone {
        release(env, time_zone);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

- (())setDateFormat:(id)format { // NSString *
    let date_format: id = msg![env; format copy];
    let old = std::mem::replace(
        &mut env.objc.borrow_mut::<NSDateFormatterHostObject>(this).date_format,
        if date_format == nil { None } else { Some(date_format) },
    );
    if let Some(old) = old {
        release(env, old);
    }
}

// [深扫修 2026-09-11] setTimeZone: 真正生效。SDK(MAUtils getGMTDateTimeString:、
// YMLUtilToolkit fixStringForGMTFromDate: 等)会显式设 GMT;默认改成本地时区后若它仍是
// no-op,这些 "GMT 字符串" 反而会错。nil 表示恢复默认(本地时区)。
- (())setTimeZone:(id)time_zone { // NSTimeZone *
    if time_zone != nil {
        retain(env, time_zone);
    }
    let old = std::mem::replace(
        &mut env.objc.borrow_mut::<NSDateFormatterHostObject>(this).time_zone,
        if time_zone == nil { None } else { Some(time_zone) },
    );
    if let Some(old) = old {
        release(env, old);
    }
}

- (id)timeZone {
    let time_zone = env.objc.borrow::<NSDateFormatterHostObject>(this).time_zone;
    match time_zone {
        Some(time_zone) => time_zone,
        None => msg_class![env; NSTimeZone systemTimeZone],
    }
}

- (id)stringFromDate:(id)date {
    let &NSDateFormatterHostObject {
        date_format,
        time_zone,
    } = env.objc.borrow(this);
    // setDateFormat: 没被调过(date_format=None)时,旧代码 .unwrap() 会 panic 崩模拟器。
    // 真机此时返回 nil/空串,这里也安全降级成空串。
    let Some(date_format) = date_format else {
        let empty = ns_string::from_rust_string(env, String::new());
        return autorelease(env, empty);
    };
    let format = ns_string::to_rust_string(env, date_format).to_string();
    log_dbg!("date_format before: {:?}", format);

    let ti: NSTimeInterval = msg![env; date timeIntervalSinceReferenceDate];
    // [深扫修 2026-09-11] 不再调 CFAbsoluteTimeGetGregorianDate(ti, nil) 按 UTC 拆:
    // 按格式化器的时区(默认本地时区)取该日期的偏移,i64 换算(2001 以前不 panic)。
    let (unix_secs, frac) = cf_absolute_time_to_unix_floor(ti);
    let offset = match time_zone {
        Some(time_zone) => ns_time_zone::seconds_from_gmt_at_unix(env, time_zone, unix_secs),
        None => local_utc_offset_at(unix_secs),
    };

    // ★12 小时制(小写 h/hh)。摩尔庄园好友留言板的 -[MessageViewController configureCell:]
    // 用 @"MM/dd/yyyy hh:mm:ss" 格式化每条留言时间;旧实现曾因残留字母 unimplemented!() 崩溃。
    // 现在用模式分词器逐字段替换:未支持/纯装饰的模式字母以字面呈现,不崩(纯外观)。
    let tokens = tokenize_date_format(&format);
    let result = format_date(&tokens, unix_secs, frac, offset);

    log_dbg!("date_format after: {:?}", result);

    let res = ns_string::from_rust_string(env, result);
    autorelease(env, res)
}

// [深扫修 2026-09-11] 新增。-[GuessWorldCupMainLayer secondsFromNowToFutureDate:] 用
// "yyyy-MM-dd HH:mm:ss" 把 [NSDate date] 经 stringFromDate: 再 dateFromString: 回来,与
// 目标日期相减;ASIHTTPRequest dateFromRFC1123String: 等 SDK 也用。与 stringFromDate:
// 共用同一时区字段,往返不会漂移 8 小时。解析失败返回 nil(与真机一致)。
- (id)dateFromString:(id)string { // NSString *
    let &NSDateFormatterHostObject {
        date_format,
        time_zone,
    } = env.objc.borrow(this);
    let (Some(date_format), false) = (date_format, string == nil) else {
        return nil;
    };
    let format = ns_string::to_rust_string(env, date_format).to_string();
    let input = ns_string::to_rust_string(env, string).to_string();
    let tokens = tokenize_date_format(&format);
    let Some(parsed) = parse_date(&tokens, &input) else {
        log_dbg!("[NSDateFormatter dateFromString:{:?}] format {:?} => nil", input, format);
        return nil;
    };
    let local_secs = civil_to_timestamp_i64(
        parsed.year,
        parsed.month - 1,
        parsed.day,
        parsed.hour,
        parsed.minute,
        parsed.second,
    );
    let offset = match (parsed.offset, time_zone) {
        (Some(offset), _) => offset,
        (None, Some(time_zone)) => {
            ns_time_zone::seconds_from_gmt_for_wall_clock(env, time_zone, local_secs)
        }
        (None, None) => local_utc_offset_for_wall_clock(local_secs),
    };
    let unix_secs = local_secs - offset as i64;
    let ti: NSTimeInterval =
        (unix_secs - SECS_FROM_UNIX_TO_APPLE_EPOCHS as i64) as f64 + parsed.frac;
    log_dbg!("[NSDateFormatter dateFromString:{:?}] format {:?} => {}", input, format, ti);
    msg_class![env; NSDate dateWithTimeIntervalSinceReferenceDate:ti]
}

@end

};
