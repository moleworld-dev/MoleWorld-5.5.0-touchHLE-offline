/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */

use super::ns_string::{from_rust_string, to_rust_string};
use super::NSInteger;
use crate::abi::FRAME_POINTER;
use crate::cpu::Cpu;
use crate::dyld::{ConstantExports, FunctionExports, HostConstant};
use crate::mem::{ConstPtr, MutVoidPtr, Ptr};
use crate::objc::{id, msg, nil, release, Class, HostObject};
use crate::{export_c_func, Environment};
use std::sync::atomic::{AtomicU32, Ordering};

// All constants are NSExceptionName
pub const CONSTANTS: ConstantExports = &[
    (
        "_NSCharacterConversionException",
        HostConstant::NSString("NSCharacterConversionException"),
    ),
    (
        "_NSDecimalNumberDivideByZeroException",
        HostConstant::NSString("NSDecimalNumberDivideByZeroException"),
    ),
    (
        "_NSDecimalNumberExactnessException",
        HostConstant::NSString("NSDecimalNumberExactnessException"),
    ),
    (
        "_NSDecimalNumberOverflowException",
        HostConstant::NSString("NSDecimalNumberOverflowException"),
    ),
    (
        "_NSDecimalNumberUnderflowException",
        HostConstant::NSString("NSDecimalNumberUnderflowException"),
    ),
    (
        "_NSDestinationInvalidException",
        HostConstant::NSString("NSDestinationInvalidException"),
    ),
    (
        "_NSFileHandleOperationException",
        HostConstant::NSString("NSFileHandleOperationException"),
    ),
    (
        "_NSGenericException",
        HostConstant::NSString("NSGenericException"),
    ),
    (
        "_NSInternalInconsistencyException",
        HostConstant::NSString("NSInternalInconsistencyException"),
    ),
    (
        "_NSInvalidArchiveOperationException",
        HostConstant::NSString("NSInvalidArchiveOperationException"),
    ),
    (
        "_NSInvalidArgumentException",
        HostConstant::NSString("NSInvalidArgumentException"),
    ),
    (
        "_NSInvalidReceivePortException",
        HostConstant::NSString("NSInvalidReceivePortException"),
    ),
    (
        "_NSInvalidSendPortException",
        HostConstant::NSString("NSInvalidSendPortException"),
    ),
    (
        "_NSInvalidUnarchiveOperationException",
        HostConstant::NSString("NSInvalidUnarchiveOperationException"),
    ),
    (
        "_NSInvocationOperationCancelledException",
        HostConstant::NSString("NSInvocationOperationCancelledException"),
    ),
    (
        "_NSInvocationOperationVoidResultException",
        HostConstant::NSString("NSInvocationOperationVoidResultException"),
    ),
    (
        "_NSMallocException",
        HostConstant::NSString("NSMallocException"),
    ),
    (
        "_NSObjectInaccessibleException",
        HostConstant::NSString("NSObjectInaccessibleException"),
    ),
    (
        "_NSObjectNotAvailableException",
        HostConstant::NSString("NSObjectNotAvailableException"),
    ),
    (
        "_NSOldStyleException",
        HostConstant::NSString("NSOldStyleException"),
    ),
    (
        "_NSParseErrorException",
        HostConstant::NSString("NSParseErrorException"),
    ),
    (
        "_NSPortReceiveException",
        HostConstant::NSString("NSPortReceiveException"),
    ),
    (
        "_NSPortSendException",
        HostConstant::NSString("NSPortSendException"),
    ),
    (
        "_NSPortTimeoutException",
        HostConstant::NSString("NSPortTimeoutException"),
    ),
    (
        "_NSRangeException",
        HostConstant::NSString("NSRangeException"),
    ),
    (
        "_NSUndefinedKeyException",
        HostConstant::NSString("NSUndefinedKeyException"),
    ),
    (
        "_NSInconsistentArchiveException",
        HostConstant::NSString("NSInconsistentArchiveException"),
    ),
    (
        "_NSPPDIncludeNotFoundException",
        HostConstant::NSString("NSPPDIncludeNotFoundException"),
    ),
    (
        "_NSPPDIncludeStackOverflowException",
        HostConstant::NSString("NSPPDIncludeStackOverflowException"),
    ),
    (
        "_NSPPDIncludeStackUnderflowException",
        HostConstant::NSString("NSPPDIncludeStackUnderflowException"),
    ),
    (
        "_NSPPDParseException",
        HostConstant::NSString("NSPPDParseException"),
    ),
    (
        "_NSRTFPropertyStackOverflowException",
        HostConstant::NSString("NSRTFPropertyStackOverflowException"),
    ),
    (
        "_NSTIFFException",
        HostConstant::NSString("NSTIFFException"),
    ),
    (
        "_NSAbortModalException",
        HostConstant::NSString("NSAbortModalException"),
    ),
    (
        "_NSAbortPrintingException",
        HostConstant::NSString("NSAbortPrintingException"),
    ),
    (
        "_NSAccessibilityException",
        HostConstant::NSString("NSAccessibilityException"),
    ),
    (
        "_NSAppKitIgnoredException",
        HostConstant::NSString("NSAppKitIgnoredException"),
    ),
    (
        "_NSAppKitVirtualMemoryException",
        HostConstant::NSString("NSAppKitVirtualMemoryException"),
    ),
    (
        "_NSBadBitmapParametersException",
        HostConstant::NSString("NSBadBitmapParametersException"),
    ),
    (
        "_NSBadComparisonException",
        HostConstant::NSString("NSBadComparisonException"),
    ),
    (
        "_NSBadRTFColorTableException",
        HostConstant::NSString("NSBadRTFColorTableException"),
    ),
    (
        "_NSBadRTFDirectiveException",
        HostConstant::NSString("NSBadRTFDirectiveException"),
    ),
    (
        "_NSBadRTFFontTableException",
        HostConstant::NSString("NSBadRTFFontTableException"),
    ),
    (
        "_NSBadRTFStyleSheetException",
        HostConstant::NSString("NSBadRTFStyleSheetException"),
    ),
    (
        "_NSBrowserIllegalDelegateException",
        HostConstant::NSString("NSBrowserIllegalDelegateException"),
    ),
    (
        "_NSColorListIOException",
        HostConstant::NSString("NSColorListIOException"),
    ),
    (
        "_NSColorListNotEditableException",
        HostConstant::NSString("NSColorListNotEditableException"),
    ),
    (
        "_NSDraggingException",
        HostConstant::NSString("NSDraggingException"),
    ),
    (
        "_NSFontUnavailableException",
        HostConstant::NSString("NSFontUnavailableException"),
    ),
    (
        "_NSIllegalSelectorException",
        HostConstant::NSString("NSIllegalSelectorException"),
    ),
    (
        "_NSImageCacheException",
        HostConstant::NSString("NSImageCacheException"),
    ),
    (
        "_NSNibLoadingException",
        HostConstant::NSString("NSNibLoadingException"),
    ),
    (
        "_NSPasteboardCommunicationException",
        HostConstant::NSString("NSPasteboardCommunicationException"),
    ),
    (
        "_NSPrintOperationExistsException",
        HostConstant::NSString("NSPrintOperationExistsException"),
    ),
    (
        "_NSPrintPackageException",
        HostConstant::NSString("NSPrintPackageException"),
    ),
    (
        "_NSPrintingCommunicationException",
        HostConstant::NSString("NSPrintingCommunicationException"),
    ),
    (
        "_NSTextLineTooLongException",
        HostConstant::NSString("NSTextLineTooLongException"),
    ),
    (
        "_NSTextNoSelectionException",
        HostConstant::NSString("NSTextNoSelectionException"),
    ),
    (
        "_NSTextReadException",
        HostConstant::NSString("NSTextReadException"),
    ),
    (
        "_NSTextWriteException",
        HostConstant::NSString("NSTextWriteException"),
    ),
    (
        "_NSTypedStreamVersionException",
        HostConstant::NSString("NSTypedStreamVersionException"),
    ),
    (
        "_NSWindowServerCommunicationException",
        HostConstant::NSString("NSWindowServerCommunicationException"),
    ),
    (
        "_NSWordTablesReadException",
        HostConstant::NSString("NSWordTablesReadException"),
    ),
    (
        "_NSWordTablesWriteException",
        HostConstant::NSString("NSWordTablesWriteException"),
    ),
    (
        "_UIViewControllerHierarchyInconsistencyException",
        HostConstant::NSString("UIViewControllerHierarchyInconsistencyException"),
    ),
    (
        "_UIApplicationInvalidInterfaceOrientationException",
        HostConstant::NSString("UIApplicationInvalidInterfaceOrientationException"),
    ),
];

