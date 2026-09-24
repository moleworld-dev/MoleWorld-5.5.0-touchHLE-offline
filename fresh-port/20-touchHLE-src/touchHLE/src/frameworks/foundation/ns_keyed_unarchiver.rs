/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSKeyedUnarchiver` and deserialization of its object graph format.
//!
//! Resources:
//! - You can get a good intuitive grasp of how the format works just by staring
//!   at a pretty-print of a simple nib file from something that can parse
//!   plists, e.g. `plutil -p` or `println!("{:#?}", plist::Value::...);`.
//! - Apple's [Archives and Serializations Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Archiving/Articles/archives.html)

use super::ns_string::{from_rust_string, get_static_str, to_rust_string, NSUTF8StringEncoding};
use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{NSInteger, NSUInteger};
use crate::frameworks::uikit::ui_geometry::{
    CGPointFromString, CGRectFromString, CGSizeFromString,
};
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, MutPtr, MutVoidPtr};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
};
use crate::frameworks::core_foundation::time::apple_epoch;
use crate::Environment;
use plist::{Dictionary, Uid, Value};
use std::io::Cursor;
use std::time::SystemTime;

pub const NSKeyedArchiveRootObjectKey: &str = "root";

pub const CONSTANTS: ConstantExports = &[(
    "_NSKeyedArchiveRootObjectKey",
    HostConstant::NSString(NSKeyedArchiveRootObjectKey),
)];

struct NSKeyedUnarchiverHostObject {
    plist: Dictionary,
    current_key: Option<Uid>,
    /// linear map of Uid => id
    already_unarchived: Vec<Option<id>>,
    /// Something responding to NSKeyedUnarchiverDelegate
    delegate: id,
    /// Stores the buffers decoded by `decodeBytesForKey:returnedLength:`
    /// Instead of reusing the same buffer, we allocate different ones that get
    /// freed on dealloc. A similar behavior has been observed in real iOS.
    temporary_buffers: Vec<MutVoidPtr>,
}
impl HostObject for NSKeyedUnarchiverHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSKeyedUnarchiver: NSCoder

+ (id)allocWithZone:(NSZonePtr)_zone { // struct _NSZone*
    let unarchiver = Box::new(NSKeyedUnarchiverHostObject {
        plist: Dictionary::new(),
        current_key: None,
        already_unarchived: Vec::new(),
        delegate: nil,
        temporary_buffers: Vec::new(),
    });
    env.objc.alloc_object(this, unarchiver, &mut env.mem)
}

+ (id)unarchiveObjectWithFile:(id)path { // NSString *
    let data: id = msg_class![env; NSData dataWithContentsOfFile:path];
    if data == nil {
        return nil;
    }
    msg![env; this unarchiveObjectWithData:data]
}

+ (id)unarchiveObjectWithData:(id)data { // NSData *
    // [2026-09-16] B-03 删掉排查 map.dat 解档时留下的 DIAG 块(先发 length,再对大于 4KB 的档发 count
    // 并打日志),恢复上游写法。只把日志降级不够:那两次 msg_send 仍会每次执行,根对象不是集合时
    // count 还会落进「does not respond」兜底。
    // [2026-09-16 黄金岛审查修] 原来 alloc 出来的 unarchiver 从不释放:它的 host object 持有整份 plist
    // 与 already_unarchived 表(解出来的每一个对象都被它强引用),于是**每解一次档,整张对象图就永久留在内存里**。
    // 黄金岛每次进岛解四个侧档、主存档每次读档也走这里,长时间游玩会持续堆积。
    // 注意 initForReadingWithData: 在 data 为 nil 时返回 nil,这条路径也要把 alloc 出来的那份释放掉。
    // decodeObjectForKey:(见本文件 `- (id)decodeObjectForKey:`)已经代调用方 retain+autorelease 过了,
    // 所以这里**不再**对 result 追加 autorelease —— 原来那一下是多余的一对,且会让读者误以为它返回 +0。
    let allocated: id = msg![env; this alloc];
    let new: id = msg![env; allocated initForReadingWithData:data];
    if new == nil {
        release(env, allocated);
        return nil;
    }
    let root_key = get_static_str(env, NSKeyedArchiveRootObjectKey);
    let result: id = msg![env; new decodeObjectForKey:root_key];
    // ★用 autorelease 而不是立即 release:本方法的 `- (())dealloc` 会把 already_unarchived 里
    //   **每一个**解出来的对象都 release 一遍。立即释放等于让「没被任何父容器 retain 的图成员」
    //   当场消失,时序比以前提前了一帧;放进自动释放池则把它们的存活窗口保持成和以前完全一样
    //   (活到本帧池排空),只是不再永久留着。调用方(四个 island_*.dat 的读档)都在同一次宿主
    //   调用内用完结果,窗口足够。
    autorelease(env, new);
    result
}

// TODO: other init methods.

- (id)initForReadingWithData:(id)data { // NSData *
    if data == nil {
        return nil;
    }

    let length: NSUInteger = msg![env; data length];
    let bytes: ConstVoidPtr = msg![env; data bytes];
    let slice = env.mem.bytes_at(bytes.cast(), length);

    // [MoleWorld 离线移植] 防御性解析:把易碎的 plist 解析放在借用 host_obj 之前,任何一步
    // 失败(空/截断/非法档案,如自动存档被写残的 userinfo.dat)都【返回 nil】而不是 unwrap
    // panic 崩掉整个模拟器。调用方 `unarchiveObjectWithData:` 对 nil unarchiver 续发
    // decodeObjectForKey: 得 nil → 游戏当作"无存档"正常启动,而非启动即崩。
    let plist = match Value::from_reader(Cursor::new(slice)) {
        Ok(p) => p,
        Err(e) => {
            log!(
                "[!] NSKeyedUnarchiver: 坏档({} 字节)无法解析:{:?} — 返回 nil(当无存档,避免崩启动)",
                length,
                e
            );
            return nil;
        }
    };
    let plist = match plist.into_dictionary() {
        Some(d) => d,
        None => {
            log!("[!] NSKeyedUnarchiver: 档案根非字典 — 返回 nil");
            return nil;
        }
    };
    if plist.get("$version").and_then(|v| v.as_unsigned_integer()) != Some(100000)
        || plist.get("$archiver").and_then(|v| v.as_string()) != Some("NSKeyedArchiver")
    {
        log!("[!] NSKeyedUnarchiver: $version/$archiver 不符 — 返回 nil");
        return nil;
    }
    let key_count = match plist.get("$objects").and_then(|v| v.as_array()) {
        Some(a) => a.len(),
        None => {
            log!("[!] NSKeyedUnarchiver: 缺 $objects 数组 — 返回 nil");
            return nil;
        }
    };

    let host_obj = env.objc.borrow_mut::<NSKeyedUnarchiverHostObject>(this);
    assert!(host_obj.already_unarchived.is_empty());
    assert!(host_obj.current_key.is_none());
    assert!(host_obj.plist.is_empty());
    host_obj.already_unarchived = vec![None; key_count];
    host_obj.plist = plist;

    this
}

- (())dealloc {
    let host_obj = borrow_host_obj(env, this);
    let already_unarchived = std::mem::take(&mut host_obj.already_unarchived);
    let temporary_buffers = std::mem::take(&mut host_obj.temporary_buffers);

    for &object in already_unarchived.iter().flatten() {
        release(env, object);
    }

    for &buffer in temporary_buffers.iter() {
        env.mem.free(buffer);
    }

    env.objc.dealloc_object(this, &mut env.mem)
}

// TODO: implement calls to delegate methods
// weak/non-retaining
- (())setDelegate:(id)delegate { // id<NSKeyedUnarchiverDelegate>
    let host_object = env.objc.borrow_mut::<NSKeyedUnarchiverHostObject>(this);
    host_object.delegate = delegate;
}
- (id)delegate {
    env.objc.borrow::<NSKeyedUnarchiverHostObject>(this).delegate
}

// These methods drive most of the decoding. They get called in two cases:
// - By the code that initiates the unarchival, e.g. UINib, to retrieve
//   top-level objects.
// - By the object currently being unarchived, i.e. something that had
//   `initWithCoder:` called on it, to retrieve objects from its scope.
// They are all from the NSCoder abstract class and they return default values
// if the key is unknown.

- (bool)decodeBoolForKey:(id)key { // NSString *
    // [MoleWorld 容错] 真 Cocoa NSKeyedUnarchiver 对数值 key 做跨 NSNumber 类型强转;原版无脑
    // as_boolean().unwrap() 在值存成 int/real 时会 panic(实测进岛解 island_map.dat 崩在
    // decodeIntForKey: None.unwrap)。bool 也兜底:int/real 非0 即真,缺/异常回 false。
    get_value_to_decode_for_key(env, this, key)
        .and_then(|value| coerce_int(value).map(|i| i != 0))
        .unwrap_or(false)
}

- (f64)decodeDoubleForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_real)
        .unwrap_or(0.0)
}

- (f32)decodeFloatForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_real)
        .unwrap_or(0.0) as f32
}

- (NSInteger)decodeIntegerForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_int)
        .unwrap_or(0) as NSInteger
}

- (i32)decodeIntForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_int)
        .unwrap_or(0) as i32
}

- (i32)decodeInt32ForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_int)
        .unwrap_or(0) as i32
}

- (i64)decodeInt64ForKey:(id)key { // NSString *
    get_value_to_decode_for_key(env, this, key)
        .and_then(coerce_int)
        .unwrap_or(0)
}

- (id)decodeObjectForKey:(id)key { // NSString*
    // [深扫修 2026-09-11] 容错:键不存在 → nil;值不是 UID(对象引用)→ 记日志返回 nil,
    // 不再 unwrap panic;UID 0 就是 "$null" = nil 对象(Apple 的 encodeObject:nil 会照写这个键,
    // touchHLE 自己的归档器也把 nil 映射到 UID 0),必须解成 nil,原来会被解成内容为 "$null" 的
    // NSString。与 Apple / GNUstep / swift-corelibs 行为一致。
    let uid_or_other: Option<Option<Uid>> =
        get_value_to_decode_for_key(env, this, key).map(|value| value.as_uid().copied());
    let Some(maybe_uid) = uid_or_other else {
        return nil;
    };
    let Some(next_uid) = maybe_uid else {
        let key_str = to_rust_string(env, key);
        log!(
            "[!] NSKeyedUnarchiver decodeObjectForKey:{:?} 的值不是对象引用(UID)— 返回 nil",
            key_str
        );
        return nil;
    };
    if next_uid.get() == 0 {
        return nil;
    }
    let object = unarchive_key(env, this, next_uid);

    // on behalf of the caller
    retain(env, object);
    autorelease(env, object)
}

- (ConstPtr<u8>)decodeBytesForKey:(id)key returnedLength:(MutPtr<NSUInteger>)length {
    assert!(key != nil);
    let Some(data) = get_value_to_decode_for_key(env, this, key)
        .and_then(|value| value.as_data())
        .map(|data| data.to_vec()) else {
            env.mem.write(length, 0);
            return ConstPtr::null();
    };
    let len: GuestUSize = data.len().try_into().unwrap();
    let guest_bytes: MutVoidPtr = env.mem.alloc(len);
    env.objc.borrow_mut::<NSKeyedUnarchiverHostObject>(this)
        .temporary_buffers
        .push(guest_bytes);
    env.mem
        .bytes_at_mut(guest_bytes.cast(), len)
        .copy_from_slice(data.as_slice());
    env.mem.write(length, len);
    guest_bytes.cast().cast_const()
}

- (bool)containsValueForKey:(id)key { // NSString*
    assert!(key != nil);
    get_value_to_decode_for_key(env, this, key).is_some()
}

// TODO: add more decode methods

// These come from a category in UIKit's UIGeometry.h
- (CGPoint)decodeCGPointForKey:(id)key { // NSString*
    let string: id = msg![env; this decodeObjectForKey:key];
    CGPointFromString(env, string)
}
- (CGSize)decodeCGSizeForKey:(id)key { // NSString*
    let string: id = msg![env; this decodeObjectForKey:key];
    CGSizeFromString(env, string)
}
- (CGRect)decodeCGRectForKey:(id)key { // NSString*
    let string: id = msg![env; this decodeObjectForKey:key];
    CGRectFromString(env, string)
}

@end

};

fn borrow_host_obj(env: &mut Environment, unarchiver: id) -> &mut NSKeyedUnarchiverHostObject {
    env.objc.borrow_mut(unarchiver)
}

fn get_value_to_decode_for_key(env: &mut Environment, unarchiver: id, key: id) -> Option<&Value> {
    let key = to_rust_string(env, key); // TODO: avoid copying string
    let host_obj = borrow_host_obj(env, unarchiver);
    let scope = match host_obj.current_key {
        Some(current_uid) => {
            &host_obj.plist["$objects"].as_array().unwrap()[current_uid.get() as usize]
        }
        None => &host_obj.plist["$top"],
    }
    .as_dictionary()
    .unwrap();
    scope.get(&key)
}

/// [MoleWorld 容错] 把 plist::Value 跨数值类型强转为 i64。真 Cocoa NSKeyedUnarchiver 解 NSNumber
/// 时不区分 int/real/bool 子类型(decodeIntForKey: 对存成 real 的值照样取整),我们要忠实复刻。
/// ★修因(runtime 实测):一键进岛解 island_map.dat 时某 int 字段实际存成 real →
/// 原 `as_signed_integer().unwrap()` 拿到 None panic(ns_keyed_unarchiver.rs:220)→ 进岛崩。
/// 非数值(对象引用/字符串等)返 None,由调用方兜底默认值,绝不 panic。
fn coerce_int(value: &Value) -> Option<i64> {
    value
        .as_signed_integer()
        .or_else(|| value.as_unsigned_integer().map(|u| u as i64))
        .or_else(|| value.as_real().map(|r| r as i64))
        .or_else(|| value.as_boolean().map(|b| b as i64))
}

/// 同 [coerce_int],跨数值类型强转为 f64(int 存的值用 decodeDoubleForKey: 也能取)。
fn coerce_real(value: &Value) -> Option<f64> {
    value
        .as_real()
        .or_else(|| value.as_signed_integer().map(|i| i as f64))
        .or_else(|| value.as_unsigned_integer().map(|u| u as f64))
}

/// The core of the implementation: unarchive something by its uid.
///
/// This is recursive in practice: the `initWithCoder:` messages sent by this
/// function will be received by objects which will then send
/// `decodeXXXWithKey:` messages back to the unarchiver, which will then call
/// this function (and so on).
///
/// The object returned is retained only by the archiver. Remember to retain and
/// possibly autorelease it as appropriate.
fn unarchive_key(env: &mut Environment, unarchiver: id, key: Uid) -> id {
    // [深扫修 2026-09-11] UID 0 永远是 $objects[0] = "$null",代表 nil 对象,统一在此解成 nil
    // (原来落到下面 Value::String 分支变成字符串 "$null")。越界 UID(坏档)记日志返回 nil,
    // 不再数组下标 panic。
    if key.get() == 0 {
        return nil;
    }
    let host_obj = borrow_host_obj(env, unarchiver);
    if key.get() as usize >= host_obj.already_unarchived.len() {
        log!(
            "[!] NSKeyedUnarchiver: UID {} 超出 $objects 范围({} 项,坏档?)— 返回 nil",
            key.get(),
            host_obj.already_unarchived.len()
        );
        return nil;
    }
    if let Some(existing) = host_obj.already_unarchived[key.get() as usize] {
        return existing;
    }

    let objects = host_obj.plist["$objects"].as_array().unwrap();

    let item = &objects[key.get() as usize];
    let new_object = match item {
        // The most general kind of item: a dictionary that contains the info
        // needed to invoke `initWithCoder:` on a class implementing NSCoding.
        Value::Dictionary(dict) => {
            // [2026-09-16 黄金岛审查修] 坏档护栏。上面已为 `key` 做了「UID 越界 → 记日志返 nil」,
            // 但 `$class` 这一串取值原来全是 unwrap/裸下标:`$class` 缺失或不是 UID、类 UID 越界、
            // 类条目不是字典、`$classname` 不是字符串 —— 任一条都会 Rust panic 整个模拟器退出。
            // 而 panic 发生在 `unarchiveObjectWithFile:` 内部,mole_cheats 的坏档隔离(改名 .corrupt
            // + 置保护位)永远轮不到 → 玩家每次进黄金岛都当场崩,不手删 Documents 里的档就再也进不去。
            // 全部改成「记一行日志、这个对象解成 nil」,与 `key` 越界同一口径:解出的对象图少一个条目,
            // 上层的 `dict == nil` / 字段缺失分支能正常走到坏档隔离。
            let Some(class_key) = dict.get("$class").and_then(|v| v.as_uid()).copied() else {
                log!("[!] NSKeyedUnarchiver: UID {} 的条目缺 $class 或它不是 UID(坏档?)— 返回 nil", key.get());
                return nil;
            };
            if class_key.get() as usize >= host_obj.already_unarchived.len() {
                log!(
                    "[!] NSKeyedUnarchiver: $class UID {} 超出 $objects 范围({} 项,坏档?)— 返回 nil",
                    class_key.get(),
                    host_obj.already_unarchived.len()
                );
                return nil;
            }
            let class;
            if let Some(existing) = host_obj.already_unarchived[class_key.get() as usize] {
                class = existing;
            } else {
                let class_dict = &objects[class_key.get() as usize];
                let Some(class_dict) = class_dict.as_dictionary() else {
                    log!("[!] NSKeyedUnarchiver: $class UID {} 指向的不是字典(坏档?)— 返回 nil", class_key.get());
                    return nil;
                };

                let Some(class_name) = class_dict.get("$classname").and_then(|v| v.as_string()) else {
                    log!("[!] NSKeyedUnarchiver: $class UID {} 的条目缺 $classname 或它不是字符串(坏档?)— 返回 nil", class_key.get());
                    return nil;
                };

                class = {
                    // get_known_class needs &mut ObjC, so we can't call it
                    // while holding a reference to the class name, since it
                    // is ultimately owned by ObjC via the host object
                    let class_name = class_name.to_string();
                    env.objc.get_known_class(&class_name, &mut env.mem)
                };
                let host_obj = borrow_host_obj(env, unarchiver); // reborrow

                host_obj.already_unarchived[class_key.get() as usize] = Some(class);
            };

            let host_obj = borrow_host_obj(env, unarchiver); // reborrow
            let old_current_key = host_obj.current_key;
            host_obj.current_key = Some(key);

            let new_object: id = msg![env; class alloc];
            let new_object: id = msg![env; new_object initWithCoder:unarchiver];

            let host_obj = borrow_host_obj(env, unarchiver); // reborrow
            host_obj.current_key = old_current_key;

            new_object
        }
        Value::String(s) => {
            let s = s.to_string();
            from_rust_string(env, s)
        }
        Value::Integer(int) => {
            #[allow(clippy::clone_on_copy)]
            let int = int.clone();
            // Similar logic to deserialize_plist()
            let number: id = msg_class![env; NSNumber alloc];
            // TODO: is this the correct order of preference? does it matter?
            if let Some(int64) = int.as_signed() {
                let longlong: i64 = int64;
                msg![env; number initWithLongLong:longlong]
            } else if let Some(uint64) = int.as_unsigned() {
                let ulonglong: u64 = uint64;
                msg![env; number initWithUnsignedLongLong:ulonglong]
            } else {
                unreachable!(); // according to plist crate docs
            }
        }
        Value::Real(real) => {
            let double: f64 = *real;
            let number: id = msg_class![env; NSNumber alloc];
            msg![env; number initWithDouble:double]
        }
        Value::Boolean(b) => {
            let value: bool = *b;
            let number: id = msg_class![env; NSNumber alloc];
            msg![env; number initWithBool:value]
        }
        Value::Date(date_val) => {
            let time: SystemTime = (*date_val).into();
            // [深扫修 2026-09-12] 2001 年以前的日期 duration_since 返回 Err,原来 unwrap 直接 panic。
            let time_interval = match time.duration_since(apple_epoch()) {
                Ok(d) => d.as_secs_f64(),
                Err(e) => -e.duration().as_secs_f64(),
            };
            let date: id = msg_class![env; NSDate alloc];
            msg![env; date initWithTimeIntervalSinceReferenceDate:time_interval]
        }
        // [深扫修 2026-09-11] Apple 的 NSKeyedArchiver 把不可变 NSData(dataWithBytes: 等,含空
        // NSData)直接存成 $objects 里的原始 <data>(clang 实测),原来落到下面 unimplemented! panic。
        // 构造不可变 NSData,分配逻辑与 decode_current_data 相同。
        Value::Data(raw) => {
            let raw: Vec<u8> = raw.clone();
            let len: GuestUSize = raw.len().try_into().unwrap();
            // alloc(0) 不安全:至少分配 1 字节;length 仍按真实 len(0 = 空 NSData)。
            let guest_bytes: MutVoidPtr = env.mem.alloc(len.max(1));
            if len > 0 {
                env.mem
                    .bytes_at_mut(guest_bytes.cast(), len)
                    .copy_from_slice(raw.as_slice());
            }
            let data: id = msg_class![env; NSData alloc];
            msg![env; data initWithBytesNoCopy:guest_bytes length:len freeWhenDone:true]
        }
        // [深扫修 2026-09-11] 其它形态(原始 Array、Uid 等)NSKeyedArchiver 不会放进 $objects,
        // 没有现实触发源,不瞎猜语义:记日志返回 nil(不写入缓存),替代原来的 unimplemented! panic。
        _ => {
            log!(
                "[!] NSKeyedUnarchiver: 不支持的 $objects[{}] 形态,返回 nil: {:?}",
                key.get(),
                item
            );
            return nil;
        }
    };

    let host_obj = borrow_host_obj(env, unarchiver); // reborrow
    host_obj.already_unarchived[key.get() as usize] = Some(new_object);
    new_object
}

/// Shortcut for use by `[_touchHLE_NSArray initWithCoder:]`.
///
/// The objects are to be considered retained by the `Vec`.
pub fn decode_current_array(env: &mut Environment, unarchiver: id) -> Vec<id> {
    let keys = keys_for_key(env, unarchiver, "NS.objects");

    keys.into_iter()
        // [深扫修 2026-09-11] UID 0($null)元素跳过:unarchive_key 现在把它解成 nil,
        // 而集合里不能放 nil(Apple 的归档器也从不把 nil 编进 NS.objects,只可能来自坏档)。
        // 只跳过显式 $null,真实对象 initWithCoder: 返回 nil 的情况保持原行为不变。
        .filter(|key| key.get() != 0)
        .map(|key| {
            let new_object = unarchive_key(env, unarchiver, key);
            // object is retained by the Vec
            retain(env, new_object)
        })
        .collect()
}

/// Shortcut for use by `[_touchHLE_NSMutableDictionary initWithCoder:]`.
///
/// Similar to `decode_current_array`, but for dictionaries.
/// The keys and objects are not retained!
pub fn decode_current_dict(env: &mut Environment, unarchiver: id) -> Vec<(id, id)> {
    let keys = keys_for_key(env, unarchiver, "NS.keys");
    let vals = keys_for_key(env, unarchiver, "NS.objects");
    // DIAG: surface what the unarchiver actually reads for a big dict (the village map is the only
    // large dict here). NS.keys==0 ⇒ the gunzip'd bplist's root dict is empty (server/gzip/body
    // offset issue); NS.keys==N>0 but final count 0 ⇒ key/val unarchive or insert drops them.
    // [扫描修 2026-09-15] F10-6:原来用 eprintln! 直接写 stderr,绕过 echo!/log! 的日志文件
    // (touchHLE_log.txt 里看不到,排查读档时只在终端可见),每轮进村打 9 行。读档诊断已闭环,
    // 改成 log_dbg!:平时不打印,需要时把本模块加进 log.rs 的 ENABLED_MODULES 即可,且会进日志文件。
    // [2026-09-16] A1-03 现在不用重编也能打开:TOUCHHLE_LOG_MODULES 或
    // --log-modules=touchHLE::frameworks::foundation::ns_keyed_unarchiver。
    if keys.len() > 8 {
        log_dbg!(
            "decode_current_dict: NS.keys={} NS.objects={}",
            keys.len(),
            vals.len()
        );
    }
    log_dbg!("decode_current_dict: keys {:?}, vals {:?}", keys, vals);

    // [深扫修 2026-09-11] 键或值为 UID 0($null)的条目整对跳过(字典不能存 nil 键/值,
    // 只可能来自坏档);其余保持原顺序:先解全部键,再解全部值。
    let pairs: Vec<(Uid, Uid)> = keys
        .into_iter()
        .zip(vals)
        .filter(|(k, v)| k.get() != 0 && v.get() != 0)
        .collect();
    let keys: Vec<id> = pairs
        .iter()
        .map(|&(key, _)| unarchive_key(env, unarchiver, key))
        .collect();
    let vals: Vec<id> = pairs
        .iter()
        .map(|&(_, val)| unarchive_key(env, unarchiver, val))
        .collect();

    keys.into_iter().zip(vals).collect()
}

/// Shortcut for use by `[NSDate initWithCoder:]`.
pub fn decode_current_date(env: &mut Environment, unarchiver: id) -> id {
    let key = get_static_str(env, "NS.time");
    // [MoleWorld] 健壮化:缺 NS.time 键时默认 0.0(参考日期),不再 unwrap(None) panic。
    let timestamp = get_value_to_decode_for_key(env, unarchiver, key)
        .and_then(|v| v.as_real())
        .unwrap_or(0.0);

    let date: id = msg_class![env; NSDate alloc];
    msg![env; date initWithTimeIntervalSinceReferenceDate:timestamp]
}

/// Shortcut for use by `[NSData initWithCoder:]`.
pub fn decode_current_data(env: &mut Environment, unarchiver: id, _is_mutable: bool) -> id {
    let key = get_static_str(env, "NS.data");
    // [MoleWorld] 健壮化:缺 NS.data 键(如离线 keychain/分析 SDK 解空归档)时返回空 NSData,
    // 不再 .unwrap() panic —— 原 unwrap(None) 导致 P0 启动闪退(本函数 .unwrap())。
    let bytes: Vec<u8> = get_value_to_decode_for_key(env, unarchiver, key)
        .and_then(|v| v.as_data())
        .map(|d| d.to_vec())
        .unwrap_or_default();
    let len: GuestUSize = bytes.len().try_into().unwrap();
    // alloc(0) 不安全:至少分配 1 字节;length 仍按真实 len(0=空 NSData)。
    let guest_bytes: MutVoidPtr = env.mem.alloc(len.max(1));
    if len > 0 {
        env.mem
            .bytes_at_mut(guest_bytes.cast(), len)
            .copy_from_slice(bytes.as_slice());
    }
    // 始终给 NSMutableData(NSData 子类,可当不可变用),顺带去掉原 assert!(is_mutable) 崩点。
    let data: id = msg_class![env; NSMutableData alloc];
    msg![env; data initWithBytesNoCopy:guest_bytes length:len freeWhenDone:true]
}

/// Shortcut for use by `[NSString initWithCoder:]`.
pub fn decode_current_string(env: &mut Environment, unarchiver: id) -> id {
    // [深扫修 2026-09-11] 兼容两种形态,去掉两次 unwrap panic:
    // - NS.bytes(<data>):nib / 旧式 NSString 归档,原有路径;
    // - NS.string(<string>):Apple 运行时把 NSMutableString 存成 {$class, NS.string}(clang 实测)。
    // 两者都缺或形态不对 → 记日志按空串处理。
    // [审查修 2026-09-13] NSMutableString 由 Apple 或 touchHLE 归档器(ns_keyed_archiver.rs 的
    // encode_object 与 -[NSString encodeWithCoder:])写成 {$class: NSMutableString, NS.string},
    // 经抽象 NSString 的 initWithCoder: 走到这里;本函数只返回不可变串,可变性由 initWithCoder:
    // 的 mutableCopy 分支负责。
    let bytes_key = get_static_str(env, "NS.bytes");
    let string_key = get_static_str(env, "NS.string");
    // TODO: avoid copying (twice!)
    let mut value: Option<Value> = get_value_to_decode_for_key(env, unarchiver, bytes_key).cloned();
    if value.is_none() {
        value = get_value_to_decode_for_key(env, unarchiver, string_key).cloned();
    }
    let bytes: Vec<u8> = match value {
        Some(Value::Data(d)) => d,
        Some(Value::String(s)) => s.into_bytes(),
        other => {
            log!(
                "[!] NSKeyedUnarchiver decode_current_string: 缺 NS.bytes/NS.string 或形态不支持({:?}),按空串处理",
                other
            );
            Vec::new()
        }
    };

    let len: GuestUSize = bytes.len().try_into().unwrap();
    // alloc(0) 不安全:至少分配 1 字节;length 仍按真实 len。
    let guest_bytes: ConstPtr<u8> = env.mem.alloc(len.max(1)).cast().cast_const();
    if len > 0 {
        env.mem
            .bytes_at_mut(guest_bytes.cast_mut(), len)
            .copy_from_slice(bytes.as_slice());
    }

    let str: id = msg_class![env; NSString alloc];
    // TODO: use initWithBytesNoCopy: once implemented
    let res = msg![env; str initWithBytes:guest_bytes length:len encoding:NSUTF8StringEncoding];
    env.mem.free(guest_bytes.cast().cast_mut());
    res
}

/// Shortcut for use by `[NSNumber initWithCoder:]`.
pub fn decode_current_number(env: &mut Environment, unarchiver: id) -> id {
    let num: id = msg_class![env; NSNumber alloc];
    let int_key = get_static_str(env, "NS.intval");
    let dbl_key = get_static_str(env, "NS.dblval");
    let bool_key = get_static_str(env, "NS.boolval");
    // [深扫修 2026-09-11] 去掉 unwrap / unimplemented! panic,改用跨数值类型强转:
    // - NS.intval 超过 i64::MAX(touchHLE 自产 UnsignedLongLong)时 as_signed_integer 为 None,
    //   原来 panic,现在走 initWithUnsignedLongLong:;
    // - 值类型与键不符时用 coerce_int / coerce_real 兜底;三个键都缺 → 记日志返回 0。
    // 键的优先顺序(intval → dblval → boolval)与原实现相同。
    let int_val: Option<Value> = get_value_to_decode_for_key(env, unarchiver, int_key).cloned();
    if let Some(value) = int_val {
        if let Some(longlong) = value.as_signed_integer() {
            return msg![env; num initWithLongLong:longlong];
        }
        if let Some(ulonglong) = value.as_unsigned_integer() {
            return msg![env; num initWithUnsignedLongLong:ulonglong];
        }
        if let Some(longlong) = coerce_int(&value) {
            return msg![env; num initWithLongLong:longlong];
        }
    }
    let dbl_val: Option<Value> = get_value_to_decode_for_key(env, unarchiver, dbl_key).cloned();
    if let Some(double) = dbl_val.as_ref().and_then(coerce_real) {
        return msg![env; num initWithDouble:double];
    }
    let bool_val: Option<Value> = get_value_to_decode_for_key(env, unarchiver, bool_key).cloned();
    if let Some(int) = bool_val.as_ref().and_then(coerce_int) {
        let boolean: bool = int != 0;
        return msg![env; num initWithBool:boolean];
    }
    log!("[!] NSKeyedUnarchiver decode_current_number: 缺 NS.intval/NS.dblval/NS.boolval — 按 0 处理");
    let zero: i64 = 0;
    msg![env; num initWithLongLong:zero]
}

/// [2026-09-16 黄金岛审查修] 坏档护栏:原来这里 `current_key.unwrap()` / `as_dictionary().unwrap()` /
/// `[key].as_array().unwrap()` / 每个元素 `as_uid().unwrap()` 全会 panic。NS.keys / NS.objects 被改坏
/// (磁盘坏块、外部工具编辑、旧版非原子写留下的半新半旧内容)就整个模拟器退出,坏档隔离轮不到。
/// 现在任一环节不符合预期就记一行日志、返回空集合 —— 容器解成空,与「档里本来就是空容器」同构,
/// 上层的 count==0 / 字段缺失护栏能正常接管。
fn keys_for_key(env: &mut Environment, unarchiver: id, key: &str) -> Vec<Uid> {
    let host_obj = borrow_host_obj(env, unarchiver);
    let objects = host_obj.plist["$objects"].as_array().unwrap();
    let Some(current_key) = host_obj.current_key else {
        log!("[!] NSKeyedUnarchiver keys_for_key({}): 没有当前解档对象 — 按空集合处理", key);
        return Vec::new();
    };
    let Some(item) = objects.get(current_key.get() as usize) else {
        log!(
            "[!] NSKeyedUnarchiver keys_for_key({}): 当前 UID {} 超出 $objects 范围(坏档?)— 按空集合处理",
            key,
            current_key.get()
        );
        return Vec::new();
    };
    let Some(keys) = item
        .as_dictionary()
        .and_then(|d| d.get(key))
        .and_then(|v| v.as_array())
    else {
        log!(
            "[!] NSKeyedUnarchiver keys_for_key({}): UID {} 的条目不是字典或缺该键/它不是数组(坏档?)— 按空集合处理",
            key,
            current_key.get()
        );
        return Vec::new();
    };
    let mut out = Vec::with_capacity(keys.len());
    for value in keys {
        let Some(uid) = value.as_uid().copied() else {
            log!(
                "[!] NSKeyedUnarchiver keys_for_key({}): UID {} 的 {} 数组里有非 UID 元素(坏档?)— 丢弃该元素",
                key,
                current_key.get(),
                key
            );
            continue;
        };
        out.push(uid);
    }
    out
}
