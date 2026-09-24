/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSFileManager` etc.

use super::{ns_array, ns_string, NSInteger, NSUInteger};
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::foundation::ns_error::{NSCocoaErrorDomain, NSFileReadNoSuchFileError};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::fs::{FsError, GuestPath, GuestPathBuf};
use crate::mem::{ConstPtr, MutPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, ClassExports, HostObject,
};
use crate::Environment;

type NSSearchPathDirectory = NSUInteger;
const NSApplicationDirectory: NSSearchPathDirectory = 1;
const NSLibraryDirectory: NSSearchPathDirectory = 5;
const NSDocumentDirectory: NSSearchPathDirectory = 9;

type NSSearchPathDomainMask = NSUInteger;
const NSUserDomainMask: NSSearchPathDomainMask = 1;

// [扫描修 2026-09-16] F2-02:removeItemAtPath:error: 删除失败时回填的 NSCocoaErrorDomain 错误码,
// 取值同 Foundation 的 FoundationErrors.h;只有本文件用到,就近定义。
const NSFileWriteUnknownError: NSInteger = 512;
const NSFileWriteNoPermissionError: NSInteger = 513;

/// [扫描修 2026-09-16] FS-01/FS-02:Fs 层错误 → NSCocoaErrorDomain 错误码。
/// 映射就是 71601f6(F2-02)在 removeItemAtPath:error: 里写的那套,抽出来给 moveItemAtPath:toPath:error:、
/// createDirectoryAtPath:withIntermediateDirectories:attributes:error:、copyItemAtPath:toPath:error: 共用:
/// 不存在 → NSFileReadNoSuchFileError(260),无权限 → NSFileWriteNoPermissionError(513),其余 → 512。
/// 宿主 IoError 按 io::ErrorKind 归到前两类(Fs::rename 的宿主 rename 会带回 NotFound)。
/// 游戏里这些调用处都不读错误码,只判断 NSError 是否为 nil / 返回值是否为 NO。
fn cocoa_file_error_code(err: &FsError) -> NSInteger {
    match err {
        FsError::DoesNotExist | FsError::NonexistentParentDir => NSFileReadNoSuchFileError,
        FsError::AccessDenied | FsError::ReadonlyParentDir => NSFileWriteNoPermissionError,
        FsError::IoError(e) if e.kind() == std::io::ErrorKind::NotFound => {
            NSFileReadNoSuchFileError
        }
        FsError::IoError(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            NSFileWriteNoPermissionError
        }
        _ => NSFileWriteUnknownError,
    }
}

/// [扫描修 2026-09-16] FS-01/FS-02:调用方传了非空 `NSError**` 时,autorelease 一个
/// NSCocoaErrorDomain 错误写回;传 NULL 时什么都不做。只在失败时调用(Cocoa 约定成功时不碰 error)。
fn write_cocoa_file_error(env: &mut Environment, out_error: MutPtr<id>, code: NSInteger) {
    if out_error.is_null() {
        return;
    }
    let domain = get_static_str(env, NSCocoaErrorDomain);
    let error: id = msg_class![env; NSError alloc];
    let error: id = msg![env; error initWithDomain:domain code:code userInfo:nil];
    autorelease(env, error);
    env.mem.write(out_error, error);
}

pub const NSFileModificationDate: &str = "NSFileModificationDate";
pub const NSFileSize: &str = "NSFileSize";
const NSFileSystemFreeSize: &str = "NSFileSystemFreeSize";
pub const NSFileType: &str = "NSFileType";
pub const NSFileTypeDirectory: &str = "NSFileTypeDirectory";
pub const NSFileTypeRegular: &str = "NSFileTypeRegular";

pub const CONSTANTS: ConstantExports = &[
    (
        "_NSFileModificationDate",
        HostConstant::NSString(NSFileModificationDate),
    ),
    ("_NSFileSize", HostConstant::NSString(NSFileSize)),
    (
        "_NSFileSystemFreeSize",
        HostConstant::NSString(NSFileSystemFreeSize),
    ),
    ("_NSFileType", HostConstant::NSString(NSFileType)),
    (
        "_NSFileTypeDirectory",
        HostConstant::NSString(NSFileTypeDirectory),
    ),
    (
        "_NSFileTypeRegular",
        HostConstant::NSString(NSFileTypeRegular),
    ),
];

fn NSSearchPathForDirectoriesInDomains(
    env: &mut Environment,
    directory: NSSearchPathDirectory,
    domain_mask: NSSearchPathDomainMask,
    expand_tilde: bool,
) -> id {
    // TODO: other cases not implemented
    assert!(domain_mask == NSUserDomainMask);
    assert!(expand_tilde);

    let dir = match directory {
        NSApplicationDirectory => {
            // This might not actually be correct. I haven't bothered to
            // test it because I can't think of a good reason an iPhone OS app
            // would have to request this;
            // Wolfenstein 3D requests it but never uses it.
            GuestPath::new(crate::fs::APPLICATIONS).to_owned()
        }
        NSDocumentDirectory => env.fs.home_directory().join("Documents"),
        NSLibraryDirectory => env.fs.home_directory().join("Library"),
        // 13 = NSCachesDirectory. MoleWorld's immob SDK requests it (to cache the
        // web-view user agent, downloaded configs, etc.). Conventionally this is
        // <home>/Library/Caches.
        13 => env.fs.home_directory().join("Library").join("Caches"),
        _ => todo!("NSSearchPathDirectory {}", directory),
    };
    let dir = ns_string::from_rust_string(env, String::from(dir));
    let dir_list = ns_array::from_vec(env, vec![dir]);
    autorelease(env, dir_list)
}

fn NSHomeDirectory(env: &mut Environment) -> id {
    let dir = env.fs.home_directory();
    let dir = ns_string::from_rust_string(env, String::from(dir.as_str()));
    autorelease(env, dir)
}

/// Check [crate::fs::Fs::new] for more info for
/// how temporary folder is setup on startup
fn NSTemporaryDirectory(env: &mut Environment) -> id {
    let dir = env.fs.home_directory().join("tmp");
    let dir = ns_string::from_rust_string(env, String::from(dir.as_str()));
    autorelease(env, dir)
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(NSHomeDirectory()),
    export_c_func!(NSTemporaryDirectory()),
    export_c_func!(NSSearchPathForDirectoriesInDomains(_, _, _)),
];

#[derive(Default)]
pub struct State {
    default_manager: Option<id>,
}

struct NSDirectoryEnumeratorHostObject {
    iterator: std::vec::IntoIter<GuestPathBuf>,
}
impl HostObject for NSDirectoryEnumeratorHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSFileManager: NSObject

+ (id)defaultManager {
    if let Some(existing) = env.framework_state.foundation.ns_file_manager.default_manager {
        existing
    } else {
        let new: id = msg![env; this new];
        env.framework_state.foundation.ns_file_manager.default_manager = Some(new);
        new
    }
}

- (id)currentDirectoryPath {
    ns_string::from_rust_string(env, env.fs.working_directory().as_str().to_string())
}

- (bool)changeCurrentDirectoryPath:(id)path {
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let path = GuestPath::new(&path);
    match env.fs.change_working_directory(path) {
        Ok(_) => true,
        Err(()) => false
    }
}

- (bool)fileExistsAtPath:(id)path { // NSString*
    let res_exists = if path == nil {
        false
    } else {
        let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
        // fileExistsAtPath: will return true for directories
        // hence Fs::exists() rather than Fs::is_file() is appropriate.
        env.fs.exists(GuestPath::new(&path))
    };
    log_dbg!("[(NSFileManager*) {:?} fileExistsAtPath:{:?}] => {}", this, path, res_exists);
    res_exists
}

- (bool)fileExistsAtPath:(id)path // NSString*
             isDirectory:(MutPtr<bool>)is_dir {
    let (res_exists, res_is_dir) = if path == nil {
        (false, false)
    } else {
        // TODO: mutualize with fileExistsAtPath:
        let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
        let guest_path = GuestPath::new(&path);
        (env.fs.exists(guest_path), !env.fs.is_file(guest_path))
    };

    if !is_dir.is_null() {
        env.mem.write(is_dir, res_is_dir);
    }

    log_dbg!("[(NSFileManager*) {:?} fileExistsAtPath:{:?} isDirectory:{:?}] => {}", this, path, res_is_dir, res_exists);
    res_exists
}

- (bool)createFileAtPath:(id)path // NSString*
                contents:(id)data // NSData*
              attributes:(id)attributes { // NSDictionary*
    assert!(attributes == nil); // TODO
    if data == nil {
        let empty: id = msg_class![env; NSData new];
        let res: bool = msg![env; empty writeToFile:path atomically:false];
        release(env, empty);
        res
    } else {
        msg![env; data writeToFile:path atomically:false]
    }
}

- (bool)removeItemAtPath:(id)path // NSString*
                   error:(MutPtr<id>)out_error { // NSError**
    // TODO: call delegate
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    match env.fs.remove(GuestPath::new(&path)) {
        Ok(()) => true,
        Err(err) => {
            // [扫描修 2026-09-16] F2-02:原来只给 DoesNotExist 造 NSError,其余错误在 error 指针非空时走
            // unimplemented! 崩溃。原版 -[GameData resetUserGameData]@0x7dec8、-[GameData loadUserInfoData]@0x75a26、
            // -[WrapperManager deleteFile:]@0x38f79a 发这个消息时都传了非空 NSError**,所以 Fs::remove 一旦返回
            // 宿主 IoError/无权限,光修 fs.rs 只是把崩溃点挪到这里。按 Foundation 删除失败时的 errno 归类补全:
            // 无权限 → NSFileWriteNoPermissionError,其余 → NSFileWriteUnknownError;文件或父目录不存在沿用原有的
            // NSFileReadNoSuchFileError。上述调用处都不读错误码,关键是回 NO 而不是崩。
            // [扫描修 2026-09-16] FS-01/FS-02:归类与回填抽成 cocoa_file_error_code / write_cocoa_file_error,
            // 与 moveItemAtPath:、createDirectoryAtPath:、copyItemAtPath: 共用,本方法行为不变。
            let code = cocoa_file_error_code(&err);
            if code != NSFileReadNoSuchFileError {
                log!(
                    "[NSFileManager] removeItemAtPath {} 失败({:?}),返回 NO,NSCocoaErrorDomain 错误码 {}",
                    path,
                    err,
                    code
                );
            }
            write_cocoa_file_error(env, out_error, code);
            false
        }
    }
}

- (bool)moveItemAtPath:(id)path // NSString*
                toPath:(id)toPath // NSString*
                 error:(MutPtr<id>)error { // NSError**
    // TODO: call delegate
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let toPath = ns_string::to_rust_string(env, toPath); // TODO: avoid copy
    match env.fs.rename(GuestPath::new(&path), GuestPath::new(&toPath)) {
        Ok(()) => true,
        Err(err) => {
            // [扫描修 2026-09-16] FS-01:原来 error 指针非空时是 todo!() 崩溃。原版 -[ASIHTTPRequest
            // handleStreamComplete]@0x2ac86e(及 TMA_/TM_/TMI_/AppDriverChina 变体共 9 处)传 &moveError,
            // 调用后在 0x2ac872 读 moveError 判非 nil 才走失败分支(不看返回值),所以失败时必须写回 NSError,
            // 否则下载会被当成移动成功。Fs::rename 的失败现在都是 Err 且不改 guest 树,这里按统一映射回 NO。
            let code = cocoa_file_error_code(&err);
            log!(
                "[NSFileManager] moveItemAtPath {} toPath {} 失败({:?}),返回 NO,NSCocoaErrorDomain 错误码 {}",
                path,
                toPath,
                err,
                code
            );
            write_cocoa_file_error(env, error, code);
            false
        }
    }
}

- (bool)createDirectoryAtPath:(id)path // NSString *
                   attributes:(id)attributes { // NSDictionary*
    let error: MutPtr<id> = Ptr::null();
    msg![env; this createDirectoryAtPath:path
             withIntermediateDirectories:false
                              attributes:attributes
                                   error:error]
}

- (bool)createDirectoryAtPath:(id)path // NSString *
  withIntermediateDirectories:(bool)with_intermediates
                   attributes:(id)attributes // NSDictionary*
                        error:(MutPtr<id>)error { // NSError**
    // [扫描修 2026-09-16] FS-02:原来 assert_eq!(attributes, nil)。原版 -[PLCrashReporter
    // populateCrashReportDirectoryAndReturnError:]@0x53866c/0x5386bc 传的是
    // dictionaryWithObject:forKey: 建的 {NSFilePosixPermissions: 0755},走到就崩。touchHLE 不模拟
    // POSIX 权限位(libc mkdir 同样忽略 mode),记一次日志后忽略属性,照常建目录。
    if attributes != nil {
        log_once!("[NSFileManager] createDirectoryAtPath:withIntermediateDirectories:attributes:error: 忽略 attributes(不模拟 POSIX 权限)");
    }

    let path_str = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let res = if with_intermediates {
        env.fs.create_dir_all(GuestPath::new(&path_str))
    } else {
        env.fs.create_dir(GuestPath::new(&path_str))
    };
    match res {
        Ok(()) => {
            log_dbg!("createDirectoryAtPath {} => true", path_str);
            true
        }
        Err(err) => {
            // [扫描修 2026-09-16] FS-02:原来 error 指针非空时 assert! 崩溃。PLCrashReporter 上面两处把自己的
            // outError 原样传下来;Fs::create_dir 的宿主失败(FS-03)现在也会返回 Err 走到这里。
            // 按 removeItemAtPath:error: 同一套映射回 NO 并回填 NSError。
            let code = cocoa_file_error_code(&err);
            log!(
                "Warning: createDirectoryAtPath {} failed with {:?}, returning false (NSCocoaErrorDomain code {})",
                path_str,
                err,
                code
            );
            write_cocoa_file_error(env, error, code);
            false
        }
    }
}

- (id)enumeratorAtPath:(id)path { // NSString*
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let Ok(paths) = env.fs.enumerate_recursive(GuestPath::new(&path)) else {
        return nil;
    };
    let host_object = Box::new(NSDirectoryEnumeratorHostObject {
        iterator: paths.into_iter(),
    });
    let class = env.objc.get_known_class("NSDirectoryEnumerator", &mut env.mem);
    let enumerator = env.objc.alloc_object(class, host_object, &mut env.mem);
    autorelease(env, enumerator)
}

- (id)directoryContentsAtPath:(id)path /* NSString* */ { // NSArray*
    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    let Ok(paths) = env.fs.enumerate(GuestPath::new(&path)) else {
        return nil;
    };
    let paths: Vec<GuestPathBuf> = paths
        .map(|path| GuestPathBuf::from(GuestPath::new(path)))
        .collect();
    log_dbg!("directoryContentsAtPath {}: {:?}", path, paths);
    let path_strings = paths
        .iter()
        .map(|name| ns_string::from_rust_string(env, name.as_str().to_string()))
        .collect();
    let res = ns_array::from_vec(env, path_strings);
    autorelease(env, res)
}

- (id)contentsOfDirectoryAtPath:(id)path /* NSString* */
                          error:(MutPtr<id>)error { // NSError**
    let contents: id = msg![env; this directoryContentsAtPath:path];
    if contents == nil && !error.is_null() {
        // The directory doesn't exist / couldn't be read. Report a generic
        // NSCocoaErrorDomain error rather than aborting. (MoleWorld probes
        // optional cache directories that may not exist yet.)
        let domain = ns_string::get_static_str(env, "NSCocoaErrorDomain");
        let err: id = msg_class![env; NSError alloc];
        let err: id = msg![env; err initWithDomain:domain code:260 userInfo:nil]; // NSFileReadNoSuchFileError
        autorelease(env, err);
        env.mem.write(error, err);
    }
    contents
}

- (bool)isReadableFileAtPath:(id)path { // NSString*
    let (_, readable, _, _) = {
        let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
        env.fs.access(GuestPath::new(&path))
    };
    readable
}

- (bool)isWritableFileAtPath:(id)path { // NSString*
    let (_, _, writable, _) = {
        let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
        env.fs.access(GuestPath::new(&path))
    };
    writable
}

- (bool)isDeletableFileAtPath:(id)path { // NSString*
    let is_file = {
        let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
        env.fs.is_file(GuestPath::new(&path))
    };

    if is_file {
        return msg![env; this isWritableFileAtPath:path];
    }

    let directory_enumerator: id = msg![env; this enumeratorAtPath:path];

    let mut is_deletable = true;
    loop {
        let path: id = msg![env; directory_enumerator nextObject];
        if path == nil {
            break;
        }
        let is_path_deletable: bool = msg![env; this isDeletableFileAtPath:path];
        is_deletable &= is_path_deletable;
        if !is_deletable {
            break;
        }
    }
    is_deletable
}

- (id)contentsAtPath:(id)path { // NSString *
    // TODO: return nil if path is directory
    // TODO: handle non-absolute paths?
    assert!(msg![env; path isAbsolutePath]);
    msg_class![env; NSData dataWithContentsOfFile:path]
}

- (bool)copyItemAtPath:(id)src // NSString*
                toPath:(id)dst // NSString*
                 error:(MutPtr<id>)error { // NSError**
    let src = ns_string::to_rust_string(env, src);
    let dst = ns_string::to_rust_string(env, dst);
    // [扫描修 2026-09-16] FS-02 同类:原来两处失败分支在 error 指针非空时 assert! 崩溃。原版
    // -[ASIDownloadCache storeResponseForRequest:maxAge:]@0x5058ea(及 TMI_/AppDriverChina 变体)传的是
    // 栈上 &error。失败时回 NO 并按统一映射回填 NSError;读失败只拿得到 (),按源是否存在分 260/512。
    let data = match env.fs.read(GuestPath::new(src.as_ref())) {
        Ok(d) => d,
        Err(_) => {
            let code = if env.fs.exists(GuestPath::new(&src)) {
                NSFileWriteUnknownError
            } else {
                NSFileReadNoSuchFileError
            };
            log!(
                "[NSFileManager] copyItemAtPath {} toPath {} 读源失败,返回 NO,NSCocoaErrorDomain 错误码 {}",
                src,
                dst,
                code
            );
            write_cocoa_file_error(env, error, code);
            return false;
        }
    };
    if let Err(err) = env.fs.write(GuestPath::new(dst.as_ref()), &data) {
        let code = cocoa_file_error_code(&err);
        log!(
            "[NSFileManager] copyItemAtPath {} toPath {} 写目标失败({:?}),返回 NO,NSCocoaErrorDomain 错误码 {}",
            src,
            dst,
            err,
            code
        );
        write_cocoa_file_error(env, error, code);
        return false;
    }
    true
}

- (ConstPtr<u8>)fileSystemRepresentationWithPath:(id)path { // NSString*
    let length: NSUInteger = msg![env; path length];
    assert!(length > 0);
    // TODO: throw an exception if conversion fails
    msg![env; path UTF8String]
}

- (id)fileAttributesAtPath:(id)path // NSString *
              traverseLink:(bool)traverse {
    // TODO: other attributes
    log_once!("Warning: NSFileManager fileAttributesAtPath:traverseLink: returns only NSFileType, NSFileModificationDate and NSFileSize attributes!");

    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    // TODO: traverse link
    log_dbg!("[(NSFileManager *){:?} fileAttributesAtPath:{} traverse:{}]", this, path, traverse);
    let guest_path = GuestPath::new(&path);

    file_attributes_common(env, guest_path)
}

- (id)attributesOfItemAtPath:(id)path // NSString *
                       error:(MutPtr<id>)error { // NSError **
    // [扫描修 2026-09-16] FS-02 同类:原来入口处 assert!(error.is_null()),原版 -[ASIHTTPRequest buildPostBody]@0x2a4ad8
    // 等传栈上 &err 的调用不论文件在不在都会崩。按 Cocoa 约定只在失败(返回 nil)时回填错误:
    // file_attributes_common 只在路径不存在时返回 nil,对应 260。

    // TODO: other attributes
    log_once!("Warning: NSFileManager attributesOfItemAtPath:error: returns only NSFileType, NSFileModificationDate and NSFileSize attributes!");

    let path = ns_string::to_rust_string(env, path); // TODO: avoid copy
    // TODO: traverse link
    log_dbg!("[(NSFileManager *){:?} attributesOfItemAtPath:{} error:{:?}]", this, path, error);
    let guest_path = GuestPath::new(&path);

    let attrs = file_attributes_common(env, guest_path);
    if attrs == nil {
        write_cocoa_file_error(env, error, NSFileReadNoSuchFileError);
    }
    attrs
}

- (id)attributesOfFileSystemForPath:(id)_path
                              error:(MutPtr<id>)error {
    // TODO: other attributes
    log_once!("Warning: NSFileManager attributesOfFileSystemForPath:error: returns only NSFileSystemFreeSize attribute!");

    assert!(error.is_null()); // TODO

    let dict = msg_class![env; NSMutableDictionary new];

    // Reporting 1 Gb of free space should be enough
    // TODO: unify with `statfs`
    // TODO: account for path
    let size: u64 = 1024 * 1024 * 1024;
    let size_num: id = msg_class![env; NSNumber numberWithUnsignedLongLong:size];

    let fs_free_size_key = get_static_str(env, NSFileSystemFreeSize);
    () = msg![env; dict setObject:size_num forKey:fs_free_size_key];

    let dict_imm = msg![env; dict copy];
    release(env, dict);
    autorelease(env, dict_imm)
}

@end

@implementation NSDirectoryEnumerator: NSEnumerator

- (id)nextObject {
    let host_obj = env.objc.borrow_mut::<NSDirectoryEnumeratorHostObject>(this);
    host_obj.iterator.next().map_or(nil, |s| ns_string::from_rust_string(env, String::from(s)))
}

@end

};

/// Helper function for `fileAttributesAtPath:traverseLink:` and
/// `attributesOfItemAtPath:error:`
fn file_attributes_common(env: &mut Environment, guest_path: &GuestPath) -> id {
    if !env.fs.exists(guest_path) {
        log!(
            "file_attributes_common() called with file that does not exist: {:?}, Returning nil",
            guest_path
        );
        return nil;
    }

    // TODO: support more attributes
    let unix_timestamp: f64 = env.fs.modified(guest_path).unwrap() as f64;
    let unix_ref_date: id = msg_class![env; NSDate dateWithTimeIntervalSince1970:0f64];
    let unix_date: id =
        msg_class![env; NSDate dateWithTimeInterval:unix_timestamp sinceDate:unix_ref_date];

    let size = env.fs.size(guest_path).unwrap();
    let size_num: id = msg_class![env; NSNumber numberWithUnsignedLongLong:size];

    let dict = msg_class![env; NSMutableDictionary new];

    let modif_date_key = get_static_str(env, NSFileModificationDate);
    () = msg![env; dict setObject:unix_date forKey:modif_date_key];

    let size_key = get_static_str(env, NSFileSize);
    () = msg![env; dict setObject:size_num forKey:size_key];

    let file_type_key = get_static_str(env, NSFileType);
    // TODO: other types
    if env.fs.is_file(guest_path) {
        let file_type_regular = get_static_str(env, NSFileTypeRegular);
        () = msg![env; dict setObject:file_type_regular forKey:file_type_key];
    } else if env.fs.is_dir(guest_path) {
        let file_type_directory = get_static_str(env, NSFileTypeDirectory);
        () = msg![env; dict setObject:file_type_directory forKey:file_type_key];
    }

    let dict_imm = msg![env; dict copy];
    release(env, dict);
    autorelease(env, dict_imm)
}