/// This exception handler is supposed to do last-minute logging before the
/// program terminates. For our purposes, it's completely safe to ignore that.
fn NSSetUncaughtExceptionHandler(_env: &mut Environment, handler: MutVoidPtr) {
    log!(
        "TODO: Ignoring uncaught exception handler at address {:?}",
        handler
    );
}

// ============================================================================
// [扫描修 2026-09-15] F8-5:NSException / NSAssertionHandler / objc_exception_throw
// ============================================================================
//
// 根因:这两个类原来在 touchHLE 里是 UnimplementedClass,+raise:format:、-raise、
// handleFailureInMethod:… 一律按"发给 nil"空操作,游戏带着坏数据继续跑,日志里也毫无痕迹;
// objc_exception_throw 则链接到返回 0 的空桩。
//
// 类定义挂在 ns_string.rs 的类表里(见那里的 NSException / NSAssertionHandler 段):Foundation 的
// 类表注册在 foundation.rs,不属于本修复包,挂进已注册的 ns_string::CLASSES 才能直接生效。
// 本文件放全部逻辑,类方法只是薄包装。以后若把注册挪到 foundation.rs,整段搬过来即可,
// 切勿两边同时注册同名类。
//
// touchHLE 没有 ObjC 异常所需的 SJLJ 栈展开,跳不到 @catch,所以"抛出"只能二选一:
// 记日志后返回(等于异常被吞)或 panic 终止(等于真机未捕获异常闪退)。
//
// 判断依据(re.py 对 5.5.0 主程序的核查):
// 1. +raise:format: 共 401 处调用,游戏自己的只有:+[CryptUtils doCipher:key:context:options:]
//    两处("Key is too small…"/"Problem during cryption; ccStatus == %d.")、
//    +[InvalidKeyException raise](doCipher 在 kCCParamError 时调用)、
//    -[AsyncSocket connectToHost:…]/checkForThreadSafety 的连接状态守卫、MBProgressHUD 空视图检查、
//    cocos2d(CCTexture2D/CCGridBase/CCRenderTexture…)参数检查;其余全在 SDK
//    (JSONKit 各拷贝、ASI、PLCrashReporter、TMA_CryptUtils…)。
// 2. NSAssertionHandler 728 处引用全部在 SDK(TMI_JKSerializer/TMI_JKDictionary、DianRu、GTMBase64、
//    Reachability、TMLocalFile/TMIAP*/TMPurchase),没有一处在存档读写或主玩法代码里。
// 3. objc_exception_throw 只有 4 处调用,全在 SDK(FBFrictionlessRequestSettings、ZAttributedString×3)。
// 4. doCipher 抛异常后并不停下:照常 calloc、调 CCCrypt,最后 dataWithBytes:length: 返回(失败时为空或残缺数据)。
//    - 加密方向:+[CryptUtils encrypt:key:options:] 是全部加密存档写盘的唯一入口,调用方有
//      -[GameData saveUserInfoData]@0x755c8、saveUserPurchaseInfo@0x7e336、
//      -[NewSceneData saveUserinfoToLocal]@0x21dda4、-[NewSceneNetworkBuffer saveToFile]@0x22e2c2、
//      -[WrapperManager writeToFileWithEncrypted:fileName:md5Key:]@0x38f5a6。
//      如果在这里继续执行,空或残缺的密文就会被原子写进 Documents,真机上则是闪退,存档保持原样。
//      → 这是必须停下的"存档损坏路径"。
//    - 解密方向:42 处 decryptData:withKey: 调用(loadUpgradeXP@0x6fc26 等)都把
//      [NSData dataWithContentsOfFile:] 的结果不判空直接传入。文件不存在时,touchHLE 的 CCCrypt
//      对空输入返回 kCCDecodeError,于是会走到 "Problem during cryption" 这个 raise;
//      这在首次启动、可选本地档缺失时是正常流程。-[NewSceneNetworkBuffer loadFromFile] 自己还有 @catch。
//      如果在这里终止,正常游玩也会崩溃,所以只记日志后继续(与原来行为一致)。
//
// 策略(环境变量 MOLE_ASSERT):
// - 不设置(默认,auto):只有调用链经过 +[CryptUtils encrypt:key:options:] 的 raise 才终止,
//   其余 raise / 断言失败都记醒目日志后继续,不引入新崩溃;
// - abort:所有 raise / 断言失败都终止,排查用;
// - log:全部只记日志,包括存档加密路径,风险自担。
// objc_exception_throw 不受该开关影响,始终终止,原因见该函数注释。

