/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The `NSString` class cluster, including `NSMutableString`.
//!
//! Resources:
//! - Apple's [String Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Strings/introStrings.html)

mod path_algorithms;

use super::ns_keyed_archiver::{
    get_value_to_encode_for_current_key, set_value_to_encode_for_current_key,
};
use super::{ns_array, ns_keyed_unarchiver};
use super::{
    unichar, NSComparisonResult, NSInteger, NSNotFound, NSOrderedAscending, NSOrderedDescending,
    NSOrderedSame, NSRange, NSUInteger,
};
use crate::abi::VaList;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::_nib_archive_decoder;
use crate::frameworks::uikit::ui_font::{
    self, UILineBreakMode, UILineBreakModeWordWrap, UITextAlignment, UITextAlignmentLeft,
};
use crate::fs::GuestPath;
use crate::mach_o::MachO;
use crate::mem::{guest_size_of, ConstPtr, ConstVoidPtr, GuestUSize, Mem, MutPtr, Ptr, SafeRead};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, Class, ClassExports,
    HostObject, NSZonePtr, ObjC, SEL,
};
use crate::{fs, Environment};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::string::FromUtf16Error;

pub type NSStringEncoding = NSUInteger;
pub const NSASCIIStringEncoding: NSUInteger = 1;
pub const NSUTF8StringEncoding: NSUInteger = 4;
pub const NSISOLatin1StringEncoding: NSUInteger = 5;
pub const NSShiftJISStringEncoding: NSUInteger = 8;
pub const NSUnicodeStringEncoding: NSUInteger = 10;
pub const NSWindowsCP1252StringEncoding: NSUInteger = 12;
pub const NSMacOSRomanStringEncoding: NSUInteger = 30;
pub const NSUTF16StringEncoding: NSUInteger = NSUnicodeStringEncoding;
pub const NSUTF16BigEndianStringEncoding: NSUInteger = 0x90000100;
pub const NSUTF16LittleEndianStringEncoding: NSUInteger = 0x94000100;
// [扫描修 2026-09-15] F8-4:补齐其余常见 NSStringEncoding。中文编码的值 =
// CFStringConvertEncodingToNSStringEncoding(kCFStringEncoding…) = 0x80000000 | CF 编码值。
pub const NSNEXTSTEPStringEncoding: NSUInteger = 2;
pub const NSJapaneseEUCStringEncoding: NSUInteger = 3;
pub const NSNonLossyASCIIStringEncoding: NSUInteger = 7;
pub const NSISOLatin2StringEncoding: NSUInteger = 9;
pub const NSWindowsCP1251StringEncoding: NSUInteger = 11;
pub const NSWindowsCP1253StringEncoding: NSUInteger = 13;
pub const NSWindowsCP1254StringEncoding: NSUInteger = 14;
pub const NSWindowsCP1250StringEncoding: NSUInteger = 15;
pub const NSISO2022JPStringEncoding: NSUInteger = 21;
pub const NSUTF32StringEncoding: NSUInteger = 0x8c000100;
pub const NSUTF32BigEndianStringEncoding: NSUInteger = 0x98000100;
pub const NSUTF32LittleEndianStringEncoding: NSUInteger = 0x9c000100;
/// kCFStringEncodingGB_2312_80 = 0x0630
pub const NSGB2312StringEncoding: NSUInteger = 0x80000630;
/// kCFStringEncodingGBK_95 = 0x0631
pub const NSGBKStringEncoding: NSUInteger = 0x80000631;
/// kCFStringEncodingGB_18030_2000 = 0x0632
pub const NSGB18030StringEncoding: NSUInteger = 0x80000632;
/// kCFStringEncodingDOSChineseSimplif(CP936)= 0x0421
pub const NSDOSChineseSimplifStringEncoding: NSUInteger = 0x80000421;
/// kCFStringEncodingEUC_CN = 0x0930
pub const NSEUCCNStringEncoding: NSUInteger = 0x80000930;
/// kCFStringEncodingEUC_KR = 0x0940
pub const NSEUCKRStringEncoding: NSUInteger = 0x80000940;
/// kCFStringEncodingBig5 = 0x0A03
pub const NSBig5StringEncoding: NSUInteger = 0x80000A03;

pub type NSStringCompareOptions = NSUInteger;
pub const NSCaseInsensitiveSearch: NSUInteger = 1;
pub const NSLiteralSearch: NSUInteger = 2;
pub const NSBackwardsSearch: NSUInteger = 4;
pub const NSNumericSearch: NSUInteger = 64;
// [扫描修 2026-09-15] F8-4:补齐其余比较选项位(原来只认上面四个精确值,组合位直接 panic)。
pub const NSAnchoredSearch: NSUInteger = 8;
pub const NSDiacriticInsensitiveSearch: NSUInteger = 128;
pub const NSWidthInsensitiveSearch: NSUInteger = 256;
pub const NSForcedOrderingSearch: NSUInteger = 512;
// [复核修 2026-09-15] 该位有意不放进 KNOWN_COMPARE_OPTIONS(未实现 → 走"未知位"日志按字面处理),
// 目前只有单元测试引用它,非测试构建报 dead_code 警告;保留常量作文档,只抑制警告。
#[allow(dead_code)]
pub const NSRegularExpressionSearch: NSUInteger = 1024;

// [扫描修 2026-09-15] 原来的 C_STRING_FRIENDLY_ENCODINGS 白名单已删除:C 字符串相关方法改为按
// nul_terminator_size()/encode_str() 判断(见文件下方的编码辅助函数),不再用白名单 assert。

pub const NSMaximumStringLength: NSUInteger = (i32::MAX - 1) as _;

#[derive(Default)]
pub struct State {
    static_str_pool: HashMap<&'static str, id>,
}
impl State {
    fn get(env: &mut Environment) -> &mut Self {
        &mut env.framework_state.foundation.ns_string
    }
}

/// Constant strings embedded in the app binary use this struct. The name is
/// according to Ghidra, the rest is guesswork.
#[allow(non_camel_case_types)]
struct cfstringStruct {
    _isa: Class,
    flags: u32,
    bytes: ConstPtr<u8>,
    length: NSUInteger,
}
unsafe impl SafeRead for cfstringStruct {}

type Utf16String = Vec<u16>;

/// Belongs to _touchHLE_NSString.
enum StringHostObject {
    Utf8(Cow<'static, str>),
    /// Not necessarily well-formed UTF-16: might contain unpaired surrogates.
    Utf16(Utf16String),
}
impl HostObject for StringHostObject {}
impl StringHostObject {
    /// [扫描修 2026-09-15] F8-4:改为返回 Option。None 表示本实现不认识的编码,调用方按真机语义返回 nil。
    /// 原来遇到没列出的编码直接 panic;ASCII/Latin-1 遇到 ≥0x80 的字节、CP1252/Shift-JIS 遇到非法字节、
    /// UTF-16 奇数长度也都 assert panic。真机 iOS 在这些情况下都不会让进程崩溃。现在:
    /// - 已知编码一律宽容解码(非法字节换成替换字符,或按 Latin-1 映射),同类情况只打一次日志;
    /// - 编码 0:touchHLE 没实现 CFStringConvertEncodingToNSStringEncoding(dyld 链到返回 0 的空桩),
    ///   ASIHTTPRequest 等 SDK 按响应头 charset 换算出来的编码因此变成 0,这里按 UTF-8 宽容解码补上;
    /// - 其它不认识的编码返回 None(打一次日志)。
    fn decode(bytes: Cow<[u8]>, encoding: NSStringEncoding) -> Option<StringHostObject> {
        if bytes.is_empty() {
            return Some(StringHostObject::Utf8(Cow::Borrowed("")));
        }

        let host_object = match encoding {
            NSASCIIStringEncoding | NSNonLossyASCIIStringEncoding | NSNEXTSTEPStringEncoding => {
                if bytes.iter().all(|byte| byte.is_ascii()) {
                    // Safety: 上面已确认全是 ASCII 字节,必然是合法 UTF-8
                    let string = unsafe { String::from_utf8_unchecked(bytes.into_owned()) };
                    StringHostObject::Utf8(Cow::Owned(string))
                } else {
                    warn_once(format!("decode-ascii-high:{encoding:#x}"), || {
                        format!(
                            "Warning: 用 ASCII 类编码 {encoding:#x} 解码时遇到 ≥0x80 的字节(原来这里 assert panic),按 ISO Latin-1 逐字节映射"
                        )
                    });
                    StringHostObject::Utf8(Cow::Owned(latin1_to_string(&bytes)))
                }
            }
            // ISO Latin-1 的每个字节就是 U+0000..U+00FF,原来 assert 全 ASCII 是 TODO 占位
            NSISOLatin1StringEncoding => StringHostObject::Utf8(Cow::Owned(latin1_to_string(&bytes))),
            NSUTF8StringEncoding => {
                StringHostObject::Utf8(Cow::Owned(decode_utf8_lenient(bytes.into_owned())))
            }
            0 => {
                warn_once("decode-encoding-0".to_string(), || {
                    "Warning: 字符串编码为 0(多半来自未实现的 CFStringConvertEncodingToNSStringEncoding 空桩),按 UTF-8 宽容解码".to_string()
                });
                StringHostObject::Utf8(Cow::Owned(decode_utf8_lenient(bytes.into_owned())))
            }
            NSUTF16StringEncoding
            | NSUTF16BigEndianStringEncoding
            | NSUTF16LittleEndianStringEncoding => {
                let mut data: &[u8] = &bytes;
                if !data.len().is_multiple_of(2) {
                    warn_once(format!("decode-utf16-odd:{encoding:#x}"), || {
                        format!(
                            "Warning: UTF-16 字节数为奇数({} 字节,原来这里 assert panic),丢弃最后一个字节",
                            data.len()
                        )
                    });
                    data = &data[..data.len() - 1];
                }
                let bom = data.get(0..2).map(|b| [b[0], b[1]]);
                let is_big_endian = match encoding {
                    NSUTF16BigEndianStringEncoding => true,
                    NSUTF16LittleEndianStringEncoding => false,
                    // 通用 NSUTF16StringEncoding:有 BOM 时按 BOM 定字节序并剥掉 BOM(真机行为);
                    // 没有 BOM 时按宿主字节序(ARM 小端)处理,与原实现一致。
                    _ => match bom {
                        Some([0xFE, 0xFF]) => {
                            data = &data[2..];
                            true
                        }
                        Some([0xFF, 0xFE]) => {
                            data = &data[2..];
                            false
                        }
                        _ => false,
                    },
                };
                StringHostObject::Utf16(
                    data.chunks_exact(2)
                        .map(|chunk| {
                            if is_big_endian {
                                u16::from_be_bytes([chunk[0], chunk[1]])
                            } else {
                                u16::from_le_bytes([chunk[0], chunk[1]])
                            }
                        })
                        .collect(),
                )
            }
            NSUTF32StringEncoding
            | NSUTF32BigEndianStringEncoding
            | NSUTF32LittleEndianStringEncoding => {
                let mut data: &[u8] = &bytes;
                if !data.len().is_multiple_of(4) {
                    warn_once(format!("decode-utf32-len:{encoding:#x}"), || {
                        format!(
                            "Warning: UTF-32 字节数 {} 不是 4 的倍数,丢弃末尾不完整的码元",
                            data.len()
                        )
                    });
                    data = &data[..data.len() - data.len() % 4];
                }
                let bom = data.get(0..4).map(|b| [b[0], b[1], b[2], b[3]]);
                let is_big_endian = match encoding {
                    NSUTF32BigEndianStringEncoding => true,
                    NSUTF32LittleEndianStringEncoding => false,
                    _ => match bom {
                        Some([0x00, 0x00, 0xFE, 0xFF]) => {
                            data = &data[4..];
                            true
                        }
                        Some([0xFF, 0xFE, 0x00, 0x00]) => {
                            data = &data[4..];
                            false
                        }
                        _ => false,
                    },
                };
                let string: String = data
                    .chunks_exact(4)
                    .map(|chunk| {
                        let raw = [chunk[0], chunk[1], chunk[2], chunk[3]];
                        let value = if is_big_endian {
                            u32::from_be_bytes(raw)
                        } else {
                            u32::from_le_bytes(raw)
                        };
                        char::from_u32(value).unwrap_or(char::REPLACEMENT_CHARACTER)
                    })
                    .collect();
                StringHostObject::Utf8(Cow::Owned(string))
            }
            _ => match legacy_encoding_for(encoding) {
                Some(legacy) => {
                    // 用 without_bom_handling:不让 encoding_rs 按 BOM 偷偷改成 UTF-8/16
                    // (原来 CP1252/Shift-JIS 分支用 assert_eq! 防这个,现在直接不嗅探)
                    let (decoded, had_errors) = legacy.decode_without_bom_handling(&bytes);
                    if had_errors {
                        warn_once(format!("decode-invalid:{encoding:#x}"), || {
                            format!(
                                "Warning: 按 {} 解码时遇到非法字节(原来这里 assert panic),已替换为 U+FFFD",
                                legacy.name()
                            )
                        });
                    }
                    StringHostObject::Utf8(Cow::Owned(decoded.into_owned()))
                }
                None => {
                    warn_once(format!("decode-unknown:{encoding:#x}"), || {
                        format!(
                            "Warning: 遇到未实现的字符串编码 {encoding:#x}(原来这里 panic),按真机「无法转换」语义返回 nil"
                        )
                    });
                    return None;
                }
            },
        };
        Some(host_object)
    }
    fn to_utf8(&self) -> Result<Cow<'static, str>, FromUtf16Error> {
        match self {
            StringHostObject::Utf8(utf8) => Ok(utf8.clone()),
            StringHostObject::Utf16(utf16) => Ok(Cow::Owned(String::from_utf16(utf16)?)),
        }
    }
    /// Mutate the object, converting to UTF-16 if the string was not already
    /// UTF-16. Returns a reference to the UTF-16 content and a boolean that is
    /// [true] if a conversion happened.
    fn convert_to_utf16_inplace(&mut self) -> (&mut Utf16String, bool) {
        let converted = match self {
            Self::Utf8(_) => {
                *self = Self::Utf16(self.iter_code_units().collect());
                true
            }
            Self::Utf16(_) => false,
        };
        let Self::Utf16(utf16) = self else {
            unreachable!();
        };
        (utf16, converted)
    }
    /// Iterate over the string as UTF-16 code units.
    fn iter_code_units(&self) -> CodeUnitIterator<'_> {
        match self {
            StringHostObject::Utf8(utf8) => CodeUnitIterator::Utf8(utf8.encode_utf16()),
            StringHostObject::Utf16(utf16) => CodeUnitIterator::Utf16(utf16.iter()),
        }
    }
}

