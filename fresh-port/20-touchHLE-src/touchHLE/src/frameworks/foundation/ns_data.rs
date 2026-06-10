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
        assert!(path.starts_with("http"));
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

// FIXME: writes should be atomic
- (bool)writeToFile:(id)path // NSString*
         atomically:(bool)_use_aux_file {
    let file = to_rust_string(env, path);
    log_dbg!("[(NSData*){:?} writeToFile:{:?} atomically:_]", this, file);
    let host_object = env.objc.borrow::<NSDataHostObject>(this);
    // Mem::bytes_at() panics when the pointer is NULL, but NSData's pointer can
    // be NULL if the length is 0.
    let slice = if host_object.length == 0 {
        &[]
    } else {
        env.mem.bytes_at(host_object.bytes.cast(), host_object.length)
    };
    env.fs.write(GuestPath::new(&file), slice).is_ok()
}

// -[NSData writeToFile:options:error:] — the modern variant. Missing before, it
// silently no-op'd, so any save written through it (game state, backups) never
// reached disk. Delegate to the atomically: variant which actually writes, and
// clear the out-error.
- (bool)writeToFile:(id)path // NSString*
            options:(NSUInteger)_write_options
              error:(MutPtr<id>)error { // NSError**
    if !error.is_null() {
        env.mem.write(error, nil);
    }
    msg![env; this writeToFile:path atomically:true]
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
    if range.length == 0 {
        return;
    }
    let length = env.objc.borrow::<NSDataHostObject>(this).length;
    let end = range.location + range.length;
    if end > length {
        () = msg![env; this increaseLengthBy:(end - length)];
    }
    let &NSDataHostObject { bytes, .. } = env.objc.borrow(this);
    env.mem.memmove(bytes + range.location, replacement.cast(), range.length);
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
    assert!(!borrowed_data.bytes.is_null() && borrowed_data.length != 0);
    env.mem
        .bytes_at(borrowed_data.bytes.cast(), borrowed_data.length)
}