/// NSException 的宿主对象。字段在 initWithName:reason:userInfo: 里 copy 持有,dealloc 时释放。
pub(super) struct NSExceptionHostObject {
    pub(super) name: id,      // NSString*
    pub(super) reason: id,    // NSString*
    pub(super) user_info: id, // NSDictionary*
}
impl HostObject for NSExceptionHostObject {}

pub(super) fn new_exception_host_object() -> Box<NSExceptionHostObject> {
    Box::new(NSExceptionHostObject {
        name: nil,
        reason: nil,
        user_info: nil,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AssertPolicy {
    Auto,
    Abort,
    Log,
}

fn assert_policy() -> AssertPolicy {
    static POLICY: std::sync::OnceLock<AssertPolicy> = std::sync::OnceLock::new();
    *POLICY.get_or_init(|| match std::env::var("MOLE_ASSERT").as_deref().map(str::trim) {
        Ok("abort") => AssertPolicy::Abort,
        Ok("log") => AssertPolicy::Log,
        _ => AssertPolicy::Auto,
    })
}

/// +[CryptUtils encrypt:key:options:] 在 5.5.0 主程序里的指令范围(0x12492c 起、0x124968 前,
/// 下一个方法是 decrypt:key:options:)。调用链上出现这里的返回地址 = 正在加密存档准备写盘。
const SAVE_ENCRYPT_RANGE: std::ops::Range<u32> = 0x12492c..0x124968;

/// 日志限流:同一局前 64 次完整打印,之后每 256 次打印一次(防止某条路径每帧抛异常刷屏)。
/// 需要终止时的日志不受限流影响。
static EXCEPTION_LOG_COUNT: AtomicU32 = AtomicU32::new(0);
fn should_log_exception() -> bool {
    let n = EXCEPTION_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
    n < 64 || n.is_multiple_of(256)
}

/// 沿 guest 的 r7 帧指针链收集返回地址(第一个是当前 LR),最多 `max` 个。
/// 只在当前 SP 之上 8MB 范围内、4 字节对齐且严格递增时继续,避免读到栈外的垃圾。
/// iOS ARM 代码都以 `push {…, r7, lr}; add r7, sp, #…` 建帧,[r7] = 上一帧 r7,[r7+4] = 返回地址。
fn guest_return_addresses(env: &Environment, max: usize) -> Vec<u32> {
    let regs = *env.cpu.regs();
    let sp = regs[Cpu::SP];
    let null_end = env.mem.null_segment_size();
    let mut out = Vec::with_capacity(max);
    out.push(regs[Cpu::LR] & !1);
    let mut fp = regs[FRAME_POINTER];
    while out.len() < max {
        let valid = fp & 3 == 0
            && fp >= sp
            && fp >= null_end
            && fp - sp < (8 << 20)
            && fp <= u32::MAX - 8;
        if !valid {
            break;
        }
        let saved_fp_ptr: ConstPtr<u32> = Ptr::from_bits(fp);
        let saved_lr_ptr: ConstPtr<u32> = Ptr::from_bits(fp + 4);
        let saved_fp: u32 = env.mem.read(saved_fp_ptr);
        let saved_lr: u32 = env.mem.read(saved_lr_ptr);
        if saved_lr == 0 {
            break;
        }
        out.push(saved_lr & !1);
        if saved_fp <= fp {
            break;
        }
        fp = saved_fp;
    }
    out
}

fn format_trace(trace: &[u32]) -> String {
    trace
        .iter()
        .map(|addr| format!("{addr:#x}"))
        .collect::<Vec<_>>()
        .join(" ← ")
}

/// 取异常对象的 (类名, name, reason)。非 NSException 对象(比如 @throw 一个字符串)用 description 当 reason。
fn describe_exception(env: &mut Environment, exception: id) -> (String, String, String) {
    if exception == nil {
        return ("nil".to_string(), String::new(), String::new());
    }
    let class: Class = msg![env; exception class];
    let class_name = env
        .objc
        .try_get_class_name(class)
        .unwrap_or("?")
        .to_string();
    let fields = env
        .objc
        .get_host_object(exception)
        .and_then(|host| host.as_any().downcast_ref::<NSExceptionHostObject>())
        .map(|host| (host.name, host.reason));
    match fields {
        Some((name, reason)) => (
            class_name,
            to_rust_string(env, name).into_owned(),
            to_rust_string(env, reason).into_owned(),
        ),
        None => {
            let desc: id = msg![env; exception description];
            (class_name, String::new(), to_rust_string(env, desc).into_owned())
        }
    }
}

/// +[NSException raise:format:] 的实现:用格式化好的 reason 构造异常(接收者可能是 guest 子类,
/// 比如 InvalidKeyException),再走 raise_common。
pub(super) fn raise_with_reason(
    env: &mut Environment,
    class: id,
    name: id,
    reason: String,
    via: &str,
) {
    let reason_ns = from_rust_string(env, reason);
    let exception: id = msg![env; class exceptionWithName:name reason:reason_ns userInfo:nil];
    release(env, reason_ns);
    raise_common(env, exception, via);
}

/// -[NSException raise] 的实现:打印异常与 guest 调用链,再按策略终止或返回。
pub(super) fn raise_common(env: &mut Environment, exception: id, via: &str) {
    let (class_name, name, reason) = describe_exception(env, exception);
    let trace = guest_return_addresses(env, 12);
    let lr = trace.first().copied().unwrap_or(0);
    let in_save_encrypt = trace.iter().any(|addr| SAVE_ENCRYPT_RANGE.contains(addr));
    let policy = assert_policy();
    let abort = match policy {
        AssertPolicy::Abort => true,
        AssertPolicy::Log => false,
        AssertPolicy::Auto => in_save_encrypt,
    };
    if abort {
        let why = if policy == AssertPolicy::Abort {
            "已设 MOLE_ASSERT=abort"
        } else {
            "调用链经过 +[CryptUtils encrypt:key:options:](存档加密写盘路径),继续执行会把空或残缺的密文写进 Documents 存档"
        };
        log!(
            "[!] 未捕获的 NSException {} ({}):name={:?} reason={:?} 调用方 LR={:#x};guest 调用链 {}",
            via,
            class_name,
            name,
            reason,
            lr,
            format_trace(&trace)
        );
        log!(
            "[!] 按真机「未捕获异常 = 闪退」终止模拟器:{}。存档文件在此之前没有被改写。确认要强行继续可设 MOLE_ASSERT=log(风险自担)。",
            why
        );
        panic!("未捕获的 NSException {name}: {reason}({via},LR={lr:#x})");
    }
    if should_log_exception() {
        log!(
            "[!] NSException {} ({}):name={:?} reason={:?} 调用方 LR={:#x};guest 调用链 {};已记录并继续执行(真机此处是未捕获异常闪退;设 MOLE_ASSERT=abort 可在此终止排查)",
            via,
            class_name,
            name,
            reason,
            lr,
            format_trace(&trace)
        );
    }
}

/// NSAssertionHandler handleFailureIn… 的公共实现。所有 NSAssert 调用点都在 SDK 里,默认只记日志。
pub(super) fn assertion_failure(
    env: &mut Environment,
    location: String,
    file_name: id,
    line: NSInteger,
    description: String,
) {
    let file_name = to_rust_string(env, file_name).into_owned();
    let trace = guest_return_addresses(env, 12);
    let lr = trace.first().copied().unwrap_or(0);
    let abort = assert_policy() == AssertPolicy::Abort;
    if abort || should_log_exception() {
        log!(
            "[!] NSAssertionHandler:*** Assertion failure in {}, {}:{} — {:?};调用方 LR={:#x};guest 调用链 {}{}",
            location,
            file_name,
            line,
            description,
            lr,
            format_trace(&trace),
            if abort {
                ";已设 MOLE_ASSERT=abort,终止"
            } else {
                ";已记录并继续执行(真机 NSAssert 失败会抛 NSInternalInconsistencyException 闪退;设 MOLE_ASSERT=abort 可在此终止)"
            }
        );
    }
    if abort {
        panic!("NSAssert 失败:{location} {file_name}:{line} — {description}");
    }
}

/// `void objc_exception_throw(id exception)`,是 @throw 编译出来的调用。
///
/// 这个函数不会返回:编译器在调用点之后不再生成指令,紧跟的就是下一个函数的序言。例如
/// -[ZAttributedString attributedSubstringFromRange:]@0x3082a4 的 blx 后面,0x3082a8 就是下一个方法的
/// push {r4, r5, r7, lr}。原来的空桩返回 0 以后会接着执行这段无关代码,是未定义行为。
/// touchHLE 又没有 SJLJ 栈展开,跳不到 @catch,所以唯一正确的处理是打印后终止,不受 MOLE_ASSERT=log 影响。
/// 游戏自身代码没有直接调用它(4 处调用点都在 SDK)。
fn objc_exception_throw(env: &mut Environment, exception: id) {
    let (class_name, name, reason) = describe_exception(env, exception);
    let trace = guest_return_addresses(env, 12);
    let lr = trace.first().copied().unwrap_or(0);
    log!(
        "[!] objc_exception_throw({}):name={:?} reason={:?} 调用方 LR={:#x};guest 调用链 {}",
        class_name,
        name,
        reason,
        lr,
        format_trace(&trace)
    );
    log!("[!] @throw 是不返回的调用,touchHLE 无法展开到 @catch,返回只会执行无关指令,因此在此终止。");
    panic!("objc_exception_throw:未捕获的 {class_name} {name}: {reason}(LR={lr:#x})");
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(NSSetUncaughtExceptionHandler(_)),
    // [扫描修 2026-09-15] 原来没有导出,dyld 链接到返回 0 的空桩
    export_c_func!(objc_exception_throw(_)),
];