enum CodeUnitIterator<'a> {
    Utf8(std::str::EncodeUtf16<'a>),
    Utf16(std::slice::Iter<'a, u16>),
}
impl Iterator for CodeUnitIterator<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<u16> {
        match self {
            CodeUnitIterator::Utf8(iter) => iter.next(),
            CodeUnitIterator::Utf16(iter) => iter.next().copied(),
        }
    }
}
impl Clone for CodeUnitIterator<'_> {
    fn clone(&self) -> Self {
        match self {
            CodeUnitIterator::Utf8(iter) => CodeUnitIterator::Utf8(iter.clone()),
            CodeUnitIterator::Utf16(iter) => CodeUnitIterator::Utf16(iter.clone()),
        }
    }
}
impl CodeUnitIterator<'_> {
    /// If the sequence of code units in `prefix` is a prefix of `self`,
    /// return [Some] with `self` advanced past that prefix, otherwise [None].
    ///
    /// Code units comparison is done conditional to `case_insensitive` bool:
    /// if it's true, the code units are converted to chars first and compared
    /// as lowercase variants, otherwise the match is exact.
    fn strip_prefix(&self, prefix: &CodeUnitIterator, case_insensitive: bool) -> Option<Self> {
        let mut self_match = self.clone();
        let mut prefix_match = prefix.clone();
        loop {
            match prefix_match.next() {
                None => {
                    return Some(self_match);
                }
                Some(prefix_c) => {
                    let self_c = self_match.next();
                    if case_insensitive {
                        // [扫描修 2026-09-15] 原来遇到代理项(非 BMP 字符的半边)panic,改为精确比较该码元
                        if !units_equal(self_c?, prefix_c, true) {
                            return None;
                        }
                    } else if self_c != Some(prefix_c) {
                        return None;
                    }
                }
            }
        }
    }
}

/// Helper for formatting methods. They can't call eachother currently due to
/// full vararg passthrough being missing.
pub fn with_format(env: &mut Environment, format: id, args: VaList) -> String {
    let format_string = to_rust_string(env, format);

    log_dbg!("Formatting {:?} ({:?})", format, format_string);

    let res = crate::libc::stdio::printf::printf_inner::<true, _>(
        env,
        |_, idx| {
            if idx as usize == format_string.len() {
                b'\0'
            } else {
                format_string.as_bytes()[idx as usize]
            }
        },
        args,
    );
    // TODO: what if it's not valid UTF-8?
    String::from_utf8(res).unwrap()
}

pub fn from_rust_ordering(ordering: std::cmp::Ordering) -> NSComparisonResult {
    match ordering {
        std::cmp::Ordering::Less => NSOrderedAscending,
        std::cmp::Ordering::Equal => NSOrderedSame,
        std::cmp::Ordering::Greater => NSOrderedDescending,
    }
}

// ============================================================================
// [扫描修 2026-09-15] F8-4:比较选项按位解析 + 字符串编码转换的公共辅助函数
// ============================================================================
// 原来 rangeOfString:options: / compare:options: / 替换 三处都按 options 的精确值 match,
// 组合位(如 CaseInsensitive|Backwards = 5、CaseInsensitive|Numeric = 0x41)或没列出的位
// (NSAnchoredSearch = 8、连 replace 的 NSLiteralSearch = 2)一律 unimplemented! 整机 panic;
// 编码侧 decode/cStringUsingEncoding:/getCString… 也是遇到没列出的值就 panic。
// 目前游戏自己的调用点都在已支持的集合里(compare:options: 用 0x40,编码全是 4),
// 这里主要防 SDK 路径和在线新数据包踩到。纯函数部分(不碰 env)在文件末尾有单元测试。

/// 同一类告警整局只打一次(按 key 去重),避免某条路径每帧刷屏。
fn warn_once(key: String, message: impl FnOnce() -> String) {
    static SEEN: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    let first_time = SEEN.lock().map(|mut seen| seen.insert(key)).unwrap_or(false);
    if first_time {
        log!("{}(同类告警只显示一次)", message());
    }
}

/// 解析后的 NSStringCompareOptions。
/// - NSLiteralSearch:本实现本来就按 UTF-16 码元逐个比较,等价于字面比较,无需单独处理;
/// - NSDiacriticInsensitiveSearch / NSWidthInsensitiveSearch:忽略(按区分变音/全半角处理);
/// - NSRegularExpressionSearch 及其它未知位:忽略并打一次日志,按字面处理。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CompareOptions {
    case_insensitive: bool,
    backwards: bool,
    anchored: bool,
    numeric: bool,
    forced_ordering: bool,
}

const KNOWN_COMPARE_OPTIONS: NSUInteger = NSCaseInsensitiveSearch
    | NSLiteralSearch
    | NSBackwardsSearch
    | NSAnchoredSearch
    | NSNumericSearch
    | NSDiacriticInsensitiveSearch
    | NSWidthInsensitiveSearch
    | NSForcedOrderingSearch;

/// 返回 (解析结果, 未支持的位)。
fn parse_compare_options(options: NSStringCompareOptions) -> (CompareOptions, NSUInteger) {
    (
        CompareOptions {
            case_insensitive: (options & NSCaseInsensitiveSearch) != 0,
            backwards: (options & NSBackwardsSearch) != 0,
            anchored: (options & NSAnchoredSearch) != 0,
            numeric: (options & NSNumericSearch) != 0,
            forced_ordering: (options & NSForcedOrderingSearch) != 0,
        },
        options & !KNOWN_COMPARE_OPTIONS,
    )
}

fn compare_options_logged(options: NSStringCompareOptions, api: &str) -> CompareOptions {
    let (parsed, unsupported) = parse_compare_options(options);
    if unsupported != 0 {
        warn_once(format!("compare-options:{api}:{unsupported:#x}"), || {
            format!(
                "Warning: [NSString {api}] options={options:#x} 含未实现的位 {unsupported:#x}(如 NSRegularExpressionSearch=0x400),已忽略这些位按字面处理(原来这里 unimplemented! panic)"
            )
        });
    }
    parsed
}

/// 两个 UTF-16 码元是否相等。大小写不敏感时按 char 的小写形式比较;
/// 代理项(非 BMP 字符的半边)不是合法 char,无法单独折叠大小写,只做精确比较(原来这里 panic)。
fn units_equal(a: u16, b: u16, case_insensitive: bool) -> bool {
    if a == b {
        return true;
    }
    if !case_insensitive {
        return false;
    }
    match (char::from_u32(a as u32), char::from_u32(b as u32)) {
        (Some(x), Some(y)) => x.to_lowercase().eq(y.to_lowercase()),
        _ => false,
    }
}

fn cmp_units(a: u16, b: u16, case_insensitive: bool) -> std::cmp::Ordering {
    if !case_insensitive || a == b {
        return a.cmp(&b);
    }
    match (char::from_u32(a as u32), char::from_u32(b as u32)) {
        (Some(x), Some(y)) => x.to_lowercase().cmp(y.to_lowercase()),
        _ => a.cmp(&b),
    }
}

/// 在 hay 里找 needle,返回匹配起点。NSAnchoredSearch 只看开头(带 NSBackwardsSearch 时只看结尾)。
fn find_code_units(hay: &[u16], needle: &[u16], opts: CompareOptions) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    let last = hay.len() - needle.len();
    let matches_at = |start: usize| {
        hay[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(&x, &y)| units_equal(x, y, opts.case_insensitive))
    };
    if opts.anchored {
        let start = if opts.backwards { last } else { 0 };
        return matches_at(start).then_some(start);
    }
    if opts.backwards {
        (0..=last).rev().find(|&start| matches_at(start))
    } else {
        (0..=last).find(|&start| matches_at(start))
    }
}

/// compare:options: 的核心。NSNumericSearch:两边同时遇到 ASCII 数字串时按数值比较
/// (先去前导零再比位数和字典序,长数字串不会溢出;原实现用 u32 累加,超过 10 位会溢出)。
/// NSForcedOrderingSearch:大小写不敏感/数值比较结果相等时,再用字面比较强制分出先后。
fn compare_code_units(a: &[u16], b: &[u16], opts: CompareOptions) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn is_digit(unit: u16) -> bool {
        (b'0' as u16..=b'9' as u16).contains(&unit)
    }
    fn digit_run(units: &[u16], start: usize) -> (&[u16], usize) {
        let end = units[start..]
            .iter()
            .position(|&unit| !is_digit(unit))
            .map_or(units.len(), |offset| start + offset);
        let run = &units[start..end];
        let first_significant = run
            .iter()
            .position(|&unit| unit != b'0' as u16)
            .unwrap_or(run.len());
        (&run[first_significant..], end)
    }

    let (mut i, mut j) = (0usize, 0usize);
    let primary = loop {
        match (a.get(i).copied(), b.get(j).copied()) {
            (None, None) => break Ordering::Equal,
            (None, Some(_)) => break Ordering::Less,
            (Some(_), None) => break Ordering::Greater,
            (Some(unit_a), Some(unit_b)) => {
                if opts.numeric && is_digit(unit_a) && is_digit(unit_b) {
                    let (digits_a, next_i) = digit_run(a, i);
                    let (digits_b, next_j) = digit_run(b, j);
                    let order = digits_a
                        .len()
                        .cmp(&digits_b.len())
                        .then_with(|| digits_a.cmp(digits_b));
                    if order != Ordering::Equal {
                        break order;
                    }
                    i = next_i;
                    j = next_j;
                    continue;
                }
                let order = cmp_units(unit_a, unit_b, opts.case_insensitive);
                if order != Ordering::Equal {
                    break order;
                }
                i += 1;
                j += 1;
            }
        }
    };
    if primary == Ordering::Equal && opts.forced_ordering && (opts.case_insensitive || opts.numeric)
    {
        return a.cmp(b);
    }
    primary
}

/// 替换 src 中所有(不重叠的)target,返回 (结果, 替换次数)。
/// NSAnchoredSearch 只替换开头(带 NSBackwardsSearch 时只替换结尾)那一处;
/// NSBackwardsSearch 决定重叠匹配时从哪头开始取(如 "aaa" 里替换 "aa")。
fn replace_code_units(
    src: &[u16],
    target: &[u16],
    replacement: &[u16],
    opts: CompareOptions,
) -> (Utf16String, usize) {
    if target.is_empty() || target.len() > src.len() {
        return (src.to_vec(), 0);
    }
    let matches_at = |start: usize| {
        src[start..start + target.len()]
            .iter()
            .zip(target)
            .all(|(&x, &y)| units_equal(x, y, opts.case_insensitive))
    };
    let mut starts: Vec<usize> = Vec::new();
    if opts.anchored {
        let start = if opts.backwards {
            src.len() - target.len()
        } else {
            0
        };
        if matches_at(start) {
            starts.push(start);
        }
    } else if opts.backwards {
        let mut end = src.len();
        while end >= target.len() {
            let start = end - target.len();
            if matches_at(start) {
                starts.push(start);
                end = start;
            } else {
                end -= 1;
            }
        }
        starts.reverse();
    } else {
        let mut start = 0;
        while start + target.len() <= src.len() {
            if matches_at(start) {
                starts.push(start);
                start += target.len();
            } else {
                start += 1;
            }
        }
    }
    let mut result = Vec::with_capacity(src.len());
    let mut copied_up_to = 0;
    for &start in &starts {
        result.extend_from_slice(&src[copied_up_to..start]);
        result.extend_from_slice(replacement);
        copied_up_to = start + target.len();
    }
    result.extend_from_slice(&src[copied_up_to..]);
    (result, starts.len())
}

/// 取任意 NSString 的 UTF-16 码元。宿主字符串直接读宿主对象;guest 自定义子类走 length/characterAtIndex:。
fn collect_code_units(env: &mut Environment, string: id) -> Utf16String {
    if string == nil {
        return Vec::new();
    }
    let is_host_string = env
        .objc
        .get_host_object(string)
        .is_some_and(|host| host.as_any().is::<StringHostObject>());
    if is_host_string {
        let mut units = Vec::new();
        for_each_code_unit(env, string, |_, unit| units.push(unit));
        units
    } else {
        let len: NSUInteger = msg![env; string length];
        (0..len)
            .map(|index| {
                let unit: u16 = msg![env; string characterAtIndex:index];
                unit
            })
            .collect()
    }
}

/// rangeOfString:options: 与 rangeOfString:options:range: 的公共实现。
fn range_of_string_common(
    env: &mut Environment,
    this: id,
    search_string: id,
    options: NSStringCompareOptions,
    range: NSRange,
) -> NSRange {
    let not_found = NSRange {
        location: NSNotFound as NSUInteger,
        length: 0,
    };
    if search_string == nil {
        return not_found;
    }
    let hay = collect_code_units(env, this);
    let needle = collect_code_units(env, search_string);
    // NSRange 是 packed 结构体,先拷到局部变量再用
    let (location, length) = (range.location as usize, range.length as usize);
    let start = location.min(hay.len());
    let end = location.saturating_add(length).min(hay.len());
    if location.saturating_add(length) > hay.len() {
        log!(
            "Warning: [(NSString*){:?} rangeOfString:options:range:{{{}, {}}}] 越界(字符串长度 {},真机抛 NSRangeException),裁剪后搜索",
            this,
            location,
            length,
            hay.len()
        );
    }
    let opts = compare_options_logged(options, "rangeOfString:options:");
    match find_code_units(&hay[start..end], &needle, opts) {
        Some(offset) => NSRange {
            location: (start + offset) as NSUInteger,
            length: needle.len() as NSUInteger,
        },
        None => not_found,
    }
}

/// UTF-8 宽容解码:取合法前缀,绝不崩溃(原 NSUTF8 分支的逻辑原样搬过来,编码 0 也复用)。
fn decode_utf8_lenient(bytes: Vec<u8>) -> String {
    // 真实 iOS 的 NSUTF8 解码对非法/截断字节是宽容的(不会崩)。touchHLE 原来
    // 直接 unwrap():当多字节字符(如中文名)被某处定长缓冲/存档截断在 UTF-8
    // 字符中间时(尾部出现半个汉字,如 0xE5),就 panic(实测离线改中文名后崩)。
    // 改为宽容解码:取合法前缀,绝不崩 —— 与 iOS 行为一致或更宽松。
    match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            log!(
                "Warning: [MoleWorld] NSUTF8 解码遇到非法/截断字节(共 {} 字节,\
                 合法到 {});取合法前缀避免崩溃(多半是某处定长缓冲把中文等多字节\
                 字符截断在字符中间)。",
                bytes.len(),
                valid
            );
            bytes.truncate(valid);
            // SAFETY: bytes[..valid_up_to] 按 Utf8Error 定义是合法 UTF-8。
            unsafe { String::from_utf8_unchecked(bytes) }
        }
    }
}

/// ISO Latin-1:每个字节就是 U+0000..U+00FF。
fn latin1_to_string(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| byte as char).collect()
}

