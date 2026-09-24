/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSData` and `NSMutableData`.

use super::ns_string::{get_static_str, to_rust_string};
use super::{NSRange, NSUInteger};
use crate::frameworks::foundation::ns_keyed_unarchiver::decode_current_data;
use crate::fs::GuestPath;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::{msg_class, Environment};

pub(super) struct NSDataHostObject {
    pub(super) bytes: MutVoidPtr,
    pub(super) length: NSUInteger,
    free_when_done: bool,
}
impl HostObject for NSDataHostObject {}

/// Diagnostic rate-limiter for getBytes:range: out-of-range logs (a garbage-count parser can hit it
/// tens of thousands of times in one frame).
static OVERRUN_LOG_N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// NSData doesn't seem to be an abstract class?
@implementation NSData: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSDataHostObject {
        bytes: Ptr::null(),
        length: 0,
        free_when_done: true,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)dataWithBytesNoCopy:(MutVoidPtr)bytes
                   length:(NSUInteger)length {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithBytesNoCopy:bytes length:length];
    autorelease(env, new)
}

+ (id)dataWithBytesNoCopy:(MutVoidPtr)bytes
                   length:(NSUInteger)length
             freeWhenDone:(bool)free_when_done {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithBytesNoCopy:bytes length:length freeWhenDone:free_when_done];
    autorelease(env, new)
}

+ (id)dataWithBytes:(ConstVoidPtr)bytes
             length:(NSUInteger)length {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithBytes:bytes length:length];
    autorelease(env, new)
}

+ (id)dataWithContentsOfFile:(id)path {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfFile:path];
    autorelease(env, new)
}

+ (id)dataWithContentsOfMappedFile:(id)path {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfMappedFile:path];
    autorelease(env, new)
}

+ (id)dataWithContentsOfURL:(id)url {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithContentsOfURL:url];
    autorelease(env, new)
}

+ (id)dataWithData:(id)data {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithData:data];
    autorelease(env, new)
}

// Calling the standard `init` is also allowed, in which case we just get data
// of size 0.

- (id)initWithBytesNoCopy:(MutVoidPtr)bytes
                   length:(NSUInteger)length {
    msg![env; this initWithBytesNoCopy:bytes length:length freeWhenDone:true]
}

- (id)initWithBytesNoCopy:(MutVoidPtr)bytes
                   length:(NSUInteger)length
             freeWhenDone:(bool)free_when_done {
    let host_object = env.objc.borrow_mut::<NSDataHostObject>(this);
    assert!(host_object.bytes.is_null() && host_object.length == 0);
    host_object.bytes = bytes;
    host_object.length = length;
    host_object.free_when_done = free_when_done;
    this
}

- (id)initWithBytes:(ConstVoidPtr)bytes
              length:(NSUInteger)length {
    let host_object = env.objc.borrow_mut::<NSDataHostObject>(this);
    assert!(host_object.bytes.is_null() && host_object.length == 0);
    let alloc = env.mem.alloc(length);
    env.mem.memmove(alloc, bytes, length);
    host_object.bytes = alloc;
    host_object.length = length;
    this
}

- (id)initWithData:(id)data {
    let bytes: ConstVoidPtr = msg![env; data bytes];
    let length: NSUInteger = msg![env; data length];
    msg![env; this initWithBytes:bytes length:length]
}

- (id)initWithContentsOfURL:(id)url { // NSURL *
    if msg![env; url isFileURL] {
        let ns_path: id = msg![env; url path];
        let path = to_rust_string(env, ns_path);
        assert!(path.starts_with("/")); // TODO
        msg![env; this initWithContentsOfFile:ns_path]
    } else {
        let absolute_str: id = msg![env; url absoluteString];
        let path = to_rust_string(env, absolute_str);
        // MoleWorld online mode: the game reads its serverlist body via
        // [[NSData alloc] initWithContentsOfURL:] on the dead Taomee URL
        // (http://imolelogin.61.com:8080/dynamic/online.imole). Serve our private
        // server instead. Body: "<areaId>\n<host>:<port>\n" (MOLE_SERVER override).
        if env.options.network_access
            && (path.contains("online.imole") || path.contains("imolelogin"))
        {
            let server = std::env::var("MOLE_SERVER")
                .unwrap_or_else(|_| "login.moleworld.net:7821".to_string());
            log!(
                "[serverlist hook] NSData initWithContentsOfURL:{} -> private server '{}'",
                path,
                server
            );
            let body = format!("1\n{}\n", server).into_bytes();
            let len = body.len() as GuestUSize;
            let buf = env.mem.alloc(len);
            env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(&body);
            return msg![env; this initWithBytesNoCopy:buf length:len];
        }
        // [扫描修 2026-09-15] F11-7:淘米 CDN 静态资源(公告板图片),说明见文件末尾 cdn_fetch 上方的注释
        if let Some(relative) = path.strip_prefix(TAOMEE_CDN_PREFIX) {
            let relative = relative.to_string();
            if let Some(bytes) = cdn_fetch(env, &relative) {
                let len = bytes.len() as GuestUSize;
                let buf = env.mem.alloc(len);
                env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(&bytes);
                return msg![env; this initWithBytesNoCopy:buf length:len];
            }
        }
        // [扫描修 2026-09-15] 原来这里 assert!(path.starts_with("http")),其它 scheme 直接 panic;改为同样记日志返回 nil
        log!("TODO: ignoring [(NSData*){:?} initWithContentsOfURL:{:?}]", this, path);
        release(env, this);
        nil
    }
}

- (id)initWithContentsOfFile:(id)path {
    if path == nil {
        return nil;
    }
    let path = to_rust_string(env, path);
    log_dbg!("[(NSData*){:?} initWithContentsOfFile:{:?}]", this, path);
    let Ok(bytes) = env.fs.read(GuestPath::new(&path)) else {
        release(env, this);
        return nil;
    };
    let size = bytes.len().try_into().unwrap();
    let alloc = env.mem.alloc(size);
    let slice = env.mem.bytes_at_mut(alloc.cast(), size);
    slice.copy_from_slice(&bytes);

    let host_object = env.objc.borrow_mut::<NSDataHostObject>(this);
    host_object.bytes = alloc;
    host_object.length = size;
    this
}

- (id)initWithContentsOfMappedFile:(id)path {
    log_dbg!("[NSData initWithContentsOfMappedFile:] not using memory mapping");
    msg![env; this initWithContentsOfFile:path]
}

// [深扫修 2026-09-11] 原来这里写着 "FIXME: writes should be atomic" 并丢弃 atomically 参数,
// 一律走 fs.write(O_TRUNC 后 write_all = 先截断再写)。写盘途中被杀会留下残缺文件,摩尔庄园
// 读到残缺 userinfo.dat/map.dat/偏好 plist 会崩溃或自己删档。现在 atomically:YES 走
// Fs::write_atomic(同目录临时文件 + rename 覆盖)。这是唯一收口点:
// NSKeyedArchiver archiveRootObject:toFile:(atomically:true)、NSDictionary/NSArray/NSString
// writeToFile:atomically:(透传参数)、NSUserDefaults synchronize(经 NSDictionary,atomically:true)、
// 以及 mole_cheats 岛档(writeToFile:atomically:true)全部经过这里,一处修好全部受益。
// NSFileManager createFileAtPath:contents:attributes: 仍传 atomically:false,保持非原子旧行为。
- (bool)writeToFile:(id)path // NSString*
         atomically:(bool)use_aux_file {
    let file = to_rust_string(env, path);
    log_dbg!("[(NSData*){:?} writeToFile:{:?} atomically:{}]", this, file, use_aux_file);
    let host_object = env.objc.borrow::<NSDataHostObject>(this);
    // Mem::bytes_at() panics when the pointer is NULL, but NSData's pointer can
    // be NULL if the length is 0.
    let slice = if host_object.length == 0 {
        &[]
    } else {
        env.mem.bytes_at(host_object.bytes.cast(), host_object.length)
    };
    let result = if use_aux_file {
        env.fs.write_atomic(GuestPath::new(&file), slice)
    } else {
        env.fs.write(GuestPath::new(&file), slice)
    };
    match result {
        Ok(()) => true,
        Err(e) => {
            log!(
                "[!] NSData writeToFile:{:?} atomically:{} 失败:{:?}",
                file,
                use_aux_file,
                e
            );
            false
        }
    }
}

// -[NSData writeToFile:options:error:] — the modern variant. Missing before, it
// silently no-op'd, so any save written through it (game state, backups) never
// reached disk. Delegate to the atomically: variant which actually writes, and
// clear the out-error.
// [深扫修 2026-09-11] 原来忽略 options、一律转 atomically:true(而那边又是假原子)。现在按
// NSDataWritingAtomic(= 1)位分流:含该位走原子写,不含走普通写。摩尔庄园 GameData
// saveUserInfoData@0x756ea 传 movs r3,#1 = NSDataWritingAtomic,userinfo.dat 因此真正原子落盘。
// 其它位(如 NSDataWritingWithoutOverwriting = 2)仍未实现,按原样忽略。
- (bool)writeToFile:(id)path // NSString*
            options:(NSUInteger)write_options
              error:(MutPtr<id>)error { // NSError**
    if !error.is_null() {
        env.mem.write(error, nil);
    }
    // 1 = NSDataWritingAtomic
    let atomic: bool = (write_options & 1) != 0;
    msg![env; this writeToFile:path atomically:atomic]
}

- (())dealloc {
    let &NSDataHostObject { bytes, free_when_done, .. } = env.objc.borrow(this);
    if !bytes.is_null() && free_when_done {
        env.mem.free(bytes);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    release(env, this);
    // Note: Assuming NSKeyedUnarchiver as coder here
    decode_current_data(env, coder, /* is_mutable: */ true)
}

// NSCoding 编码侧(与上面 initWithCoder:→decode_current_data 对称:都用 "NS.data" 键)。
// ★缺它会害死性能:归档器 encode_object 对每个对象发 encodeWithCoder:,NSData/NSMutableData
// 原来没实现 → 命中"未实现选择子"兜底,每个都刷一行 "NSMutableData does not respond to
// encodeWithCoder:; no-op" 警告【且把字节丢掉=存档里 NSData 字段全空】。摩尔庄园岛上每次交互
// 都自动存档、归档里有大量 NSMutableData(图集/缓冲块),于是点击建筑/NPC 就狂刷几十~上千行
// 警告 = I/O 卡顿,且存档不完整。补上对称编码后:警告全消、存档 NSData 正确往返、卡顿消失。
// NSMutableData 是 NSData 子类,继承此方法。
- (())encodeWithCoder:(id)coder {
    let bytes: ConstVoidPtr = msg![env; this bytes];
    let length: NSUInteger = msg![env; this length];
    let key = get_static_str(env, "NS.data");
    let bytes_u8: ConstPtr<u8> = bytes.cast();
    () = msg![env; coder encodeBytes:bytes_u8 length:length forKey:key];
}

- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    let bytes: ConstVoidPtr = msg![env; this bytes];
    let length: NSUInteger = msg![env; this length];
    let new = msg_class![env; NSMutableData alloc];
    msg![env; new initWithBytes:(bytes.cast_mut()) length:length]
}

- (ConstVoidPtr)bytes {
    env.objc.borrow::<NSDataHostObject>(this).bytes.cast_const()
}
- (NSUInteger)length {
    env.objc.borrow::<NSDataHostObject>(this).length
}

- (bool)isEqualToData:(id)other {
    // [深扫修 2026-09-11] 先比长度:空 NSData 互相比较在 Apple 上是合法的(返回 YES),原来直接
    // to_rust_slice 会在长度 0 时 assert panic。摩尔庄园 checkUserinfoMd5:@0x75482 对
    // subdataWithRange: 裁出的 0 长度数据发 isEqualToData:,就会在这里崩。nil 参数返回 NO。
    if other == nil {
        return false;
    }
    let len_a = env.objc.borrow::<NSDataHostObject>(this).length;
    let len_b = env.objc.borrow::<NSDataHostObject>(other).length;
    if len_a != len_b {
        return false;
    }
    if len_a == 0 {
        return true;
    }
    // FIXME: Avoid allocation
    let a = to_rust_slice(env, this).to_owned();
    let b = to_rust_slice(env, other);
    a == b
}

- (id)subdataWithRange:(NSRange)range { // NSData*
    let &NSDataHostObject { bytes, length, .. } = env.objc.borrow(this);
    // Clamp to valid bounds (real NSData would throw NSRangeException; clamping
    // is safer for our purposes).
    let loc = range.location.min(length);
    let len = range.length.min(length - loc);
    let src: Vec<u8> = if len == 0 {
        Vec::new()
    } else {
        env.mem.bytes_at((bytes + loc).cast(), len).to_vec()
    };
    let buf = env.mem.alloc(len);
    if len != 0 {
        env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(&src);
    }
    let new: id = msg_class![env; NSData dataWithBytesNoCopy:buf length:len];
    new
}

- (())getBytes:(MutPtr<u8>)buffer length:(NSUInteger)length {
    let length = length.min(env.objc.borrow::<NSDataHostObject>(this).length);
    let range = NSRange { location: 0, length };
    msg![env; this getBytes:buffer range:range]
}

- (())getBytes:(MutPtr<u8>)buffer range:(NSRange)range {
    if range.length == 0 {
        return;
    }
    let &NSDataHostObject { bytes, length, .. } = env.objc.borrow(this);
    // Real iOS raises NSRangeException for an out-of-range request; touchHLE has no ObjC exceptions, so
    // assert!()-ing here SIGABRTs the whole emulator. A hand-rolled private server can legitimately send
    // a shorter-than-expected reply — e.g. an empty body where the game's parser (parseFriendsList,
    // cmd 1006) then reads a garbage count and loops getBytes:range: off the end of the NSData. Degrade
    // gracefully instead of crashing: clamp to the in-bounds portion and zero-fill the rest of the
    // destination buffer (mirrors subdataWithRange:'s clamping above). The protocol bug stays visible
    // via the log so the server side can be fixed to send a well-formed body.
    // Copy the packed NSRange fields into locals before use — formatting / &-borrowing a
    // #[repr(packed)] field directly is an unaligned reference (rustc E0793).
    let (loc, rlen) = (range.location, range.length);
    if loc >= length || rlen > length - loc {
        // Rate-limit: a garbage-count parser can call this tens of thousands of times in one frame.
        let n = OVERRUN_LOG_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if n < 12 {
            log!(
                "[!] NSData getBytes:range: 越界(loc={} len={} data={}B)— 裁剪+补零不崩(贴近真机 NSRangeException)",
                loc,
                rlen,
                length
            );
        }
        env.mem.bytes_at_mut(buffer, rlen).fill(0);
        if loc < length {
            let avail = (length - loc).min(rlen);
            env.mem
                .memmove(buffer.cast(), bytes.cast_const() + loc, avail);
        }
        return;
    }
    env.mem.memmove(buffer.cast(), bytes.cast_const() + loc, rlen);
}

- (())getBytes:(MutPtr<u8>)buffer {
    let &NSDataHostObject { bytes, length, .. } = env.objc.borrow(this);
    env.mem.memmove(
        buffer.cast(),
        bytes.cast_const(),
        length,
    );
}

@end

@implementation NSMutableData: NSData

+ (id)data {
    msg![env; this dataWithCapacity:0u32]
}

+ (id)dataWithCapacity:(NSUInteger)capacity {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCapacity:capacity];
    autorelease(env, new)
}

+ (id)dataWithLength:(NSUInteger)length {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithLength:length];
    autorelease(env, new)
}

- (id)initWithCapacity:(NSUInteger)_capacity {
    msg![env; this init]
}

- (id)initWithLength:(NSUInteger)length {
    let host_object = env.objc.borrow_mut::<NSDataHostObject>(this);
    assert!(host_object.bytes.is_null() && host_object.length == 0);
    let alloc = env.mem.calloc(length);
    host_object.bytes = alloc;
    host_object.length = length;
    this
}

- (id)copyWithZone:(NSZonePtr)_zone {
    let bytes: ConstVoidPtr = msg![env; this bytes];
    let length: NSUInteger = msg![env; this length];
    let new = msg_class![env; NSData alloc];
    msg![env; new initWithBytes:bytes length:length]
}

- (())increaseLengthBy:(NSUInteger)add_len {
    let &NSDataHostObject { bytes, length, .. } = env.objc.borrow(this);
    let new_len = length + add_len;
    let new_bytes = env.mem.realloc(bytes, new_len);
    let host = env.objc.borrow_mut::<NSDataHostObject>(this);
    host.length = new_len;
    host.bytes = new_bytes;
    log_dbg!("increaseLengthBy bytes {:?}, new_bytes {:?}; length {}, new_len {}", bytes, new_bytes, length, new_len);
}

- (())appendData:(id)other_data { // NSData *
    let other_bytes: ConstVoidPtr = msg![env; other_data bytes];
    let other_bytes: ConstPtr<u8> = other_bytes.cast();
    let other_length: NSUInteger = msg![env; other_data length];
    log_dbg!("appendData other_data {:?}, other_bytes {:?}, other_length {}", other_data, other_bytes, other_length);
    msg![env; this appendBytes:other_bytes length:other_length]
}

- (())appendBytes:(ConstPtr<u8>)append_bytes length:(NSUInteger)append_length {
    let old_len = env.objc.borrow::<NSDataHostObject>(this).length;
    let old_bytes = env.objc.borrow::<NSDataHostObject>(this).bytes;
    () = msg![env; this increaseLengthBy:append_length];
    let &NSDataHostObject { bytes, length, .. } = env.objc.borrow(this);
    log_dbg!("appendBytes old_len {}, append_length {}, length {}", old_len, append_length, length);
    log_dbg!("appendBytes old_bytes {:?}, append_bytes {:?}, bytes {:?}", old_bytes, append_bytes, bytes);
    env.mem.memmove(bytes + old_len, append_bytes.cast(), append_length);
}

// -[NSMutableData replaceBytesInRange:withBytes:] — overwrite range.length bytes
// at range.location with the same number of bytes from `replacement`. Missing
// before, it silently no-op'd, corrupting any in-place patched save buffer.
- (())replaceBytesInRange:(NSRange)range withBytes:(ConstPtr<u8>)replacement {
    // Copy the packed NSRange fields into locals before use (see getBytes:range:).
    let (loc, rlen) = (range.location, range.length);
    if rlen == 0 {
        return;
    }
    let length = env.objc.borrow::<NSDataHostObject>(this).length;
    // [深扫修 2026-09-11] 用 checked 运算校验 range。
    // 根因:原来 `location + length` 在 release 下 u32 回绕。摩尔庄园 checkUserinfoMd5:@0x753b8
    // 对长度 L<16 的 userinfo.dat 算 location = L-16(下溢成 0xFFFFFFF0+L),回绕后 end=L,
    // 跳过扩容,再在 mem.rs Ptr::add 的 checked_add().unwrap() 处莫名其妙地 panic。
    // Apple 语义:location 超出接收者范围抛 NSRangeException(未捕获 = 闪退)。
    // 取舍(按对抗复核 fix_review):回绕/溢出这种必然越界的情况【明确报 NSRangeException 后终止】,
    // 而【不是】静默 no-op。静默跳过会让 md5 校验失败 → 游戏走 loadUserInfoData 0x759e4 反作弊
    // 分支,同时删掉 userinfo.dat 和原本完好的 map.dat,把"崩溃但数据还在"变成"不崩但主村全丢"。
    // 截断文件的来源已由原子写(Fs::write_atomic)根治;已损坏存档的自愈应在游戏专用层(.bak)做。
    let Some(end) = loc.checked_add(rlen) else {
        log!(
            "[!] NSRangeException: -[NSMutableData replaceBytesInRange:{{{}, {}}} withBytes:] 越界(数据仅 {} 字节,location+length 溢出)。真机 iOS 在此抛未捕获异常闪退,这里同样终止。摩尔庄园典型成因:Documents/userinfo.dat 被截断到 16 字节以下(checkUserinfoMd5:);刻意不静默跳过,以免游戏走反作弊分支连 map.dat 一起删除。",
            loc,
            rlen,
            length
        );
        panic!(
            "NSRangeException: -[NSMutableData replaceBytesInRange:{{{loc}, {rlen}}} withBytes:] out of bounds (data length {length}); likely a truncated save file (e.g. Documents/userinfo.dat < 16 bytes)"
        );
    };
    if loc > length {
        // 同样是 Apple 上的 NSRangeException,但这条路径原来能"扩容后写入"不崩,
        // 而 NetworkManager 组包等 20 处调用点共用本方法,为免引入新崩溃保留原宽松行为,
        // 只大声记日志,并把中间空洞补零(原来是 realloc 出的未定义内容)。
        log!(
            "[!] NSRangeException 语义: -[NSMutableData replaceBytesInRange:{{{}, {}}} withBytes:] location 超出数据长度 {} — 宽松处理:扩容、空洞补零后写入",
            loc,
            rlen,
            length
        );
    }
    if end > length {
        () = msg![env; this increaseLengthBy:(end - length)];
        if loc > length {
            let &NSDataHostObject { bytes, .. } = env.objc.borrow(this);
            env.mem
                .bytes_at_mut((bytes + length).cast(), loc - length)
                .fill(0);
        }
    }
    let &NSDataHostObject { bytes, .. } = env.objc.borrow(this);
    env.mem.memmove(bytes + loc, replacement.cast(), rlen);
}

// -[NSMutableData replaceBytesInRange:withBytes:length:] — general form: remove range.length
// bytes at range.location and insert `replacement_length` bytes from `replacement` (resizing
// as needed; the tail after the range shifts). A NULL `replacement` zero-fills (the game's TCP
// login builder uses withBytes:NULL length:16 to write a 16-zero password block). MoleWorld's
// whole packet-build path (login + getAllObjects 1062 / user-info 1001 bodies) needs this; it
// was missing, so every village command body built wrong → only cmd 1234 ever reached the server.
- (())replaceBytesInRange:(NSRange)range
                withBytes:(ConstPtr<u8>)replacement
                   length:(NSUInteger)replacement_length {
    let old_total = env.objc.borrow::<NSDataHostObject>(this).length;
    let loc = range.location.min(old_total);
    let end = loc.saturating_add(range.length).min(old_total);
    let tail_len = old_total - end;
    let new_total = loc + replacement_length + tail_len;
    // Snapshot tail + replacement to host memory before reallocating (handles overlap/move).
    let tail: Vec<u8> = if tail_len > 0 {
        let &NSDataHostObject { bytes, .. } = env.objc.borrow(this);
        env.mem.bytes_at((bytes + end).cast(), tail_len).to_vec()
    } else {
        Vec::new()
    };
    let repl: Vec<u8> = if replacement_length == 0 {
        Vec::new()
    } else if replacement.is_null() {
        vec![0u8; replacement_length as usize]
    } else {
        env.mem
            .bytes_at(replacement.cast(), replacement_length)
            .to_vec()
    };
    let &NSDataHostObject { bytes, .. } = env.objc.borrow(this);
    let new_bytes = env.mem.realloc(bytes, new_total.max(1));
    if replacement_length > 0 {
        env.mem
            .bytes_at_mut((new_bytes + loc).cast(), replacement_length)
            .copy_from_slice(&repl);
    }
    if tail_len > 0 {
        env.mem
            .bytes_at_mut((new_bytes + (loc + replacement_length)).cast(), tail_len)
            .copy_from_slice(&tail);
    }
    let host = env.objc.borrow_mut::<NSDataHostObject>(this);
    host.bytes = new_bytes;
    host.length = new_total;
}

- (MutVoidPtr)mutableBytes {
    let host_obj = env.objc.borrow_mut::<NSDataHostObject>(this);
    assert!(host_obj.length != 0);
    host_obj.bytes
}

- (())setLength:(NSUInteger)new_length {
    let &NSDataHostObject {bytes, length, .. } = env.objc.borrow(this);
    let new_bytes = env.mem.realloc(bytes, new_length);
    if new_length > length {
        env.mem.bytes_at_mut(new_bytes.cast(), new_length)[length as usize..].fill(0);
    }
    let host = env.objc.borrow_mut::<NSDataHostObject>(this);
    host.length = new_length;
    host.bytes = new_bytes;
    log_dbg!("setLength bytes {:?}, new_bytes {:?}; length {}, new_len {}", bytes, new_bytes, length, new_length);
}

@end

};

pub fn to_rust_slice(env: &mut Environment, data: id) -> &[u8] {
    let borrowed_data = env.objc.borrow::<NSDataHostObject>(data);
    // [深扫修 2026-09-11] 长度 0 的 NSData 是合法对象(bytes 可能为 NULL),返回空切片,
    // 不再 assert panic(原来比较/读取空数据会让整个模拟器崩溃)。
    if borrowed_data.length == 0 {
        return &[];
    }
    assert!(!borrowed_data.bytes.is_null());
    env.mem
        .bytes_at(borrowed_data.bytes.cast(), borrowed_data.length)
}

// ============================================================================
// [扫描修 2026-09-15] F11-7:淘米 CDN(mcdn.61.com)静态资源,主要是公告板图片
// ============================================================================
// 根因:-[GameManager onCommandReceived:] 收到 1058 公告回包、noticeMessages.count>0 时,会创建
// NoticeBoardLayer 并调用 getNoticeImageFromServer。-[NoticeBoardLayer asynchronousHttpNoticeImage]@0x1ff7e0
// 用 [NSMutableData dataWithContentsOfURL:] 拉取 http://mcdn.61.com/ad/2012060701/noticeBoard_iPhone.png
// (iPad 或 Retina 设备拉 noticeBoard_iPad.png)。原来这里对非服务器列表的 http URL 一律返回 nil,
// 于是 imageRecieved_=0,-[NoticeBoardLayer onImageRecieved]@0x1ff67c 走失败分支:cancelAllOperations、
// detech,并弹出「暂时没有公告消息哦!」(NOTICE_EMPTY_MESSAGE)。私服 web 后台编辑的公告因此永远显示不出来;
// 真机上该 CDN 早已停服,同样受影响。
// 拉到数据后,-[NoticeBoardLayer showWithTarget:selector:]@0x1ffd30 用 UIImage imageWithData: 生成 CCSprite
// (key 为 "AdImage")作装饰图;公告正文放在独立的 UITextView(noticeMessage_)里,排版不依赖图片尺寸。
// 所以只要返回一张能正常解码的 PNG,正文就能显示。
//
// 取图顺序(结果按 URL 缓存,整局只解析一次;更换覆盖图需要重启游戏):
// 1. 沙盒覆盖文件:Documents/cdn/<URL 路径>,例如 Documents/cdn/ad/2012060701/noticeBoard_iPad.png;
// 2. 在线模式(network_access)下向私服发明文 GET,Host 头保留 mcdn.61.com,方便私服反代按 Host 路由到静态目录
//    (真机把域名 DNS 指过来也能共用同一份)。目标地址取 MOLE_CDN(host[:port],设为 off 关闭),
//    没设时用 MOLE_SERVER 的主机名加 80 端口。连接超时 2 秒、读写超时 3 秒:touchHLE 的 NSOperationQueue
//    是同步执行的,这里会卡住主线程;
// 3. 包内同名文件:<MoleWorld.app>/<文件名>。已核实 5.5.0 包、58 个旧版 ipa 和解密后的数据表里都没有原图。
//    包里只有布局文件 noticeBoard-iPad.plist,它引用的 noticeBoard.png 等图集帧也不存在。
//    以后找到或重做原图,直接放进包里即可;
// 4. 以上都拿不到,且文件是 noticeBoard_*.png:返回内嵌的 1×1 全透明 PNG,保证板子能打开、正文能显示。
// 同前缀的其它资源(taomee_reward.xml、MoleCartAD.png)只走 1–3,拿不到仍返回 nil,与原来一致。
// 另外 ChrismasTreeView 走 NSURLRequest,AdViewForMoleCart 是被静默的假类,都不会经过这里。
// 所有来源的数据都要校验文件头:.png 必须以 PNG 签名开头,.jpg/.jpeg 必须以 FF D8 开头。
// 原因:UIImage 解码失败得到的 CGImage 是 nil,交给 CCSprite initWithCGImage:key: 有崩溃风险;
// 而私服没配这个路径时,可能返回一张 200 状态的 HTML 页面。

const TAOMEE_CDN_PREFIX: &str = "http://mcdn.61.com/";

/// 1×1 全透明 RGBA PNG(68 字节)。用 python zlib 生成,sips 校验过尺寸 1×1、带 alpha 通道。
const TRANSPARENT_PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xe9, 0xfa, 0xdc, 0xd8, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

/// 解析 http://mcdn.61.com/<relative> 对应的数据;None 表示拿不到,调用方保持原来的返回 nil。
fn cdn_fetch(env: &mut Environment, relative: &str) -> Option<Vec<u8>> {
    use std::collections::HashMap;
    use std::sync::{LazyLock, Mutex};
    static CACHE: LazyLock<Mutex<HashMap<String, Option<Vec<u8>>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    // 去掉查询串与片段;拒绝空路径、空段和 ".."(防止覆盖目录被穿越)
    let relative = relative
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_start_matches('/');
    if relative.is_empty() || relative.split('/').any(|part| part.is_empty() || part == "..") {
        log!("[CDN] 忽略可疑路径 {:?}", relative);
        return None;
    }
    if let Some(cached) = CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(relative).cloned())
    {
        return cached;
    }
    let (result, source) = cdn_resolve(env, relative);
    match &result {
        Some(bytes) => {
            log!(
                "[CDN] {}{} ← {}({} 字节)",
                TAOMEE_CDN_PREFIX,
                relative,
                source,
                bytes.len()
            );
        }
        None => {
            log!(
                "[CDN] {}{} 在沙盒覆盖/私服/包内都找不到,返回 nil(与原来一致)",
                TAOMEE_CDN_PREFIX,
                relative
            );
        }
    }
    if let Ok(mut cache) = CACHE.lock() {
        cache.insert(relative.to_string(), result.clone());
    }
    result
}

fn cdn_resolve(env: &mut Environment, relative: &str) -> (Option<Vec<u8>>, &'static str) {
    let file_name = relative.rsplit('/').next().unwrap_or(relative);

    // 1. 沙盒覆盖文件
    let override_path = env
        .fs
        .home_directory()
        .join(format!("Documents/cdn/{relative}"));
    if let Ok(bytes) = env.fs.read(&*override_path) {
        if cdn_payload_ok(relative, &bytes) {
            return (Some(bytes), "沙盒覆盖文件 Documents/cdn");
        }
        log!("[CDN] 沙盒覆盖文件 Documents/cdn/{} 文件头与扩展名不符,忽略", relative);
    }

    // 2. 在线模式:问私服
    if env.options.network_access {
        if let Some(addr) = cdn_server_addr() {
            match http_get(&addr, "mcdn.61.com", relative) {
                Some(bytes) if cdn_payload_ok(relative, &bytes) => {
                    return (Some(bytes), "私服 HTTP");
                }
                Some(_) => {
                    log!(
                        "[CDN] 私服 {} 返回的 /{} 文件头与扩展名不符(可能是 HTML 错误页),忽略",
                        addr,
                        relative
                    );
                }
                None => {}
            }
        }
    }

    // 3. 包内同名文件
    let bundle_file = env.bundle.bundle_path().join(file_name);
    if let Ok(bytes) = env.fs.read(&*bundle_file) {
        if cdn_payload_ok(relative, &bytes) {
            return (Some(bytes), "包内同名文件");
        }
    }

    // 4. 公告板图兜底
    if file_name.starts_with("noticeBoard_") && file_name.ends_with(".png") {
        return (
            Some(TRANSPARENT_PNG_1X1.to_vec()),
            "内嵌 1×1 透明占位图(包内与旧版 ipa 均无原图)",
        );
    }
    (None, "")
}

/// 按扩展名校验文件头,避免把 HTML 错误页之类当图片交给 UIImage/CCSprite。
fn cdn_payload_ok(relative: &str, bytes: &[u8]) -> bool {
    let lower = relative.to_ascii_lowercase();
    if lower.ends_with(".png") {
        bytes.starts_with(b"\x89PNG\r\n\x1a\n")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        bytes.starts_with(&[0xFF, 0xD8])
    } else {
        !bytes.is_empty()
    }
}

/// 私服静态资源地址:优先 MOLE_CDN(host[:port],设为 off 关闭),否则用 MOLE_SERVER 的主机名加 80 端口。
fn cdn_server_addr() -> Option<String> {
    if let Ok(value) = std::env::var("MOLE_CDN") {
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("off") {
            return None;
        }
        return Some(if value.contains(':') {
            value.to_string()
        } else {
            format!("{value}:80")
        });
    }
    let server = std::env::var("MOLE_SERVER")
        .unwrap_or_else(|_| "login.moleworld.net:7821".to_string());
    let host = server
        .rsplit_once(':')
        .map_or(server.as_str(), |(host, _)| host);
    if host.is_empty() {
        return None;
    }
    Some(format!("{host}:80"))
}

/// 明文 HTTP/1.1 GET(写法仿 mole_cheats.rs 的 http_post_form,改成 GET 并加超时与分块解码)。
/// 只接受 200;响应上限 8MB。
fn http_get(addr: &str, host_header: &str, relative: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let socket_addr = addr.to_socket_addrs().ok()?.next()?;
    let mut stream = TcpStream::connect_timeout(&socket_addr, Duration::from_secs(2)).ok()?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
    let request = format!(
        "GET /{relative} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: MoleWorld-touchHLE\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;
    let mut response = Vec::new();
    stream.take(8 << 20).read_to_end(&mut response).ok()?;

    let head_end = response.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&response[..head_end]).ok()?;
    let mut lines = head.split("\r\n");
    let status: u32 = lines.next()?.split_whitespace().nth(1)?.parse().ok()?;
    if status != 200 {
        log!("[CDN] 私服 {} 对 /{} 返回 HTTP {}", addr, relative, status);
        return None;
    }
    let chunked = lines.any(|line| {
        let line = line.to_ascii_lowercase();
        line.starts_with("transfer-encoding:") && line.contains("chunked")
    });
    let body = &response[head_end + 4..];
    if chunked {
        decode_chunked(body)
    } else {
        Some(body.to_vec())
    }
}

/// 解 HTTP chunked 传输编码;格式不完整返回 None。
fn decode_chunked(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n")?;
        let size_line = std::str::from_utf8(&body[..line_end]).ok()?;
        let size = usize::from_str_radix(size_line.split(';').next()?.trim(), 16).ok()?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Some(out);
        }
        if body.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}