/// 交给 encoding_rs 处理的传统编码(UTF-8/16/32、ASCII、Latin-1 另行手工处理)。
fn legacy_encoding_for(encoding: NSStringEncoding) -> Option<&'static encoding_rs::Encoding> {
    Some(match encoding {
        NSMacOSRomanStringEncoding => encoding_rs::MACINTOSH,
        NSWindowsCP1252StringEncoding => encoding_rs::WINDOWS_1252,
        NSWindowsCP1251StringEncoding => encoding_rs::WINDOWS_1251,
        NSWindowsCP1250StringEncoding => encoding_rs::WINDOWS_1250,
        NSWindowsCP1253StringEncoding => encoding_rs::WINDOWS_1253,
        NSWindowsCP1254StringEncoding => encoding_rs::WINDOWS_1254,
        NSISOLatin2StringEncoding => encoding_rs::ISO_8859_2,
        NSShiftJISStringEncoding => encoding_rs::SHIFT_JIS,
        NSJapaneseEUCStringEncoding => encoding_rs::EUC_JP,
        NSISO2022JPStringEncoding => encoding_rs::ISO_2022_JP,
        NSGB18030StringEncoding => encoding_rs::GB18030,
        NSGBKStringEncoding
        | NSGB2312StringEncoding
        | NSEUCCNStringEncoding
        | NSDOSChineseSimplifStringEncoding => encoding_rs::GBK,
        NSBig5StringEncoding => encoding_rs::BIG5,
        NSEUCKRStringEncoding => encoding_rs::EUC_KR,
        _ => return None,
    })
}

/// 结尾 NUL 占几个字节:UTF-16 两个、UTF-32 四个,其余一个。
fn nul_terminator_size(encoding: NSStringEncoding) -> GuestUSize {
    match encoding {
        NSUTF16StringEncoding | NSUTF16BigEndianStringEncoding | NSUTF16LittleEndianStringEncoding => 2,
        NSUTF32StringEncoding | NSUTF32BigEndianStringEncoding | NSUTF32LittleEndianStringEncoding => 4,
        _ => 1,
    }
}

/// 把字符串按 NSStringEncoding 编码成字节(不含结尾 NUL、不带 BOM;通用 UTF-16/32 按宿主小端)。
/// 返回 None:编码未实现,或 `lossy == false` 时遇到该编码表示不了的字符(真机此时返回 NULL/nil)。
/// `lossy == true` 时表示不了的字符写成 '?'。
fn encode_str(string: &str, encoding: NSStringEncoding, lossy: bool) -> Option<Vec<u8>> {
    match encoding {
        // 0 的来历见 decode()
        NSUTF8StringEncoding | 0 => Some(string.as_bytes().to_vec()),
        NSASCIIStringEncoding | NSNonLossyASCIIStringEncoding | NSNEXTSTEPStringEncoding => {
            if string.is_ascii() {
                return Some(string.as_bytes().to_vec());
            }
            if !lossy {
                return None;
            }
            Some(
                string
                    .chars()
                    .map(|c| if c.is_ascii() { c as u32 as u8 } else { b'?' })
                    .collect(),
            )
        }
        NSISOLatin1StringEncoding => {
            let mut out = Vec::with_capacity(string.len());
            for c in string.chars() {
                if (c as u32) <= 0xFF {
                    out.push(c as u32 as u8);
                } else if lossy {
                    out.push(b'?');
                } else {
                    return None;
                }
            }
            Some(out)
        }
        NSUTF16StringEncoding | NSUTF16LittleEndianStringEncoding => {
            Some(string.encode_utf16().flat_map(u16::to_le_bytes).collect())
        }
        NSUTF16BigEndianStringEncoding => {
            Some(string.encode_utf16().flat_map(u16::to_be_bytes).collect())
        }
        NSUTF32StringEncoding | NSUTF32LittleEndianStringEncoding => {
            Some(string.chars().flat_map(|c| (c as u32).to_le_bytes()).collect())
        }
        NSUTF32BigEndianStringEncoding => {
            Some(string.chars().flat_map(|c| (c as u32).to_be_bytes()).collect())
        }
        _ => {
            let legacy = legacy_encoding_for(encoding)?;
            let (bytes, _, had_unmappable) = legacy.encode(string);
            if !had_unmappable {
                return Some(bytes.into_owned());
            }
            if !lossy {
                return None;
            }
            let mut out = Vec::with_capacity(string.len());
            let mut buf = [0u8; 4];
            for c in string.chars() {
                let (piece, _, unmappable) = legacy.encode(c.encode_utf8(&mut buf));
                if unmappable {
                    out.push(b'?');
                } else {
                    out.extend_from_slice(&piece);
                }
            }
            Some(out)
        }
    }
}

/// dataUsingEncoding: / writeToFile:…encoding: 用:通用 NSUTF16StringEncoding/NSUTF32StringEncoding
/// 按真机习惯在前面加 BOM(小端),其余同 encode_str。
fn encode_for_data(string: &str, encoding: NSStringEncoding, lossy: bool) -> Option<Vec<u8>> {
    let bytes = encode_str(string, encoding, lossy)?;
    let bom: &[u8] = match encoding {
        NSUTF16StringEncoding => &[0xFF, 0xFE],
        NSUTF32StringEncoding => &[0xFF, 0xFE, 0x00, 0x00],
        _ => &[],
    };
    if bom.is_empty() {
        return Some(bytes);
    }
    let mut out = Vec::with_capacity(bom.len() + bytes.len());
    out.extend_from_slice(bom);
    out.extend_from_slice(&bytes);
    Some(out)
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// NSString is an abstract class. A subclass must provide:
// - (NSUInteger)length;
// - (unichar)characterAtIndex:(NSUInteger)index;
// We can pick whichever subclass we want for the various alloc methods.
// For the time being, that will always be _touchHLE_NSString.
@implementation NSString: NSObject

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSString might be subclassed by something which needs allocWithZone:
    // to have the normal behaviour. Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSString", &mut env.mem));
    msg_class![env; _touchHLE_NSString allocWithZone:zone]
}

+ (id)string {
    let str: id = msg![env; this new];
    autorelease(env, str)
}

+ (id)stringWithString:(id)string { // NSString*
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithString:string];
    autorelease(env, new)
}

+ (id)stringWithUTF8String:(ConstPtr<u8>)utf8_string {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithUTF8String:utf8_string];
    autorelease(env, new)
}

+ (id)stringWithCString:(ConstPtr<u8>)c_string {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCString:c_string];
    autorelease(env, new)
}

+ (id)stringWithCString:(ConstPtr<u8>)c_string length:(NSUInteger)length {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCString:c_string length:length];
    autorelease(env, new)
}

+ (id)stringWithCString:(ConstPtr<u8>)c_string
               encoding:(NSStringEncoding)encoding {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCString:c_string encoding:encoding];
    autorelease(env, new)
}

+ (id)stringWithContentsOfFile:(id)path { // NSString*
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfFile:path];
    autorelease(env, new)
}

+ (id)stringWithContentsOfFile:(id)path // NSString*
                      encoding:(NSStringEncoding)encoding
                         error:(MutPtr<id>)error { // NSError**
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfFile:path
                                              encoding:encoding
                                                 error:error];
    autorelease(env, new)
}

+ (id)stringWithFormat:(id)format, // NSString*
                       ...args {
    let res = with_format(env, format, args.start());
    let res = from_rust_string(env, res);
    let res = autorelease(env, res);

    // This will return _touchHLE_NSString or _touchHLE_NSMutableString
    msg![env; this stringWithString:res]
}

+ (id)stringWithCharacters:(ConstPtr<unichar>)characters length:(NSUInteger)length {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCharacters:characters length:length];
    autorelease(env, new)
}

+ (id)pathWithComponents:(id)components {
    let count: NSUInteger = msg![env; components count];
    if count == 0 {
        return get_static_str(env, "");
    }
    let mut res = msg_class![env; NSString new];
    let enumerator: id = msg![env; components objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        let len: NSUInteger = msg![env; next length];
        if len == 0 {
            continue;
        }
        // FIXME: this leads to O(N^2) for N char string, but it should be O(N)
        res = msg![env; res stringByAppendingPathComponent:next];
    }
    log_dbg!("pathWithComponents: {} -> '{}'", {
        let desc = msg![env; components description];
        to_rust_string(env, desc)
    }, to_rust_string(env, res));
    res
}

+ (NSStringEncoding)defaultCStringEncoding {
    // I don't want to figure out what that is on all platforms, and the use
    // I've seen of this method was on ASCII strings, so let's just hardcode
    // UTF-8 and hope that works.
    NSUTF8StringEncoding
}

// NSCoding implementation
// [深扫修 2026-09-12] 从 _touchHLE_NSString 上移到抽象 NSString。根因:_touchHLE_NSMutableString
// 的继承链是 NSMutableString→NSString,拿不到兄弟分支 _touchHLE_NSString 上的方法——
// 归档可变串时 encodeWithCoder: 不响应,$objects 里只留一个空字典;解 Apple 档里的
// {$class: NSMutableString, NS.string} 时 initWithCoder: 不响应,读成 nil。
// 可变接收者解出后换成可变副本,保持类型语义。
- (id)initWithCoder:(id)coder {
    let class: Class = msg![env; coder class];
    let keyed_unarch_class: Class = msg_class![env; NSKeyedUnarchiver class];
    let nib_archive_class: Class = msg_class![env; _touchHLE_NIBArchiveDecoder class];
    let new_str = if env.objc.class_is_subclass_of(class, keyed_unarch_class) {
        ns_keyed_unarchiver::decode_current_string(env, coder)
    } else if env.objc.class_is_subclass_of(class, nib_archive_class) {
        _nib_archive_decoder::decode_current_string(env, coder)
    } else {
        unimplemented!();
    };
    let this_class: Class = msg![env; this class];
    let mutable_class = env.objc.get_known_class("NSMutableString", &mut env.mem);
    let is_mutable = env.objc.class_is_subclass_of(this_class, mutable_class);
    release(env, this);
    if is_mutable && new_str != nil {
        let mutable_str: id = msg![env; new_str mutableCopy];
        release(env, new_str);
        mutable_str
    } else {
        new_str
    }
}
- (())encodeWithCoder:(id)coder {
    let string = to_rust_string(env, this);
    // [MoleWorld] 原来这里 assert! 全 ASCII(TODO 占位),导致归档含中文的字符串
    // (如离线改的中文庄园名,经 saveUserInfoData → NSKeyedArchiver → encodeWithCoder:)
    // 直接 panic(实测离线改中文名稳定复现)。二进制 plist 写入器(plist::to_writer_binary,
    // 见 ns_keyed_archiver.rs)原生支持 UTF-8,plist::Value::String 可容纳任意 UTF-8 字符串
    // → 去掉这个过严断言即可正确归档非 ASCII 字符串,与真实 iOS 行为一致。
    // [审查修 2026-09-13] 区分可变与不可变,让归档往返后类型不变。根因:原来一律把 $objects
    // 当前条目整条换成裸串,可变串解档后变成不可变 _touchHLE_NSString,上面 initWithCoder:
    // 的 mutableCopy 分支永远走不到。
    // - 不可变串:保持原写法(裸串,encode_object 对它不写 $class),旧存档形态不变。
    // - 可变串:encode_object 已在当前条目字典写好 $class(= NSMutableString),这里只补
    //   "NS.string",得到 Apple 的 {$class: NSMutableString, NS.string};解档走
    //   NSMutableString alloc → initWithCoder: → decode_current_string 读 NS.string → mutableCopy。
    // 可变判断必须与 ns_keyed_archiver.rs encode_object 的 $class 判断保持一致(见该处注释)。
    let this_class: Class = msg![env; this class];
    let mutable_class = env.objc.get_known_class("NSMutableString", &mut env.mem);
    if env.objc.class_is_subclass_of(this_class, mutable_class) {
        let scope = get_value_to_encode_for_current_key(env, coder);
        scope.insert("NS.string".into(), plist::Value::String(string.to_string()));
    } else {
        set_value_to_encode_for_current_key(env, coder, plist::Value::String(string.to_string()));
    }
}

- (id)initWithUTF8String:(ConstPtr<u8>)utf8_string {
    msg![env; this initWithCString:utf8_string encoding:NSUTF8StringEncoding]
}

- (id)initWithCString:(ConstPtr<u8>)c_string {
    let encoding: NSStringEncoding = msg_class![env; NSString defaultCStringEncoding];
    msg![env; this initWithCString:c_string encoding:encoding]
}

- (id)initWithCString:(ConstPtr<u8>)c_string length:(NSUInteger)len {
    let encoding: NSStringEncoding = msg_class![env; NSString defaultCStringEncoding];
    msg![env; this initWithBytes:c_string length:len encoding:encoding]
}

- (id)initWithCString:(ConstPtr<u8>)c_string
             encoding:(NSStringEncoding)encoding {
    // [扫描修 2026-09-15] F8-4:原来 assert 编码必须在 5 种白名单里,GB18030/Big5/Shift-JIS 等直接 panic。
    // 现在凡是以 8 位为单元、单个 NUL 结尾的编码都照常解码;UTF-16/32 不能当 C 字符串读(真机文档同样
    // 要求 8 位编码),打一次日志后返回 nil。
    if nul_terminator_size(encoding) != 1 {
        warn_once(format!("cstring-wide:{encoding:#x}"), || {
            format!("Warning: initWithCString:encoding: 收到非 8 位单元的编码 {encoding:#x},无法按 C 字符串读取,返回 nil")
        });
        release(env, this);
        return nil;
    }
    let len: NSUInteger = env.mem.cstr_at(c_string).len().try_into().unwrap();
    msg![env; this initWithBytes:c_string length:len encoding:encoding]
}

- (id)dataUsingEncoding:(NSStringEncoding)encoding {
    msg![env; this dataUsingEncoding:encoding allowLossyConversion:false]
}

// These are the two methods that have to be overridden by subclasses, so these
// implementations don't have to care about foreign subclasses.
- (NSUInteger)length {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);

    // To know what length the string has in UTF-16, we need to convert it to
    // UTF-16. If `length` is used, it's likely other methods that operate on
    // UTF-16 code unit boundaries will also be used (e.g. `characterAt:`), so
    // persisting the UTF-16 version lets us potentially optimize future method
    // calls. This is a heuristic though and won't always be optimal.
    let (utf16, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} length]: converted string to UTF-16", this);
    }

    utf16.len().try_into().unwrap()
}
- (u16)characterAtIndex:(NSUInteger)index {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);

    // The string has to be in UTF-16 to get O(1) rather than O(n) indexing, and
    // it's likely this method will be called many times, so converting it to
    // UTF-16 as early as possible and persisting that representation is
    // probably best for performance. This is a heuristic though and won't
    // always be optimal.
    let (utf16, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} characterAtIndex:{:?}]: converted string to UTF-16", this, index);
    }

    // TODO: raise exception instead of panicking?
    utf16[index as usize]
}

- (NSUInteger)lengthOfBytesUsingEncoding:(NSStringEncoding)encoding {
    // [MoleWorld] 原来 assert! 全 ASCII(TODO);对 UTF-8 编码,字节长度就是 string.len()
    // (UTF-8 字节数),含中文也正确 → 去掉过严断言,中文名按 UTF-8 计长(常用于分配
    // getCString: 缓冲)不再 panic。
    // [扫描修 2026-09-15] F8-4:原来 5 种白名单之外的编码 unimplemented! panic。现在按真实编码算字节数
    // (不含 NUL、不含 BOM);无法表示时按真机语义返回 0。游戏 5 处调用
    // (AvatarLayer/InviteFriendsLayer/RegisterView calculateTextNumber:、NetworkManager 漂流瓶/乌鸦祭司)
    // 全是 movs r2,#4 = UTF-8,结果与原来逐字节一致。
    let string = to_rust_string(env, this);
    match encode_str(&string, encoding, false) {
        Some(bytes) => bytes.len().try_into().unwrap(),
        None => 0,
    }
}

- (NSRange)rangeOfString:(id)search_string {
    msg![env; this rangeOfString:search_string options:0u32]
}

- (NSRange)rangeOfString:(id)search_string
                 options:(NSStringCompareOptions)options { // NSString *
    log_dbg!(
        "[(NSString *){} rangeOfString:{} options:{}]",
        to_rust_string(env, this), to_rust_string(env, search_string), options
    );
    // [扫描修 2026-09-15] F8-4:原来按 options 精确值 match(0/2、1、4),组合位与 NSAnchoredSearch
    // 直接 unimplemented! panic。现在按位解析,见 range_of_string_common / find_code_units。
    let len: NSUInteger = msg![env; this length];
    range_of_string_common(env, this, search_string, options, NSRange { location: 0, length: len })
}

// [扫描修 2026-09-15] F8-4:原来缺这个方法(SDK 的 iRate/TaomeeRate/TaomeeVersion/FlurryUtil 会调用)。
- (NSRange)rangeOfString:(id)search_string // NSString *
                 options:(NSStringCompareOptions)options
                   range:(NSRange)range {
    range_of_string_common(env, this, search_string, options, range)
}

- (id)description {
    this
}
// TODO: debugDescription, localized description (is that a thing for NSString?)

- (NSUInteger)hash {
    // TODO: avoid copying
    super::hash_helper(&to_rust_string(env, this))
}
- (bool)isEqual:(id)other {
    if this == other {
        return true;
    }
    let class: Class = msg_class![env; NSString class];
    if !msg![env; other isKindOfClass:class] {
        return false;
    }
    // TODO: avoid copying
    to_rust_string(env, this) == to_rust_string(env, other)
}
- (bool)isEqualToString:(id)other { // NSString*
    if this == other {
        return true;
    }
    if other == nil {
        return false;
    }
    // TODO: avoid copying
    to_rust_string(env, this) == to_rust_string(env, other)
}

- (bool)hasPrefix:(id)str { // NSString*
    // TODO: avoid copying
    let str = to_rust_string(env, str).to_string();
    to_rust_string(env, this).starts_with(&str)
}

- (bool)hasSuffix:(id)str { // NSString*
    // TODO: avoid copying
    let str = to_rust_string(env, str).to_string();
    to_rust_string(env, this).ends_with(&str)
}

- (NSComparisonResult)localizedCompare:(id)other { // NSString*
    // TODO: use current locale
    // TODO: support `compatibility equivalence` in the Unicode standard
    // More info: https://www.objc.io/issues/9-strings/unicode/
    // [扫描修 2026-09-15] F8-4:原来两个 assert 要求全 ASCII,中文串一比较就整机 panic。
    // 本地化排序规则仍未实现,退化为字面比较(ASCII 输入的结果与原来一致)。
    msg![env; this compare:other]
}

- (NSComparisonResult)compare:(id)other { // NSString*
    msg![env; this compare:other options:NSLiteralSearch]
}

- (NSComparisonResult)caseInsensitiveCompare:(id)other { //NSString*
    msg![env; this compare:other options:NSCaseInsensitiveSearch]
}

- (NSComparisonResult)compare:(id)other // NSString*
                      options:(NSStringCompareOptions)options
                        range:(NSRange)range {
    // TODO: avoid substring copying
    let substr = msg![env; this substringWithRange:range];
    msg![env; substr compare:other options:options]
}

- (NSComparisonResult)compare:(id)other options:(NSStringCompareOptions)mask { // NSString*
    // [扫描修 2026-09-15] F8-4:原来按 mask 精确值 match(0/2、1、64),组合位(如 0x41、0x44)
    // unimplemented! panic;遇到代理项 "Invalid chars" panic;other 为 nil 时 assert panic;
    // 数值比较用 u32 累加会溢出。现在按位解析,核心在 compare_code_units。
    // 游戏自己的调用 -[WrapperManager showWeather]@0x19ae76 用 NSNumericSearch(0x40),结果与原来一致。
    if other == nil {
        log!(
            "Warning: [(NSString*){:?} compare:nil options:{:#x}] 参数为 nil(真机未定义),返回 NSOrderedDescending",
            this,
            mask
        );
        return NSOrderedDescending;
    }
    let opts = compare_options_logged(mask, "compare:options:");
    let a = collect_code_units(env, this);
    let b = collect_code_units(env, other);
    from_rust_ordering(compare_code_units(&a, &b, opts))
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

// NSMutableCopying implementation
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    let str_mut: id = msg_class![env; NSMutableString alloc];
    // TODO: use `initWithString:`
    let str_mut: id = msg![env; str_mut init];
    () = msg![env; str_mut setString:this];
    str_mut
}

- (bool)getFileSystemRepresentation:(MutPtr<u8>)buffer
                          maxLength:(NSUInteger)buffer_size {
    msg![env; this getCString:buffer
                    maxLength:buffer_size
                     encoding:NSUTF8StringEncoding]
}

- (bool)getCString:(MutPtr<u8>)buffer
         maxLength:(NSUInteger)buffer_size
          encoding:(NSStringEncoding)encoding {
    get_bytes_buffer_inner(env, this, buffer, buffer_size, encoding, true)
}
- (())getCString:(MutPtr<u8>)buffer {
    let encoding: NSStringEncoding = msg_class![env; NSString defaultCStringEncoding];

    // Prevent slice out-of-range error
    let length = (u32::MAX - buffer.to_bits()).min(NSMaximumStringLength);
    let res: bool = msg![env; this getCString:buffer
                                    maxLength:length
                                     encoding:encoding];
    assert!(res);
}

- (id)componentsSeparatedByString:(id)separator { // NSString*
    // TODO: support foreign subclasses (perhaps via a helper function that
    // copies the string first)
    let mut main_iter = env.objc.borrow::<StringHostObject>(this)
        .iter_code_units();
    let sep_iter = env.objc.borrow::<StringHostObject>(separator)
        .iter_code_units();

    // TODO: zero-length separator support
    assert!(sep_iter.clone().next().is_some());

    let mut components = Vec::<Utf16String>::new();
    let mut current_component: Utf16String = Vec::new();
    loop {
        if let Some(new_main_iter) = main_iter.strip_prefix(&sep_iter, /* case_insensitive: */ false) {
            // matched separator, end current component
            components.push(std::mem::take(&mut current_component));
            main_iter = new_main_iter;
        } else {
            // no separator match, extend the current component
            match main_iter.next() {
                Some(cur) => current_component.push(cur),
                None => break,
            }
        }
    }
    components.push(current_component);

    // TODO: For a foreign subclass of NSString, do we have to return that
    // subclass? The signature implies this isn't the case and it's probably not
    // worth the effort, but it's an interesting question.
    let class = env.objc.get_known_class("_touchHLE_NSString", &mut env.mem);

    let component_ns_strings = components.drain(..).map(|utf16| {
        let host_object = Box::new(StringHostObject::Utf16(utf16));
        env.objc.alloc_object(class, host_object, &mut env.mem)
    }).collect();
    let array = ns_array::from_vec(env, component_ns_strings);
    autorelease(env, array)
}

- (id)initWithData:(id)data // NSData *
          encoding:(NSStringEncoding)encoding {
    // Defined on the ABSTRACT NSString (not the concrete _touchHLE_NSString) so BOTH concrete string
    // subclasses — _touchHLE_NSString AND _touchHLE_NSMutableString — inherit it. The game decodes its
    // HTTP serverlist body via [[NSMutableString alloc] initWithData:encoding:NSUTF8StringEncoding];
    // while this lived on _touchHLE_NSString only, the mutable subclass did NOT respond to the selector
    // → no-op'd to nil → the serverlist string was empty → the parse produced zero servers →
    // -[NetworkManager onServerListResult:] got a3=0 → "Error connecting to server" → entermainmenu,
    // which in this port stops the render run loop (village builds but never paints). Both concrete
    // subclasses implement initWithBytes:length:encoding:, so this dispatches correctly for each.
    let bytes: ConstVoidPtr = msg![env; data bytes];
    let bytes: ConstPtr<u8> = bytes.cast();
    let length: NSUInteger = msg![env; data length];
    msg![env; this initWithBytes:bytes length:length encoding:encoding]
}

- (())getCharacters:(MutPtr<unichar>)buffer
              range:(NSRange)range {
    // TODO: avoid copying
    let ranged = msg![env; this substringWithRange:range];
    msg![env; ranged getCharacters:buffer]
}

- (())getCharacters:(MutPtr<unichar>)buffer {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);

    // this conversion maybe not most optimal heuristic
    let (utf16, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} getCharacters:{:?}]: converted string to UTF-16", this, buffer);
    }

    let len: GuestUSize = guest_size_of::<unichar>() * utf16.len() as GuestUSize;
    let tmp_vec: Vec<u8> = utf16.iter().flat_map(|c| u16::to_le_bytes(*c)).collect();
    _ = env.mem.bytes_at_mut(buffer.cast(), len).write(tmp_vec.as_slice()).unwrap();
}

- (ConstPtr<u8>)cStringUsingEncoding:(NSStringEncoding)encoding {
    // [扫描修 2026-09-15] F8-4:原来 ASCII/MacRoman/Latin-1 遇到非 ASCII 字符 assert panic,
    // 其它编码 unimplemented! panic。现在按真实编码转换;无法无损表示或编码未实现时按真机语义返回 NULL。
    // UTF-8(UTF8String 走这里)的输出与原来逐字节一致。
    // TODO: avoid copying
    let string = to_rust_string(env, this);
    let Some(bytes) = encode_str(&string, encoding, false) else {
        warn_once(format!("cstring-encode:{encoding:#x}"), || {
            format!("Warning: cStringUsingEncoding:{encoding:#x} 无法表示该字符串或编码未实现,返回 NULL(真机语义;原来这里 panic)")
        });
        return Ptr::null();
    };
    let null_size = nul_terminator_size(encoding);
    let bytes_size = bytes.len() as GuestUSize;
    let total_size: GuestUSize = bytes_size + null_size;
    let c_string: MutPtr<u8> = env.mem.alloc(total_size).cast();
    let dest = env.mem.bytes_at_mut(c_string, total_size);
    dest[..bytes.len()].copy_from_slice(&bytes);
    dest[bytes.len()..].fill(0);
    // NSData will handle releasing the string (it is autoreleased)
    let _: id = msg_class![env; NSData dataWithBytesNoCopy:(c_string.cast_void())
                                                    length:total_size];
    c_string.cast_const()
}

- (ConstPtr<u8>)cString {
    // TODO: use default C-string encoding of the current locale
    // TODO: raise NSCharacterConversionException if couldn't represent
    msg![env; this UTF8String]
}

- (ConstPtr<u8>)UTF8String {
    msg![env; this cStringUsingEncoding:NSUTF8StringEncoding]
}

- (id)substringToIndex:(NSUInteger)to {
    let mut res_utf16: Utf16String = Vec::with_capacity(to as usize);

    for_each_code_unit(env, this, |idx, c| {
        if idx < to {
            res_utf16.push(c);
        }
    });

    let res = msg_class![env; _touchHLE_NSString alloc];
    *env.objc.borrow_mut(res) = StringHostObject::Utf16(res_utf16);
    autorelease(env, res)
}

- (id)substringFromIndex:(NSUInteger)from {
    let mut res_utf16: Utf16String = Vec::with_capacity(from as usize);

    for_each_code_unit(env, this, |idx, c| {
        if idx >= from {
            res_utf16.push(c);
        }
    });

    let res = msg_class![env; _touchHLE_NSString alloc];
    *env.objc.borrow_mut(res) = StringHostObject::Utf16(res_utf16);
    autorelease(env, res)
}

- (id)stringByTrimmingCharactersInSet:(id)set { // NSCharacterSet*
    let initial_length: NSUInteger = msg![env; this length];

    let mut res_start: NSUInteger = 0;
    let mut res_end = initial_length;

    while res_start < initial_length {
        let c: u16 = msg![env; this characterAtIndex:res_start];
        if msg![env; set characterIsMember:c] {
            res_start += 1;
        } else {
            break;
        }
    }

    while res_end > res_start {
        let c: u16 = msg![env; this characterAtIndex:(res_end - 1)];
        if msg![env; set characterIsMember:c] {
            res_end -= 1;
        } else {
            break;
        }
    }

    assert!(res_end >= res_start);
    let res_length = res_end - res_start;

    if res_length == initial_length {
        let ret = msg![env; this copy];
        autorelease(env, ret)
    } else {
        let range = NSRange{ location: res_start, length: res_length };
        let string: id = msg![env; this substringWithRange:range];
        string
    }
}

- (id)stringByReplacingOccurrencesOfString:(id)target // NSString*
                                withString:(id)replacement { // NSString*
    let length: NSUInteger = msg![env; this length];
    let range = NSRange { location: 0, length };
    msg![env; this stringByReplacingOccurrencesOfString:target
                                             withString:replacement
                                                options:0u32
                                                  range:range]
}

- (id)stringByReplacingOccurrencesOfString:(id)target // NSString*
                                withString:(id)replacement // NSString*
                                   options:(NSStringCompareOptions)options
                                     range:(NSRange)range {
    let loc = range.location;
    let len = range.length;
    let left: id = msg![env; this substringToIndex:loc];
    let middle: id = msg![env; this substringWithRange:range];
    let right: id = msg![env; this substringFromIndex:(loc + len)];
    let new_middle: id = string_by_replacing_occurrences_inner(env, middle, target, replacement, options);
    let res: id = msg![env; left stringByAppendingString:new_middle];
    msg![env; res stringByAppendingString:right]
}

- (id)stringByAppendingString:(id)other { // NSString*
    assert!(other != nil); // TODO: raise exception

    // TODO: ideally, don't convert to UTF-16 here
    let this_len: NSUInteger = msg![env; this length];
    let other_len: NSUInteger = msg![env; other length];
    let mut new_utf16 = Vec::with_capacity((this_len + other_len) as usize);
    for_each_code_unit(env, this, |_idx, c| {
        new_utf16.push(c);
    });
    for_each_code_unit(env, other, |_idx, c| {
        new_utf16.push(c);
    });

    // TODO: For a foreign subclass of NSString, do we have to return that
    // subclass? The signature implies this isn't the case and it's probably not
    // worth the effort, but it's an interesting question.
    let class = env.objc.get_known_class("_touchHLE_NSString", &mut env.mem);
    let host_object = Box::new(StringHostObject::Utf16(new_utf16));
    env.objc.alloc_object(class, host_object, &mut env.mem)
}

- (id)stringByAppendingFormat:(id)format, ...args {
    let new_string = with_format(env, format,  args.start());
    let new_string = from_rust_string(env, new_string);
    let new_string = msg![env; this stringByAppendingString:new_string];
    autorelease(env, new_string)
}

- (id)stringByDeletingLastPathComponent {
    let string = to_rust_string(env, this); // TODO: avoid copying
    let (res, _) = path_algorithms::split_last_path_component(&string);
    let new_string = from_rust_string(env, String::from(res));
    autorelease(env, new_string)
}

- (id)lastPathComponent {
    let string = to_rust_string(env, this); // TODO: avoid copying
    let (_, res) = path_algorithms::split_last_path_component(&string);
    let new_string = from_rust_string(env, String::from(res));
    autorelease(env, new_string)
}

- (bool)isAbsolutePath {
    // Defined on the public NSString so all subclasses (incl.
    // _touchHLE_NSMutableString) inherit it.
    let path = to_rust_string(env, this);
    path.starts_with('/') || path.starts_with('~')
}

- (id)pathComponents {
    let string = to_rust_string(env, this); // TODO: avoid copying
    let vec = path_algorithms::split_path_components(&string);
    let vec = vec.iter().map(|component| {
        from_rust_string(env, component.to_string())
    }).collect();
    let array = ns_array::from_vec(env, vec);
    autorelease(env, array)
}

- (id)stringByDeletingPathExtension {
    let string = to_rust_string(env, this); // TODO: avoid copying
    let (res, _) = path_algorithms::split_path_extension(&string);
    let new_string = from_rust_string(env, String::from(res));
    autorelease(env, new_string)
}

- (id)pathExtension {
    let string = to_rust_string(env, this); // TODO: avoid copying
    let (_, res) = path_algorithms::split_path_extension(&string);
    let new_string = from_rust_string(env, String::from(res));
    autorelease(env, new_string)
}

- (ConstPtr<u8>)fileSystemRepresentation {
    let file_manager: id = msg_class![env; NSFileManager defaultManager];
    // This behavior was confirmed on the iOS Simulator
    msg![env; file_manager fileSystemRepresentationWithPath:this]
}

- (id)stringByAddingPercentEscapesUsingEncoding:(NSStringEncoding)encoding {
    // [扫描修 2026-09-15] F8-4:原来 assert 编码只能是 ASCII/UTF-8、字符只能是 URL 合法字符,
    // URL 里带空格、中文或 [ ] 就整机 panic(iMoleVillageAppDelegate、TSMutableString、Request 等都会调)。
    // 现在把字符串按指定编码转成字节,不在原放行集合里的字节写成 %XX(大写十六进制)。
    // 放行集合与原 assert 完全相同:unreserved(字母数字 -_.~)与 reserved(!*'();:@&=+$,/?%#),
    // 所以原来不崩的输入结果不变(仍返回副本);真机同样会转义空格、[ ]、非 ASCII 等。
    // 无法按该编码表示时返回 nil(真机语义)。
    const KEEP: &[u8] = b"-_.~!*'();:@&=+$,/?%#";
    let keep = |byte: u8| byte.is_ascii_alphanumeric() || KEEP.contains(&byte);
    let string = to_rust_string(env, this);
    let Some(bytes) = encode_str(&string, encoding, false) else {
        warn_once(format!("percent-escape:{encoding:#x}"), || {
            format!("Warning: stringByAddingPercentEscapesUsingEncoding:{encoding:#x} 无法按该编码表示字符串,返回 nil")
        });
        return nil;
    };
    if bytes.iter().all(|&byte| keep(byte)) {
        let new: id = msg![env; this copy];
        return autorelease(env, new);
    }
    let mut escaped = String::with_capacity(bytes.len() * 3);
    for &byte in &bytes {
        if keep(byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    let new = from_rust_string(env, escaped);
    autorelease(env, new)
}

- (id)stringByAppendingPathComponent:(id)component { // NSString*
    // TODO: avoid copying
    let base_str = to_rust_string(env, this);
    let component_str = to_rust_string(env, component);
    let res = path_algorithms::string_by_appending_path_component(&base_str, &component_str);
    log_dbg!("'{}' + '{}' -> '{}'", base_str, component_str, res);
    let new_string = from_rust_string(env, res);
    autorelease(env, new_string)
}

- (id)stringByAppendingPathExtension:(id)extension { // NSString*
    // FIXME: handle edge cases like trailing '/' (may differ from Rust!)
    let mut combined = to_rust_string(env, this).into_owned();
    // TODO: avoid copying
    let extension_string = to_rust_string(env, extension);
    if !extension_string.is_empty(){
        combined.push('.');
        combined.push_str(&extension_string);
    }

    let new_string = from_rust_string(env, combined);
    autorelease(env, new_string)
}

- (id)stringByExpandingTildeInPath {
    let path = to_rust_string(env, this);

    let new_path_str = if let Some(new_path) = path.strip_prefix('~') {
        // ~ and anything up until the first / is stripped
        // This was confirmed using a test app on iOS
        // Examples (of what is placed after home directory):
        //  "~"            -> ""
        //  "~/"           -> ""
        //  "~user"        -> ""
        //  "~/Documents"  -> "/Documents"
        //  "~foo/bar"     -> "/bar"
        //  "~~foo/bar"    -> "/bar"
        let within_home_dir = new_path.split_once('/').map(|x| x.1).unwrap_or("");

        let guest_path = env.fs.home_directory().join(within_home_dir);
        let resolved = fs::resolve_path(&guest_path, None);
        format!("/{}", resolved.join("/"))
    } else {
        // If called on a path with no leading ~ do nothing
        path.to_string()
    };

    log_dbg!("[(NSString *){:?} stringByExpandingTildeInPath] {} -> {}", this, path, new_path_str);

    let new_string = from_rust_string(env, new_path_str);
    autorelease(env, new_string)
}

- (id)stringByStandardizingPath {
    let expanded: id = msg![env; this stringByExpandingTildeInPath];
    let path = to_rust_string(env, expanded); // TODO: avoid copying
    // TODO: Removing an initial component of "/private/var/automount",
    //       "/var/automount”, or "/private” from the path
    assert!(!path.starts_with("/private"));
    assert!(!path.starts_with("/var/automount"));
    // TODO: Reducing empty components and references to the current directory
    assert!(!path.contains("//"));
    assert!(!path.contains("/./"));
    // Removing a trailing slash from the last component.
    let path = path_algorithms::trim_trailing_slashes(&path);
    // For absolute paths only, resolve references to the parent directory
    let new_path_str = if path.starts_with('/') {
        assert!(!path.starts_with("/.."));
        // Note: while we are using fs function, it's just string manipulation
        // here.
        let resolved = fs::resolve_path(GuestPath::new(path), None);
        let new_path = format!("/{}", resolved.join("/"));
        assert!(!new_path.contains(".."));
        new_path
    } else {
        String::from(path)
    };
    log_dbg!("[(NSString *){:?} stringByStandardizingPath] {} -> {}", this, to_rust_string(env, this), new_path_str);
    let new_string = from_rust_string(env, new_path_str);
    autorelease(env, new_string)
}

- (id)stringsByAppendingPaths:(id)paths {
    let count: NSUInteger = msg![env; paths count];
    let mut_arr: id = msg_class![env; NSMutableArray new];
    for i in 0..count {
        let path: id = msg![env; paths objectAtIndex:i];
        let new: id = msg![env; this stringByAppendingPathComponent:path];
        () = msg![env; mut_arr addObject:new];
    }
    let arr = msg![env; mut_arr copy];
    release(env, mut_arr);
    autorelease(env, arr)
}

// These come from a category in UIKit (UIStringDrawing).
// TODO: Implement categories so we can completely move the code to UIFont.
// TODO: More `sizeWithFont:` variants
- (CGSize)sizeWithFont:(id)font { // UIFont*
    // TODO: avoid copy
    let text = to_rust_string(env, this);
    ui_font::size_with_font(env, font, &text, None)
}
- (CGSize)sizeWithFont:(id)font // UIFont*
     constrainedToSize:(CGSize)size {
    msg![env; this sizeWithFont:font
              constrainedToSize:size
                  lineBreakMode:UILineBreakModeWordWrap]
}
- (CGSize)sizeWithFont:(id)font // UIFont*
     constrainedToSize:(CGSize)size
         lineBreakMode:(UILineBreakMode)line_break_mode {
    // TODO: avoid copy
    let text = to_rust_string(env, this);
    ui_font::size_with_font(env, font, &text, Some((size, line_break_mode)))
}

- (CGSize)drawAtPoint:(CGPoint)point
             withFont:(id)font { // UIFont*
    // TODO: avoid copy
    let text = to_rust_string(env, this);
    ui_font::draw_at_point(env, font, &text, point, None)
}

- (CGSize)drawAtPoint:(CGPoint)point
             forWidth:(CGFloat)width
             withFont:(id)font // UIFont*
        lineBreakMode:(UILineBreakMode)line_break_mode {
    // TODO: avoid copy
    let text = to_rust_string(env, this);
    ui_font::draw_at_point(env, font, &text, point, Some((width, line_break_mode)))
}

- (CGSize)drawInRect:(CGRect)rect
            withFont:(id)font { // UIFont*
    msg![env; this drawInRect:rect
                     withFont:font
                lineBreakMode:UILineBreakModeWordWrap
                    alignment:UITextAlignmentLeft]
}
- (CGSize)drawInRect:(CGRect)rect
            withFont:(id)font // UIFont*
       lineBreakMode:(UILineBreakMode)line_break_mode {
    msg![env; this drawInRect:rect
                     withFont:font
                lineBreakMode:line_break_mode
                    alignment:UITextAlignmentLeft]
}
- (CGSize)drawInRect:(CGRect)rect
            withFont:(id)font // UIFont*
       lineBreakMode:(UILineBreakMode)line_break_mode
           alignment:(UITextAlignment)align {
    // TODO: avoid copy
    let text = to_rust_string(env, this);
    ui_font::draw_in_rect(env, font, &text, rect, line_break_mode, align)
}

- (bool)writeToFile:(id)path // NSString*
         atomically:(bool)use_aux_file {
    let encoding: NSStringEncoding = msg_class![env; NSString defaultCStringEncoding];
    let error: MutPtr<id> = Ptr::null();
    msg![env; this writeToFile:path atomically:use_aux_file encoding:encoding error:error]
}

- (bool)writeToFile:(id)path // NSString*
         atomically:(bool)use_aux_file
           encoding:(NSStringEncoding)encoding
              error:(MutPtr<id>)error { // NSError**
    // [扫描修 2026-09-15] F8-4:原来 assert 编码只能是 UTF-8/ASCII,写失败且传了 error 时 todo!() panic。
    // 现在按真实编码写(通用 UTF-16/32 带 BOM);编码失败或写盘失败返回 NO,error 写 nil(尚未构造 NSError)。
    // UTF-8/ASCII 的输出与原来逐字节一致。
    if !error.is_null() {
        env.mem.write(error, nil);
    }
    let string = to_rust_string(env, this);
    let Some(bytes) = encode_for_data(&string, encoding, false) else {
        warn_once(format!("write-encode:{encoding:#x}"), || {
            format!("Warning: writeToFile:atomically:encoding:{encoding:#x} 无法按该编码表示字符串,返回 NO")
        });
        return false;
    };
    let c_string = env.mem.alloc_and_write_cstr(&bytes);
    // This should not include a NULL terminator!
    let length: NSUInteger = bytes.len().try_into().unwrap();
    // NSData will handle releasing the string (it is autoreleased)
    let data: id = msg_class![env; NSData dataWithBytesNoCopy:(c_string.cast_void())
                                                    length:length];

    // TODO: write extended attributes about text encoding
    msg![env; data writeToFile:path atomically:use_aux_file]
}

- (f32)floatValue {
    float_value_common(env, this)
}
- (f64)doubleValue {
    float_value_common(env, this)
}

- (NSInteger)integerValue {
    msg![env; this intValue]
}
- (i64)longLongValue {
    // Same leading-number parse as intValue, but 64-bit. Games parse numeric
    // IDs/timestamps out of strings (e.g. from config) via longLongValue.
    let st = to_rust_string(env, this);
    let st = st.trim_start();
    let mut cutoff = st.len();
    for (i, c) in st.char_indices() {
        if !c.is_ascii_digit() && c != '+' && c != '-' {
            cutoff = i;
            break;
        }
    }
    st[..cutoff].parse().unwrap_or(0)
}
- (i32)intValue {
    let st = to_rust_string(env, this);
    let st = st.trim_start();
    let mut cutoff = st.len();
    for (i, c) in st.char_indices() {
        if !c.is_ascii_digit() && c != '+' && c != '-' {
            cutoff = i;
            break;
        }
    }
    // TODO: handle over/underflow properly
    st[..cutoff].parse().unwrap_or(0)
}

- (id)lowercaseString {
    // TODO: check if rust methods are consistent with ObjC one
    let str = to_rust_string(env, this).to_lowercase();
    let res = from_rust_string(env, str);
    autorelease(env, res)
}

- (id)uppercaseString {
    // TODO: check if rust methods are consistent with ObjC one
    let str = to_rust_string(env, this).to_uppercase();
    let res = from_rust_string(env, str);
    autorelease(env, res)
}

@end

// NSMutableString is an abstract class. A subclass must everything
// NSString provides, plus:
// - (void)replaceCharactersInRange:(NSRange)range withString:(NSString)string;
// Note that it inherits from NSString, so we must ensure we override any
// default methods that would be inappropriate for mutability.
@implementation NSMutableString: NSString

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSMutableString might be subclassed by something
    // which needs allocWithZone: to have the normal behaviour.
    // Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSMutableString", &mut env.mem));
    msg_class![env; _touchHLE_NSMutableString allocWithZone:zone]
}

+ (id)stringWithCapacity:(NSUInteger)capacity {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCapacity:capacity];
    autorelease(env, new)
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    let new: id = msg_class![env; NSString alloc];
    msg![env; new initWithString:this]
}

- (())appendString:(id)a_string { // NSString*
    assert_ne!(a_string, nil);
    // TODO: this is inefficient? append in place instead
    let new: id = msg![env; this stringByAppendingString:a_string];
    () = msg![env; this setString:new];
}

- (())deleteCharactersInRange:(NSRange)range {
    let location = range.location;
    let length = range.length;

    let left: id = if location == 0 {
        get_static_str(env, "")
    } else {
        let left_range = NSRange {
            location: 0,
            length: location,
        };
        msg![env; this substringWithRange:left_range]
    };

    let idx_after_removal = location + length;
    let lenght_str: NSUInteger = msg![env; this length];
    let right: id = if idx_after_removal == lenght_str {
        get_static_str(env, "")
    } else {
        let right_range = NSRange {
            location: idx_after_removal,
            length: lenght_str - idx_after_removal,
        };
        msg![env; this substringWithRange:right_range]
    };

    let res: id = msg![env; left stringByAppendingString:right];
    () = msg![env; this setString:res];
}

// [扫描修 2026-09-15] F8-4:原来 NSMutableString 完全没有这个方法(消息发过来只打"不响应"然后空操作)。
// 游戏 -[HttpManager asynchronousHttpServerlist]@0x1954d6 用它删掉服务器列表正文里的 "\r"
// (options=2 NSLiteralSearch)。私服钩子返回的正文里本来就没有 \r,结果不变;
// 真服务器用 CRLF 换行时现在也能正确清洗。其余调用者在 SDK 里(immobUtils、AppCommunicate、ASINetworkQueue 等)。
// 返回替换次数。
- (NSUInteger)replaceOccurrencesOfString:(id)target // NSString*
                              withString:(id)replacement // NSString*
                                 options:(NSStringCompareOptions)options
                                   range:(NSRange)range {
    if target == nil || replacement == nil {
        log!(
            "Warning: [(NSMutableString*){:?} replaceOccurrencesOfString:{:?} withString:{:?}] 参数为 nil(真机抛 NSInvalidArgumentException),不做替换",
            this,
            target,
            replacement
        );
        return 0;
    }
    let whole = collect_code_units(env, this);
    let target_units = collect_code_units(env, target);
    let replacement_units = collect_code_units(env, replacement);
    // NSRange 是 packed 结构体,先拷到局部变量再用
    let (location, length) = (range.location as usize, range.length as usize);
    let start = location.min(whole.len());
    let end = location.saturating_add(length).min(whole.len());
    if location.saturating_add(length) > whole.len() {
        log!(
            "Warning: [(NSMutableString*){:?} replaceOccurrencesOfString:… range:{{{}, {}}}] 越界(长度 {},真机抛 NSRangeException),裁剪后替换",
            this,
            location,
            length,
            whole.len()
        );
    }
    let opts = compare_options_logged(options, "replaceOccurrencesOfString:withString:options:range:");
    let (middle, count) = replace_code_units(&whole[start..end], &target_units, &replacement_units, opts);
    if count == 0 {
        return 0;
    }
    let mut result = Vec::with_capacity(start + middle.len() + (whole.len() - end));
    result.extend_from_slice(&whole[..start]);
    result.extend_from_slice(&middle);
    result.extend_from_slice(&whole[end..]);
    let new_string = from_u16_vec(env, result);
    () = msg![env; this setString:new_string];
    release(env, new_string);
    count.try_into().unwrap()
}

@end

// Our private subclass that is the single implementation of NSString for the
// time being.
@implementation _touchHLE_NSString: NSString

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(StringHostObject::Utf8(Cow::Borrowed("")));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// TODO: more init methods

// NSCoding(initWithCoder:/encodeWithCoder:)已上移到抽象 NSString,见该类。

- (id)initWithFormat:(id)format, // NSString*
                     ...args {
    init_with_format_inner(env, this, format, args.start())
}

- (id)initWithFormat:(id)format // NSString*
           arguments:(VaList)args {
    init_with_format_inner(env, this, format, args)
}

- (id)initWithBytes:(ConstPtr<u8>)bytes
             length:(NSUInteger)len
           encoding:(NSStringEncoding)encoding {
    // [扫描修 2026-09-15] F8-4:长度 0 时不碰内存(bytes 可以是 NULL,原来 bytes_at 在空页 panic);
    // 编码无法解码时按真机语义释放自身、返回 nil(原来 panic)。
    if len == 0 {
        *env.objc.borrow_mut(this) = StringHostObject::Utf8(Cow::Borrowed(""));
        return this;
    }
    let slice = env.mem.bytes_at(bytes, len);
    let Some(host_object) = StringHostObject::decode(Cow::Borrowed(slice), encoding) else {
        release(env, this);
        return nil;
    };

    *env.objc.borrow_mut(this) = host_object;

    this
}

- (id)initWithCharacters:(ConstPtr<unichar>)characters length:(NSUInteger)len {
    // [扫描修 2026-09-15] F8-4:unichar 缓冲是 guest 内存里的小端 UTF-16 码元,按小端原样解码。
    // 原来传 NSUTF16StringEncoding:现在该编码会识别并剥掉 BOM,首个字符恰好是 U+FEFF/U+FFFE 时会被误吞或误换字节序。
    // 长度 0 时允许 characters 为 NULL(真机返回空串,原来 assert panic)。
    assert!(!characters.is_null() || len == 0);
    let num_bytes = len * 2;
    msg![env; this initWithBytes:(characters.cast::<u8>())
                          length:num_bytes
                        encoding:NSUTF16LittleEndianStringEncoding]
}

- (id)initWithString:(id)string { // NSString *
    // TODO: optimize for more common cases (or maybe just call copy?)
    let mut code_units = Vec::new();
    for_each_code_unit(env, string, |_, c| code_units.push(c));
    *env.objc.borrow_mut(this) = StringHostObject::Utf16(code_units);
    this
}

- (id)initWithContentsOfFile:(id)path { // NSString*
    if path == nil {
        return nil;
    }
    // TODO: avoid copy?
    let path = to_rust_string(env, path);
    let Ok(bytes) = env.fs.read(GuestPath::new(&path)) else {
        return nil;
    };
    let len = bytes.len();

    let encoding = if len > 1 && (bytes[..2] == [0xFE, 0xFF] || bytes[..2] == [0xFF, 0xFE]) {
        NSUTF16StringEncoding
    } else if len > 2 && bytes[..3] == [0xEF, 0xBB, 0xBF] {
        NSUTF8StringEncoding
    } else {
        msg_class![env; NSString defaultCStringEncoding]
    };

    let Some(host_object) = StringHostObject::decode(Cow::Owned(bytes), encoding) else {
        release(env, this);
        return nil;
    };
    *env.objc.borrow_mut(this) = host_object;
    this
}

- (id)initWithContentsOfFile:(id)path // NSString*
                    encoding:(NSStringEncoding)encoding
                       error:(MutPtr<id>)error { // NSError**
    // TODO: avoid copy?
    let path = to_rust_string(env, path);
    let Ok(bytes) = env.fs.read(GuestPath::new(&path)) else {
        // [扫描修 2026-09-15] 原来 assert!(error.is_null()):调用方传了 error 指针且文件不存在就 panic。
        // 现在 error 写 nil(尚未构造 NSError),释放自身返回 nil。
        if !error.is_null() {
            env.mem.write(error, nil);
        }
        release(env, this);
        return nil;
    };

    // [扫描修 2026-09-15] F8-4:编码无法解码时返回 nil(原来 panic)
    let Some(host_object) = StringHostObject::decode(Cow::Owned(bytes), encoding) else {
        if !error.is_null() {
            env.mem.write(error, nil);
        }
        release(env, this);
        return nil;
    };

    *env.objc.borrow_mut(this) = host_object;

    this
}

- (bool)isAbsolutePath {
    // TODO: avoid copy?
    let path = to_rust_string(env, this);
    path.starts_with('/') || path.starts_with('~')
}


- (bool)boolValue {
    let string = to_rust_string(env, this);
    let string = string.trim_start_matches(|c: char| {
        c.is_ascii_whitespace() || c == '-' || c == '+' || c == '0'
    });

    let matching_values = "YyTt123456789";
    string.chars()
        .next()
        .map(|c| matching_values.contains(c))
        .unwrap_or(false)
}

- (id)dataUsingEncoding:(NSStringEncoding)encoding
   allowLossyConversion:(bool)lossy {
    data_using_encoding_lossy_inner(env, this, encoding, lossy)
}

- (id)componentsSeparatedByCharactersInSet:(id)cset { // NSCharacterSet*
    let string = {
        let host_object = env.objc.borrow_mut::<StringHostObject>(this);
        let (orig_string, did_convert) = host_object.convert_to_utf16_inplace();
        if did_convert {
            log_dbg!("[{:?} componentsSeparatedByCharactersInSet]: converted string to UTF-16", this);
        }
        orig_string.clone()
    };

    let substrings: Vec<&[u16]> = {
        string.split(|&c| msg![env; cset characterIsMember:c]).collect()
    };

    let substrings: Vec<id> = substrings.into_iter().map(|substr| {
        from_u16_vec(env, substr.to_vec())
    }).collect();

    let res = ns_array::from_vec(env, substrings);
    autorelease(env, res)
}

- (id)substringWithRange:(NSRange)range {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);
    let (orig_string, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} substringWithRange]: converted string to UTF-16", this);
    }
    let host_string =
        orig_string[(range.location as usize)..((range.location + range.length) as usize)].to_vec();
    let res = from_u16_vec(env, host_string);
    autorelease(env, res)
}

- (NSRange)lineRangeForRange:(NSRange)range {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);
    let (orig_string, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} lineRangeForRange]: converted string to UTF-16", this);
    }
    let (start, end, _) = line_range_helper(orig_string, range, true, true);
    NSRange { location: start, length: end - start }
}

- (())getLineStart:(MutPtr<NSUInteger>)start_ptr
               end:(MutPtr<NSUInteger>)end_ptr
       contentsEnd:(MutPtr<NSUInteger>)contents_end_ptr
          forRange:(NSRange)range {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);
    let (orig_string, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} getLineStart]: converted string to UTF-16", this);
    }

    let get_start = !start_ptr.is_null();
    let get_end = !end_ptr.is_null() || !contents_end_ptr.is_null();
    let (start, end, contents_end) = line_range_helper(orig_string, range, get_start, get_end);

    if !start_ptr.is_null() {
        env.mem.write(start_ptr, start);
    }

    if !end_ptr.is_null() {
        env.mem.write(end_ptr, end);
    }

    if !contents_end_ptr.is_null() {
        env.mem.write(contents_end_ptr, contents_end);
    }
}
@end

// Specialised subclass for static-lifetime strings.
// See `get_static_str`.
@implementation _touchHLE_NSString_Static: _touchHLE_NSString

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(StringHostObject::Utf8(Cow::Borrowed("")));
    env.objc.alloc_static_object(this, host_object, &mut env.mem)
}

- (id) retain { this }
- (()) release {}
- (id) autorelease { this }

@end

// Specialised subclasses for static-lifetime strings from the guest app binary.
@implementation _touchHLE_NSString_CFConstantString_UTF8: _touchHLE_NSString_Static

- (ConstPtr<u8>)UTF8String {
    let cfstringStruct { bytes, .. } = env.mem.read(this.cast());

    bytes
}

@end

@implementation _touchHLE_NSString_CFConstantString_UTF16: _touchHLE_NSString_Static
@end

@implementation _touchHLE_NSMutableString: NSMutableString

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(StringHostObject::Utf8(Cow::Borrowed("")));
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithCapacity:(NSUInteger)_capacity {
    // TODO: capacity
    msg![env; this init]
}

- (id)initWithBytes:(ConstPtr<u8>)bytes
             length:(NSUInteger)len
           encoding:(NSStringEncoding)encoding {
    // [扫描修 2026-09-15] F8-4:同 _touchHLE_NSString(长度 0 不碰内存;无法解码返回 nil 而不是 panic)
    if len == 0 {
        *env.objc.borrow_mut(this) = StringHostObject::Utf8(Cow::Borrowed(""));
        return this;
    }
    let slice = env.mem.bytes_at(bytes, len);
    let Some(host_object) = StringHostObject::decode(Cow::Borrowed(slice), encoding) else {
        release(env, this);
        return nil;
    };

    *env.objc.borrow_mut(this) = host_object;

    this
}

- (id)initWithFormat:(id)format, // NSString*
                     ...args {
    init_with_format_inner(env, this, format, args.start())
}

- (id)initWithFormat:(id)format // NSString*
           arguments:(VaList)args {
    init_with_format_inner(env, this, format, args)
}

- (id)initWithString:(id)string { // NSString*
    () = msg![env; this setString:string];
    this
}

- (id)dataUsingEncoding:(NSStringEncoding)encoding
   allowLossyConversion:(bool)lossy {
    data_using_encoding_lossy_inner(env, this, encoding, lossy)
}

- (())appendFormat:(id)format, // NSString*
                   ...args {
    assert_ne!(format, nil);
    let res = with_format(env, format, args.start());
    *env.objc.borrow_mut(this) = StringHostObject::Utf8(format!("{}{}", to_rust_string(env, this), res).into());
}

- (())setString:(id)a_string { // NSString*
    assert_ne!(a_string, nil);
    let str = to_rust_string(env, a_string);
    let host_object = StringHostObject::Utf8(str);
    *env.objc.borrow_mut(this) = host_object;
}

- (id)substringWithRange:(NSRange)range {
    let host_object = env.objc.borrow_mut::<StringHostObject>(this);
    let (orig_string, did_convert) = host_object.convert_to_utf16_inplace();
    if did_convert {
        log_dbg!("[{:?} substringWithRange]: converted string to UTF-16", this);
    }
    let host_string =
        orig_string[(range.location as usize)..((range.location + range.length) as usize)].to_vec();
    let res = from_u16_vec(env, host_string);
    autorelease(env, res)
}

@end

// ============================================================================
// [扫描修 2026-09-15] F8-5:NSException / NSAssertionHandler
// ============================================================================
// 逻辑、判断依据与 MOLE_ASSERT 策略全部写在 ns_exception.rs,这里只是薄包装。
// 挂在本文件的类表里,是因为 Foundation 的类表注册在 foundation.rs(不属于本修复包);放进已注册的
// ns_string::CLASSES 就能直接生效。切勿再在 foundation.rs 注册同名类,否则类重复。
// guest 子类 InvalidKeyException / TMA_InvalidKeyException(: NSException)自动继承这些方法。
@implementation NSException: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = super::ns_exception::new_exception_host_object();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)exceptionWithName:(id)name // NSString*
                 reason:(id)reason // NSString*
               userInfo:(id)user_info { // NSDictionary*
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithName:name reason:reason userInfo:user_info];
    autorelease(env, new)
}

+ (())raise:(id)name // NSString*
     format:(id)format, // NSString*
     ...args {
    let reason = if format == nil {
        String::new()
    } else {
        with_format(env, format, args.start())
    };
    super::ns_exception::raise_with_reason(env, this, name, reason, "+raise:format:");
}

- (id)initWithName:(id)name // NSString*
            reason:(id)reason // NSString*
          userInfo:(id)user_info { // NSDictionary*
    let name: id = msg![env; name copy];
    let reason: id = msg![env; reason copy];
    let user_info: id = msg![env; user_info copy];
    let host_object = env.objc.borrow_mut::<super::ns_exception::NSExceptionHostObject>(this);
    host_object.name = name;
    host_object.reason = reason;
    host_object.user_info = user_info;
    this
}

- (())dealloc {
    let host_object = env.objc.borrow::<super::ns_exception::NSExceptionHostObject>(this);
    let (name, reason, user_info) = (host_object.name, host_object.reason, host_object.user_info);
    release(env, name);
    release(env, reason);
    release(env, user_info);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (id)name {
    env.objc.borrow::<super::ns_exception::NSExceptionHostObject>(this).name
}

- (id)reason {
    env.objc.borrow::<super::ns_exception::NSExceptionHostObject>(this).reason
}

- (id)userInfo {
    env.objc.borrow::<super::ns_exception::NSExceptionHostObject>(this).user_info
}

// 真机 -[NSException description] 返回 reason
- (id)description {
    env.objc.borrow::<super::ns_exception::NSExceptionHostObject>(this).reason
}

- (id)callStackReturnAddresses {
    let array = ns_array::from_vec(env, Vec::new());
    autorelease(env, array)
}

- (id)callStackSymbols {
    let array = ns_array::from_vec(env, Vec::new());
    autorelease(env, array)
}

- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

- (())raise {
    super::ns_exception::raise_common(env, this, "-raise");
}

@end

@implementation NSAssertionHandler: NSObject

// 真机是每线程一个实例(存在 threadDictionary 里)。游戏(实际上都是 SDK)只会对它立刻发
// handleFailureIn…,不依赖对象同一性,所以每次给一个新的 autoreleased 实例,省去全局单例。
+ (id)currentHandler {
    let new: id = msg![env; this new];
    autorelease(env, new)
}

- (())handleFailureInMethod:(SEL)selector
                     object:(id)object
                       file:(id)file_name // NSString*
                 lineNumber:(NSInteger)line
                description:(id)format, // NSString*
                ...args {
    let description = if format == nil {
        String::new()
    } else {
        with_format(env, format, args.start())
    };
    let class_name = if object == nil {
        "nil".to_string()
    } else {
        let class: Class = msg![env; object class];
        env.objc.try_get_class_name(class).unwrap_or("?").to_string()
    };
    let selector_name = if selector.is_null() {
        "?".to_string()
    } else {
        selector.as_str(&env.mem).to_string()
    };
    super::ns_exception::assertion_failure(
        env,
        format!("[{class_name} {selector_name}]"),
        file_name,
        line,
        description,
    );
}

- (())handleFailureInFunction:(id)function_name // NSString*
                         file:(id)file_name // NSString*
                   lineNumber:(NSInteger)line
                  description:(id)format, // NSString*
                  ...args {
    let description = if format == nil {
        String::new()
    } else {
        with_format(env, format, args.start())
    };
    let function_name = to_rust_string(env, function_name).into_owned();
    super::ns_exception::assertion_failure(
        env,
        format!("{function_name}()"),
        file_name,
        line,
        description,
    );
}

@end

};

/// This helper is used in `initWithFormat:` on our private subclasses
/// _touchHLE_NSString and _touchHLE_NSMutableString
fn init_with_format_inner(env: &mut Environment, this: id, format: id, args: VaList) -> id {
    let res = with_format(env, format, args);
    *env.objc.borrow_mut::<StringHostObject>(this) = StringHostObject::Utf8(res.into());
    this
}

/// This helper is used in `dataUsingEncoding:allowLossyConversion:` on our
/// private subclasses _touchHLE_NSString and _touchHLE_NSMutableString
fn data_using_encoding_lossy_inner(
    env: &mut Environment,
    this: id,
    encoding: NSStringEncoding,
    lossy: bool,
) -> id {
    // [扫描修 2026-09-15] F8-4:原来 assert 编码只能是 UTF-8/ASCII/Latin-1,ASCII/Latin-1 还 assert 全 ASCII,
    // 否则 panic;allowLossyConversion 被忽略且每次都打日志。现在:
    // - UTF-8,以及 ASCII/Latin-1(含能表示的非 ASCII 字符):保持原有「长度包含结尾 NUL」的行为。
    //   这是原实现的既有特性,+[CryptUtils encryptString:withString:]@0x124876、
    //   -[iMoleVillageAppDelegate hashedISU]@0xfb06 会把这个长度算进密文或摘要。刻意不改,
    //   以免与已经生成的数据不一致(真机不含 NUL,是否统一需单独评估);
    // - 其它编码(UTF-16/32、GB18030、Big5、Shift-JIS 等):按真机语义返回不含 NUL 的数据,通用 UTF-16/32 带 BOM;
    // - lossy 为真时,表示不了的字符写成 '?';不允许有损且表示不了,或编码未实现:返回 nil。
    let string = to_rust_string(env, this);
    let Some(bytes) = encode_for_data(&string, encoding, lossy) else {
        warn_once(format!("data-encode:{encoding:#x}:{lossy}"), || {
            format!("Warning: dataUsingEncoding:{encoding:#x} allowLossyConversion:{lossy} 无法按该编码表示字符串或编码未实现,返回 nil")
        });
        return nil;
    };
    let keeps_nul_in_length = encoding == NSUTF8StringEncoding
        || encoding == NSASCIIStringEncoding
        || encoding == NSISOLatin1StringEncoding;
    if keeps_nul_in_length {
        let c_string = env.mem.alloc_and_write_cstr(&bytes);
        let length: NSUInteger = (bytes.len() + 1).try_into().unwrap();
        msg_class![env; NSData dataWithBytesNoCopy:(c_string.cast_void()) length:length]
    } else {
        let length: NSUInteger = bytes.len().try_into().unwrap();
        let buffer = env.mem.alloc(length.max(1));
        if length > 0 {
            env.mem.bytes_at_mut(buffer.cast(), length).copy_from_slice(&bytes);
        }
        msg_class![env; NSData dataWithBytesNoCopy:buffer length:length]
    }
}

/// For use by [crate::dyld]: Handle static strings listed in the app binary.
/// Sets up host objects and updates `isa` fields
/// (`___CFConstantStringClassReference` is ignored by our dyld).
pub fn register_constant_strings(bin: &MachO, mem: &mut Mem, objc: &mut ObjC) {
    let Some(cfstrings) = bin.get_section("__cfstring") else {
        return;
    };

    assert!(cfstrings.size % guest_size_of::<cfstringStruct>() == 0);
    let base: ConstPtr<cfstringStruct> = Ptr::from_bits(cfstrings.addr);
    for i in 0..(cfstrings.size / guest_size_of::<cfstringStruct>()) {
        let cfstr_ptr = base + i;
        let cfstringStruct {
            _isa,
            flags,
            bytes,
            length,
        } = mem.read(cfstr_ptr);

        // Constant CFStrings should (probably) only ever have flags 0x7c8 and
        // 0x7d0.
        // See https://lists.llvm.org/pipermail/cfe-dev/2008-August/002518.html
        let (host_object, class_name) = if flags == 0x7C8 {
            // ASCII
            let decoded = std::str::from_utf8(mem.bytes_at(bytes, length)).unwrap();

            (
                StringHostObject::Utf8(Cow::Owned(String::from(decoded))),
                "_touchHLE_NSString_CFConstantString_UTF8",
            )
        } else if flags == 0x7D0 {
            // UTF16 (length is in code units, not bytes)
            let decoded = mem
                .bytes_at(bytes, length * 2)
                .chunks(2)
                .map(|chunk| u16::from_le_bytes(chunk.try_into().unwrap()))
                .collect();

            (
                StringHostObject::Utf16(decoded),
                "_touchHLE_NSString_CFConstantString_UTF16",
            )
        } else {
            panic!("Bad CFTypeID for constant string: {flags:#x}");
        };

        objc.register_static_object(cfstr_ptr.cast().cast_mut(), Box::new(host_object));

        let new_isa = objc.get_known_class(class_name, mem);
        mem.write(cfstr_ptr.cast().cast_mut(), new_isa);
    }
}

/// Shortcut for host code: get an NSString corresponding to a `&'static str`,
/// which does not have to be released and is never deallocated.
pub fn get_static_str(env: &mut Environment, from: &'static str) -> id {
    if let Some(&existing) = State::get(env).static_str_pool.get(from) {
        existing
    } else {
        let new = msg_class![env; _touchHLE_NSString_Static alloc];
        *env.objc.borrow_mut(new) = StringHostObject::Utf8(Cow::Borrowed(from));
        State::get(env).static_str_pool.insert(from, new);
        new
    }
}

/// Shortcut for host code, roughly equivalent to
/// `[[NSString alloc] initWithUTF8String:]` in the proper API.
pub fn from_rust_string(env: &mut Environment, from: String) -> id {
    let string: id = msg_class![env; _touchHLE_NSString alloc];
    let host_object: &mut StringHostObject = env.objc.borrow_mut(string);
    *host_object = StringHostObject::Utf8(Cow::Owned(from));
    string
}

/// Shortcut for host code, roughly equivalent to
/// `[[NSMutableString alloc] initWithUTF8String:]` in the proper API.
pub fn mutable_from_rust_string(env: &mut Environment, from: String) -> id {
    let string: id = msg_class![env; _touchHLE_NSMutableString alloc];
    let host_object: &mut StringHostObject = env.objc.borrow_mut(string);
    *host_object = StringHostObject::Utf8(Cow::Owned(from));
    string
}

/// Shortcut for host code, allocs and inits with the given u16 vec.
pub fn from_u16_vec(env: &mut Environment, from: Vec<u16>) -> id {
    let string: id = msg_class![env; _touchHLE_NSString alloc];
    let host_object: &mut StringHostObject = env.objc.borrow_mut(string);
    *host_object = StringHostObject::Utf16(from);
    string
}

/// Shortcut for host code, provides a view of a string in UTF-8.
/// Warning: This may panic if the string is not valid UTF-16!
///
/// TODO: Try to avoid allocating a new String in more cases.
///
/// TODO: Try to avoid converting from UTF-16 in more cases.
pub fn to_rust_string(env: &mut Environment, string: id) -> Cow<'static, str> {
    // MoleWorld offline port: be lenient about nil, matching Objective-C's
    // nil-message semantics ([nil ...] is a no-op returning 0/"") . Several host
    // helpers (e.g. CGRectFromString and the NSKeyedUnarchiver decode methods)
    // pass through whatever an `-objectForKey:`/`-decode...ForKey:` returned, and
    // a key that's simply absent in an old save archive yields nil. Treat that as
    // the empty string instead of panicking in borrow_mut on the nil object.
    if string == nil {
        return Cow::Borrowed("");
    }
    // TODO: handle foreign subclasses of NSString
    let host_object = env.objc.borrow_mut::<StringHostObject>(string);
    match host_object.to_utf8() {
        Ok(utf8) => utf8,
        // [扫描修 2026-09-15] F8-4:含未配对代理项的 UTF-16(比如把 emoji 从中间截断)原来 unwrap panic,
        // 现在把坏码元换成 U+FFFD。
        Err(_) => match &*host_object {
            StringHostObject::Utf16(units) => Cow::Owned(String::from_utf16_lossy(units)),
            StringHostObject::Utf8(utf8) => utf8.clone(),
        },
    }
}

/// Shortcut for host code, calls a callback once for each UTF-16 code-unit in a
/// string. This is equivalent to a for loop using the `length` and
/// `characterAtIndex:` methods, but much more efficient.
pub fn for_each_code_unit<F>(env: &mut Environment, string: id, mut f: F)
where
    F: FnMut(NSUInteger, u16),
{
    // TODO: handle foreign subclasses of NSString
    let mut idx: NSUInteger = 0;
    env.objc
        .borrow::<StringHostObject>(string)
        .iter_code_units()
        .for_each(|c| {
            f(idx, c);
            idx += 1;
        });
}

// [扫描修 2026-09-15] 原 is_match_at_position(逐字符发消息的 O(n·m) 搜索)已由 find_code_units 取代并删除

/// Helper function for shared `doubleValue` and `floatValue` implementations.
fn float_value_common<F: std::str::FromStr + Default>(env: &mut Environment, string: id) -> F {
    let st = to_rust_string(env, string);
    let st = st.trim_start();
    let mut cutoff = st.len();
    for (i, c) in st.char_indices() {
        if !c.is_ascii_digit() && c != '.' && c != '+' && c != '-' {
            cutoff = i;
            break;
        }
    }
    // TODO: handle over/underflow properly
    st[..cutoff].parse().unwrap_or(Default::default())
}

/// Helper function for lineRangeForRange: and
/// getLineStart:end:contentsEnd:forRange:.
///
/// The two last arguments (get_[start/end]) correspond to the
/// start and end/contentsEnd returns. If false is specified for a given
/// argument, the corresponding return values will not be calculated and
/// set to 0.
fn line_range_helper(
    string: &Utf16String,
    range: NSRange,
    get_start: bool,
    get_end: bool,
) -> (NSUInteger, NSUInteger, NSUInteger) {
    let NSRange {
        location: r_start,
        length,
    } = range;
    let r_end: usize = r_start.checked_add(length).unwrap().try_into().unwrap();
    let r_start: usize = r_start.try_into().unwrap();
    // All the line range functions are "counting the posts, not the fences", so
    // it's ok if r_end = length.
    let str_len = string.len();
    assert!(r_end <= str_len, "Range out of bounds!");

    let mut start_pos: usize = 0;
    if get_start {
        start_pos = r_start;
        while start_pos > 0 {
            let c: u16 = string[start_pos - 1];
            // What counts as a line delimiter is noted here:
            // https://developer.apple.com/documentation/foundation/nsstring/1415111-getlinestart?language=objc
            // There's some special handling for if we start in the
            // middle of a CRLF.
            match c {
                // 'LINE FEED (LF)' (\n), 'NEXT LINE (NEL)', 'LINE SEPARATOR',
                // 'PARAGRAPH SEPARATOR'
                0x000A | 0x0085 | 0x2028 | 0x2029 => break,
                // 'CARRIAGE RETURN (CR)' (\r)
                0x000D => {
                    // If the first character is CR, and it is followed by an
                    // LF, then it's not counted as a line delimiter.
                    // (verified on simulator)
                    if start_pos == r_start && start_pos < str_len {
                        let after_cr: u16 = string[start_pos];
                        // 'LINE FEED (LF)' (\n)
                        if after_cr == 0x000A {
                            start_pos -= 1;
                            continue;
                        }
                    }
                    break;
                }
                _ => {}
            }
            start_pos -= 1;
        }
    }

    // There is very little extra cost for also getting contentsEnd if we're
    // getting end (or vice-versa), so they're combined into one argument.
    let mut end_pos = 0;
    let mut cend_pos = 0;
    if get_end {
        // We want to include the entire line that covers the last char
        // in [r_start, r_end).
        cend_pos = if length > 0 { r_end - 1 } else { r_start };
        while cend_pos < str_len {
            let c: u16 = string[cend_pos];
            // See above about what counts as a line delimiter.
            // There's more understandable handling for CRLF here as well.
            match c {
                //  'NEXT LINE (NEL)', 'LINE SEPARATOR', 'PARAGRAPH SEPARATOR'
                0x0085 | 0x2028 | 0x2029 => {
                    end_pos = cend_pos + 1;
                    break;
                }
                // 'LINE FEED (LF)' (\n),
                0x000A => {
                    // If this is the first character checked, then we also need
                    // to check back for a CR.
                    if cend_pos > 0 && string[cend_pos - 1] == 0x000D {
                        cend_pos -= 1;
                        end_pos = cend_pos + 2;
                    } else {
                        end_pos = cend_pos + 1;
                    }
                    break;
                }
                // 'CARRIAGE RETURN (CR)' (\r)
                0x000D => {
                    // Check if next character exists and is LF.
                    if cend_pos < str_len - 1 {
                        let after_cr: u16 = string[cend_pos + 1];
                        // 'LINE FEED (LF)' (\n)
                        if after_cr == 0x000A {
                            end_pos = cend_pos + 2;
                            break;
                        }
                    }
                    end_pos = cend_pos + 1;
                    break;
                }
                _ => {}
            }
            cend_pos += 1;
        }
        if cend_pos == str_len {
            end_pos = cend_pos
        }
    }

    (
        start_pos.try_into().unwrap(),
        end_pos.try_into().unwrap(),
        cend_pos.try_into().unwrap(),
    )
}

#[cfg(test)]
mod ns_string_tests {
    use super::*;
    #[test]
    fn linerange_tests() {
        let range = |x, y| NSRange {
            location: x,
            length: y,
        };
        let str1: Utf16String = "abcd\nab".encode_utf16().collect();
        assert!(line_range_helper(&str1, range(5, 1), true, true) == (5, 7, 7));
        assert!(line_range_helper(&str1, range(4, 1), true, true) == (0, 5, 4));

        let str2: Utf16String = "abc\r".encode_utf16().collect();
        assert!(line_range_helper(&str2, range(4, 0), true, true) == (4, 4, 4));
        assert!(line_range_helper(&str2, range(3, 1), true, true) == (0, 4, 3));

        let str3: Utf16String = "abc\r\nab".encode_utf16().collect();
        assert!(line_range_helper(&str3, range(4, 0), true, true) == (0, 5, 3));
        assert!(line_range_helper(&str3, range(4, 1), true, true) == (0, 5, 3));
        assert!(line_range_helper(&str3, range(6, 1), true, true) == (5, 7, 7));
        assert!(line_range_helper(&str3, range(4, 2), true, true) == (0, 7, 7));

        let str4: Utf16String = "\r\n".encode_utf16().collect();
        assert!(line_range_helper(&str4, range(1, 0), true, true) == (0, 2, 0));
        assert!(line_range_helper(&str4, range(1, 1), true, true) == (0, 2, 0));
        assert!(line_range_helper(&str4, range(0, 0), true, true) == (0, 2, 0));

        let str5: Utf16String = "abcd\na\n".encode_utf16().collect();
        assert!(line_range_helper(&str5, range(6, 1), true, true) == (5, 7, 6));
        assert!(line_range_helper(&str5, range(4, 1), true, true) == (0, 5, 4));
    }

    // [扫描修 2026-09-15] F8-4 单元测试:比较选项按位解析、编码转换(纯函数,不需要 Environment)
    fn u16s(s: &str) -> Utf16String {
        s.encode_utf16().collect()
    }

    #[test]
    fn compare_options_bits() {
        let (opts, unknown) = parse_compare_options(NSCaseInsensitiveSearch | NSBackwardsSearch);
        assert!(opts.case_insensitive && opts.backwards && !opts.anchored && !opts.numeric);
        assert_eq!(unknown, 0);
        let (opts, unknown) = parse_compare_options(NSLiteralSearch);
        assert_eq!(opts, CompareOptions::default());
        assert_eq!(unknown, 0);
        let (_, unknown) = parse_compare_options(NSRegularExpressionSearch | NSCaseInsensitiveSearch);
        assert_eq!(unknown, NSRegularExpressionSearch);
    }

    #[test]
    fn range_search_options() {
        let hay = u16s("abcABCabc");
        let opts = |o| parse_compare_options(o).0;
        assert_eq!(find_code_units(&hay, &u16s("ABC"), opts(0)), Some(3));
        assert_eq!(find_code_units(&hay, &u16s("ABC"), opts(NSCaseInsensitiveSearch)), Some(0));
        // options 5 = CaseInsensitive | Backwards
        assert_eq!(find_code_units(&hay, &u16s("ABC"), opts(5)), Some(6));
        // options 8 = Anchored:只看开头
        assert_eq!(find_code_units(&hay, &u16s("abc"), opts(NSAnchoredSearch)), Some(0));
        assert_eq!(find_code_units(&hay, &u16s("ABC"), opts(NSAnchoredSearch)), None);
        // Anchored | Backwards:只看结尾
        assert_eq!(find_code_units(&hay, &u16s("abc"), opts(NSAnchoredSearch | NSBackwardsSearch)), Some(6));
        assert_eq!(find_code_units(&hay, &u16s("zzz"), opts(0)), None);
        assert_eq!(find_code_units(&hay, &[], opts(0)), None);
        // 代理项不再 panic
        let emoji = u16s("x😀y");
        assert_eq!(find_code_units(&emoji, &u16s("😀"), opts(NSCaseInsensitiveSearch)), Some(1));
    }

    #[test]
    fn compare_with_combined_options() {
        use std::cmp::Ordering;
        let opts = |o| parse_compare_options(o).0;
        assert_eq!(compare_code_units(&u16s("a2"), &u16s("a10"), opts(NSNumericSearch)), Ordering::Less);
        assert_eq!(compare_code_units(&u16s("a2"), &u16s("a10"), opts(0)), Ordering::Greater);
        // 0x41 = CaseInsensitive | Numeric
        assert_eq!(compare_code_units(&u16s("File2"), &u16s("file10"), opts(0x41)), Ordering::Less);
        // 0x44 = Backwards | Numeric(Backwards 对比较无影响)
        assert_eq!(compare_code_units(&u16s("v9"), &u16s("v10"), opts(0x44)), Ordering::Less);
        assert_eq!(compare_code_units(&u16s("ABC"), &u16s("abc"), opts(NSCaseInsensitiveSearch)), Ordering::Equal);
        assert_eq!(
            compare_code_units(&u16s("ABC"), &u16s("abc"), opts(NSCaseInsensitiveSearch | NSForcedOrderingSearch)),
            Ordering::Less
        );
        // 超长数字串不溢出
        assert_eq!(
            compare_code_units(&u16s("99999999999999999999"), &u16s("100000000000000000000"), opts(NSNumericSearch)),
            Ordering::Less
        );
        assert_eq!(compare_code_units(&u16s("a01"), &u16s("a1"), opts(NSNumericSearch)), Ordering::Equal);
    }

    #[test]
    fn replace_with_options() {
        let opts = |o| parse_compare_options(o).0;
        let run = |src: &str, target: &str, repl: &str, o| {
            let (out, count) = replace_code_units(&u16s(src), &u16s(target), &u16s(repl), opts(o));
            (String::from_utf16(&out).unwrap(), count)
        };
        // options 2 = NSLiteralSearch(原来这里 panic)
        assert_eq!(run("a\r\nb\r\n", "\r", "", NSLiteralSearch), ("a\nb\n".to_string(), 2));
        assert_eq!(run("aXbxc", "x", "-", NSCaseInsensitiveSearch), ("a-b-c".to_string(), 2));
        assert_eq!(run("aaa", "aa", "b", 0), ("ba".to_string(), 1));
        assert_eq!(run("aaa", "aa", "b", NSBackwardsSearch), ("ab".to_string(), 1));
        assert_eq!(run("abab", "ab", "X", NSAnchoredSearch), ("Xab".to_string(), 1));
        assert_eq!(run("abab", "ab", "X", NSAnchoredSearch | NSBackwardsSearch), ("abX".to_string(), 1));
        assert_eq!(run("abc", "", "X", 0), ("abc".to_string(), 0));
    }

    #[test]
    fn encode_and_decode_encodings() {
        let decode_utf8 = |bytes: &[u8], encoding| -> Option<String> {
            StringHostObject::decode(Cow::Borrowed(bytes), encoding).map(|host| host.to_utf8().unwrap().into_owned())
        };
        // Latin-1 往返
        assert_eq!(encode_str("café", NSISOLatin1StringEncoding, false), Some(vec![b'c', b'a', b'f', 0xE9]));
        assert_eq!(decode_utf8(&[b'c', b'a', b'f', 0xE9], NSISOLatin1StringEncoding).as_deref(), Some("café"));
        // ASCII:非 ASCII 字符无损模式返回 None(真机 NULL),有损模式换成 '?'
        assert_eq!(encode_str("中a", NSASCIIStringEncoding, false), None);
        assert_eq!(encode_str("中a", NSASCIIStringEncoding, true), Some(b"?a".to_vec()));
        // UTF-16:LE/BE 与带 BOM
        assert_eq!(encode_str("A中", NSUTF16LittleEndianStringEncoding, false), Some(vec![0x41, 0x00, 0x2D, 0x4E]));
        assert_eq!(encode_str("A中", NSUTF16BigEndianStringEncoding, false), Some(vec![0x00, 0x41, 0x4E, 0x2D]));
        assert_eq!(encode_for_data("A", NSUTF16StringEncoding, false), Some(vec![0xFF, 0xFE, 0x41, 0x00]));
        assert_eq!(decode_utf8(&[0xFE, 0xFF, 0x00, 0x41, 0x4E, 0x2D], NSUTF16StringEncoding).as_deref(), Some("A中"));
        assert_eq!(decode_utf8(&[0x41, 0x00, 0x2D, 0x4E], NSUTF16LittleEndianStringEncoding).as_deref(), Some("A中"));
        // GB18030 往返
        let gb = encode_str("摩尔庄园", NSGB18030StringEncoding, false).unwrap();
        assert_eq!(gb, vec![0xC4, 0xA6, 0xB6, 0xFB, 0xD7, 0xAF, 0xD4, 0xB0]);
        assert_eq!(decode_utf8(&gb, NSGB18030StringEncoding).as_deref(), Some("摩尔庄园"));
        // UTF-32
        assert_eq!(decode_utf8(&[0x41, 0, 0, 0], NSUTF32LittleEndianStringEncoding).as_deref(), Some("A"));
        // 未知编码:编码侧返回 None(解码侧同样返回 None,但会写日志文件,不在单测里触发)
        assert_eq!(encode_str("abc", 0x7FFF_0001, false), None);
        assert!(legacy_encoding_for(0x7FFF_0001).is_none());
        // 结尾 NUL 宽度
        assert_eq!(nul_terminator_size(NSUTF8StringEncoding), 1);
        assert_eq!(nul_terminator_size(NSUTF16LittleEndianStringEncoding), 2);
        assert_eq!(nul_terminator_size(NSUTF32StringEncoding), 4);
    }
}

/// Helper function to get bytes of a string in the specified NSStringEncoding.
///
/// `include_null_terminator` flag controls if NULL-terminator should be
/// included or not.
/// Return value specify if provided buffer was ok or too small.
/// (TODO: indicate error on conversion too)
/// In case of small buffer no data is written.
///
/// Right now this helper is used for `NSString getCString:maxLength:encoding:`
/// method and `CFStringGetPascalString` function.
pub fn get_bytes_buffer_inner(
    env: &mut Environment,
    str: id, // NSString *
    buffer: MutPtr<u8>,
    buffer_size: NSUInteger,
    encoding: NSStringEncoding,
    include_null_terminator: bool,
) -> bool {
    // [扫描修 2026-09-15] F8-4:原来 assert 编码只能是 UTF-8/ASCII/MacRoman/Latin-1,
    // 后三种还 assert 全 ASCII,否则 panic。现在按真实编码转换(结尾 NUL 按编码单元宽度补),
    // 无法表示或编码未实现时按真机语义返回 NO。UTF-8 的输出与原来逐字节一致。
    // 另外只触碰实际要写的那段缓冲,不再按 buffer_size 整段取切片(getCString: 会传接近 2GB 的上限)。
    let src = to_rust_string(env, str);
    let Some(mut bytes) = encode_str(&src, encoding, false) else {
        warn_once(format!("get-bytes-encode:{encoding:#x}"), || {
            format!("Warning: getCString:maxLength:encoding:{encoding:#x} 无法表示该字符串或编码未实现,返回 NO(原来这里 panic)")
        });
        return false;
    };
    if include_null_terminator {
        bytes.extend(std::iter::repeat_n(0u8, nul_terminator_size(encoding) as usize));
    }
    if (buffer_size as usize) < bytes.len() {
        return false;
    }
    if !bytes.is_empty() {
        env.mem
            .bytes_at_mut(buffer, bytes.len() as GuestUSize)
            .copy_from_slice(&bytes);
    }

    true
}

/// Helper function used by
/// `[NSString stringByReplacingOccurrencesOfString:withString:options:range:]`
/// method.
fn string_by_replacing_occurrences_inner(
    env: &mut Environment,
    source: id,      // NSString *
    target: id,      // NSString *
    replacement: id, // NSString *
    options: NSStringCompareOptions,
) -> id {
    // [扫描修 2026-09-15] F8-4:原来 options 只认 0 和 NSCaseInsensitiveSearch,连最常见的
    // NSLiteralSearch(2)都 unimplemented! panic(+[TSMutableString urlEncode:stringEncoding:] 就传 2)。
    // 现在按位解析,替换逻辑见 replace_code_units;guest 自定义字符串子类也能处理。
    let source_units = collect_code_units(env, source);
    let target_units = collect_code_units(env, target);

    // Zero-length target case
    if target_units.is_empty() {
        let res = msg![env; source copy];
        return autorelease(env, res);
    }

    let replacement_units = collect_code_units(env, replacement);
    let opts = compare_options_logged(
        options,
        "stringByReplacingOccurrencesOfString:withString:options:range:",
    );
    let (result, _) = replace_code_units(&source_units, &target_units, &replacement_units, opts);

    // TODO: For a foreign subclass of NSString, do we have to return that
    // subclass? The signature implies this isn't the case and it's probably not
    // worth the effort, but it's an interesting question.
    let result_ns_string = msg_class![env; _touchHLE_NSString alloc];
    *env.objc.borrow_mut(result_ns_string) = StringHostObject::Utf16(result);
    autorelease(env, result_ns_string)
}
