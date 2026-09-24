/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The `NSArray` class cluster, including `NSMutableArray`.

use super::ns_enumerator::{fast_enumeration_helper, NSFastEnumerationState};
use super::ns_property_list_serialization::{
    deserialize_plist_from_file, NSPropertyListBinaryFormat_v1_0,
};
use super::{
    _nib_archive_decoder, ns_keyed_unarchiver, ns_string, ns_url, NSComparisonResult, NSNotFound,
    NSRange, NSUInteger,
};
use crate::abi::{CallFromHost, DotDotDot, GuestFunction};
use crate::frameworks::foundation::ns_keyed_archiver::{
    encode_object, get_value_to_encode_for_current_key,
};
use crate::fs::GuestPath;
use crate::libc::stdlib::qsort::qsort_generic;
use crate::mem::{guest_size_of, ConstPtr, GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain, Class,
    ClassExports, HostObject, NSZonePtr, SEL,
};
use crate::Environment;

struct ObjectEnumeratorHostObject {
    /// the enumerated collection, NSArray *
    array: id,
    /// an iterator
    iterator: std::vec::IntoIter<id>,
}
impl HostObject for ObjectEnumeratorHostObject {}

/// Belongs to _touchHLE_NSArray
#[derive(Debug, Default)]
pub(super) struct ArrayHostObject {
    pub(super) array: Vec<id>,
}
impl HostObject for ArrayHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// NSArray is an abstract class. A subclass must provide:
// - (NSUInteger)count;
// - (id)objectAtIndex:(NSUInteger)index;
// We can pick whichever subclass we want for the various alloc methods.
// For the time being, that will always be _touchHLE_NSArray.
@implementation NSArray: NSObject

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSArray might be subclassed by something which needs allocWithZone:
    // to have the normal behaviour. Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSArray", &mut env.mem));
    msg_class![env; _touchHLE_NSArray allocWithZone:zone]
}

+ (id)array {
    let array: id = msg![env; this new];
    autorelease(env, array)
}

+ (id)arrayWithArray:(id)other { // NSArray*
    let array: id = msg![env; this alloc];
    let array: id = msg![env; array initWithArray:other];
    autorelease(env, array)
}

// These probably comes from some category related to plists.
+ (id)arrayWithContentsOfFile:(id)path { // NSString*
    let array: id = msg![env; this alloc];
    let array: id = msg![env; array initWithContentsOfFile:path];
    autorelease(env, array)
}
+ (id)arrayWithContentsOfURL:(id)url { // NSURL*
    let array: id = msg![env; this alloc];
    let array: id = msg![env; array initWithContentsOfURL:url];
    autorelease(env, array)
}

// [深扫修 2026-09-11] 根因:原 +arrayWithObject: / 变参 +arrayWithObjects: 调 from_vec,
// 里面写死 `NSArray alloc`,不看接收者类。NSMutableArray 没覆盖 arrayWithObject:,于是
// `[NSMutableArray arrayWithObject:x]` 返回不可变的 _touchHLE_NSArray,后续 addObject:
// 被 messages.rs 当 no-op 吞掉。实证:-[NewSceneData loadObjectUpgradeDataWithFileName:]
// (0x21a10c 建数组 / 0x21a11a 追加)加载 levelupHV.dat 时,每个建筑 ID 只剩 1 级数据,
// 布兰的家 2-6 级、商店 2-4 级查表恒 nil → 升级免费秒完成、餐厅 2 级起停产、雇佣上限不涨。
// 修法:改为 `[[this alloc] initWithObjects:count:]`(与下面 +arrayWithObjects:count: 同一写法),
// 由接收者类决定可变性;_touchHLE_NSMutableArray 已补齐 initWithObjects:count:(见下)。
// 注意不能直接 borrow_mut 刚 alloc 出的对象:guest 自定义子类 alloc 出来的对象没有 ArrayHostObject。
+ (id)arrayWithObject:(id)object {
    let array = new_array_of_receiver_class(env, this, &[object]);
    autorelease(env, array)
}
+ (id)arrayWithObjects:(id)firstObj, ...args {
    // 先收集(helper 会 retain),构造时 init 会再 retain 一次,所以随后逐个 release 抵消。
    let objects = retained_objects_from_varargs(env, firstObj, args);
    let array = new_array_of_receiver_class(env, this, &objects);
    for object in objects {
        release(env, object);
    }
    autorelease(env, array)
}
+ (id)arrayWithObjects:(ConstPtr<id>)objects_ptr count:(NSUInteger)count {
    let array: id = msg![env; this alloc];
    let array: id = msg![env; array initWithObjects:objects_ptr count:count];
    autorelease(env, array)
}

// These probably comes from some category related to plists.
- (id)initWithContentsOfFile:(id)path { // NSString*
    release(env, this);
    let path = ns_string::to_rust_string(env, path);
    deserialize_plist_from_file(
        env,
        GuestPath::new(&path),
        /* array_expected: */ true,
    )
}
- (id)initWithContentsOfURL:(id)url { // NSURL*
    release(env, this);
    let path = ns_url::to_rust_path(env, url);
    deserialize_plist_from_file(env, &path, /* array_expected: */ true)
}

- (bool)writeToFile:(id)path // NSString*
         atomically:(bool)atomically {
    let error_desc: MutPtr<id> = Ptr::null();
    let data: id = msg_class![env; NSPropertyListSerialization
            dataFromPropertyList:this
                          format:NSPropertyListBinaryFormat_v1_0
                errorDescription:error_desc];
    let res = msg![env; data writeToFile:path atomically:atomically];
    log_dbg!(
        "[(NSArray *){:?} writeToFile:{:?} atomically:{}] -> {}",
        this,
        ns_string::to_rust_string(env, path),
        atomically,
        res
    );
    res
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

- (NSUInteger)indexOfObject:(id)object {
    let count: NSUInteger = msg![env; this count];
    for i in 0..count {
        let curr_object: id = msg![env; this objectAtIndex:i];
        let equal: bool = msg![env; object isEqual:curr_object];
        if equal {
            return i;
        }
    }
    NSNotFound as NSUInteger
}
- (bool)containsObject:(id)object {
    let idx: NSUInteger = msg![env; this indexOfObject:object];
    idx != NSNotFound as NSUInteger
}

- (id)firstObject {
    let size: NSUInteger = msg![env; this count];
    if size == 0 {
        return nil;
    }
    msg![env; this objectAtIndex:0u32]
}

- (id)lastObject {
    let size: NSUInteger = msg![env; this count];
    if size == 0 {
        return nil;
    }
    msg![env; this objectAtIndex:(size - 1)]
}

- (id)componentsJoinedByString:(id)str { // NSString *
    let res: id = msg_class![env; NSMutableString new];
    let count: NSUInteger = msg![env; this count];
    if count == 0 {
        autorelease(env, res);
        return res;
    }
    for i in 0..count {
        let curr_object: id = msg![env; this objectAtIndex:i];
        let curr_desc: id = msg![env; curr_object description];
        () = msg![env; res appendString:curr_desc];
        if i != count-1 {
            () = msg![env; res appendString:str];
        }
    }
    let res_imm = msg![env; res copy];
    release(env, res);
    autorelease(env, res_imm)
}

- (id)sortedArrayUsingFunction:(GuestFunction)comparator
                       context:(MutVoidPtr)context {
    let array = msg![env; this mutableCopy];
    () = msg![env; array sortUsingFunction:comparator context:context];
    let array_imm = msg![env; array copy];
    release(env, array);
    autorelease(env, array_imm)
}

- (NSUInteger)hash {
    // TODO: define better hash
    msg![env; this count]
}
- (bool)isEqual:(id)other {
    if this == other {
        return true;
    }
    let class: Class = msg_class![env; NSArray class];
    if !msg![env; other isKindOfClass:class] {
        return false;
    }
    msg![env; this isEqualToArray:other]
}
- (bool)isEqualToArray:(id)other { // NSArray *
    if other == nil {
        return false;
    }
    let count: NSUInteger = msg![env; this count];
    let other_count: NSUInteger = msg![env; other count];
    if count != other_count {
        return false;
    }
    for i in 0..count {
        let curr_object: id = msg![env; this objectAtIndex:i];
        let curr_other_object: id = msg![env; other objectAtIndex:i];
        let equal: bool = msg![env; curr_object isEqual:curr_other_object];
        if !equal {
            return false;
        }
    }
    true
}

- (id)filteredArrayUsingPredicate:(id)predicate { // NSPredicate*
    let count: NSUInteger = msg![env; this count];
    let mut kept = Vec::new();
    for i in 0..count {
        let object: id = msg![env; this objectAtIndex:i];
        let matches: bool = msg![env; predicate evaluateWithObject:object];
        if matches {
            retain(env, object);
            kept.push(object);
        }
    }
    let result = from_vec(env, kept);
    autorelease(env, result)
}

// [深扫修 2026-09-11] 根因:subarrayWithRange: / sortedArrayUsingSelector: 原先只挂在
// _touchHLE_NSArray 上,_touchHLE_NSMutableArray(继承链 NSMutableArray→NSArray)拿不到,
// 对可变数组调用会静默返回 nil。修法:上移到抽象 NSArray,只用 count / objectAtIndex: /
// mutableCopy 等消息实现、不碰 ArrayHostObject,因此对 guest 自定义子类也安全。
// 返回值按 Apple 语义一律是不可变 NSArray(与上面 sortedArrayUsingFunction:context: 一致)。
- (id)subarrayWithRange:(NSRange)range {
    let count: NSUInteger = msg![env; this count];
    // NSRange 是 packed 结构体,格式化宏会取字段引用,先拷到局部变量。
    let (location, length) = (range.location, range.length);
    let end = location.saturating_add(length);
    if location > count || end > count {
        // 真机会抛 NSRangeException;原实现是切片越界直接 panic 整个模拟器。
        // 这里显式检查,打日志后截到合法区间,避免一次越界拖垮整个游戏。
        log!(
            "[深扫修] -[NSArray subarrayWithRange:] 越界: location={} length={} count={},截断到合法区间",
            location,
            length,
            count
        );
    }
    let start = location.min(count);
    let end = end.min(count);
    let mut objects = Vec::new();
    for i in start..end {
        let object: id = msg![env; this objectAtIndex:i];
        retain(env, object);
        objects.push(object);
    }
    let res = from_vec(env, objects);
    autorelease(env, res)
}

- (id)sortedArrayUsingSelector:(SEL)comparator {
    let array: id = msg![env; this mutableCopy];
    () = msg![env; array sortUsingSelector:comparator];
    let array_imm: id = msg![env; array copy];
    release(env, array);
    autorelease(env, array_imm)
}

@end

// NSMutableArray is an abstract class. A subclass must provide everything
// NSArray provides, plus:
// - (void)insertObject:(id)object atIndex:(NSUInteger)index;
// - (void)removeObjectAtIndex:(NSUInteger)index;
// - (void)addObject:(id)object;
// - (void)removeLastObject
// - (void)replaceObjectAtIndex:(NSUInteger)index withObject:(id)object;
// Note that it inherits from NSArray, so we must ensure we override any default
// methods that would be inappropriate for mutability.
@implementation NSMutableArray: NSArray

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSArray might be subclassed by something which needs allocWithZone:
    // to have the normal behaviour. Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSMutableArray", &mut env.mem));
    msg_class![env; _touchHLE_NSMutableArray allocWithZone:zone]
}

+ (id)arrayWithCapacity:(NSUInteger)capacity {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCapacity:capacity];
    autorelease(env, new)
}

+ (id)arrayWithArray:(id)array {
    let new: id = msg![env; this alloc];
    () = msg![env; new addObjectsFromArray:array];
    autorelease(env, new)
}

+ (id)arrayWithObjects:(id)firstObj, ...args {
    // [深扫修 2026-09-11] 改用共享 helper:首个参数为 nil 时按 Apple 语义得到空数组
    // (原实现会把 nil 当成第 1 个元素,count 变成 1)。
    let objects = retained_objects_from_varargs(env, firstObj, args);
    let array = mutable_from_vec(env, objects);
    autorelease(env, array)
}

// These probably comes from some category related to plists.
- (id)initWithContentsOfFile:(id)path { // NSString*
    release(env, this);
    let path = ns_string::to_rust_string(env, path);
    let tmp = deserialize_plist_from_file(
        env,
        GuestPath::new(&path),
        /* array_expected: */ true,
    );
    if tmp == nil {
        return nil;
    }
    // We should respect mutability of the top most container!
    let res = msg_class![env; NSMutableArray alloc];
    let res = msg![env; res initWithArray:tmp];
    release(env, tmp);
    res
}
- (id)initWithContentsOfURL:(id)url { // NSURL*
    release(env, this);
    let path = ns_url::to_rust_path(env, url);
    let tmp = deserialize_plist_from_file(env, &path, /* array_expected: */ true);
    if tmp == nil {
        return nil;
    }
    // We should respect mutability of the top most container!
    let res = msg_class![env; NSMutableArray alloc];
    let res = msg![env; res initWithArray:tmp];
    release(env, tmp);
    res
}

- (())addObjectsFromArray:(id)other { // NSArray*
    let enumerator: id = msg![env; other objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        () = msg![env; this addObject:next];
    }
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    let other: id = msg_class![env; NSArray alloc];
    let other: id = msg![env; other initWithArray:this];
    other
}

@end

// Our private subclass that is the single implementation of NSArray for the
// time being.
@implementation _touchHLE_NSArray: NSArray

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(ArrayHostObject {
        array: Vec::new(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    init_with_coder_inner(env, this, coder)
}
- (())encodeWithCoder:(id)coder {
    encode_with_coder_inner(env, this, coder)
}

- (id)initWithArray:(id)array { // NSArray*
    let mut objects = Vec::new();
    let enumerator: id = msg![env; array objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        objects.push(next);
        retain(env, next);
    }
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

// [深扫修 2026-09-11] 两个 init 的主体抽成共享 helper,与 _touchHLE_NSMutableArray 共用。
- (id)initWithObjects:(id)firstObj, ...args {
    let objects = retained_objects_from_varargs(env, firstObj, args);
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

- (id)initWithObjects:(ConstPtr<id>)objects_ptr count:(NSUInteger)count {
    let objects = retained_objects_from_ptr(env, objects_ptr, count);
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

- (())dealloc {
    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(this);
    let array = std::mem::take(&mut host_object.array);

    for object in array {
        release(env, object);
    }

    env.objc.dealloc_object(this, &mut env.mem)
}

// NSMutableCopying implementation
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    mutable_copy_inner(env, this)
}

- (id)objectEnumerator { // NSEnumerator*
    object_enumerator_inner(env, this)
}
- (id)reverseObjectEnumerator { // NSEnumerator*
    reverse_object_enumerator_inner(env, this)
}

// NSFastEnumeration implementation
- (NSUInteger)countByEnumeratingWithState:(MutPtr<NSFastEnumerationState>)state
                                  objects:(MutPtr<id>)stackbuf
                                    count:(NSUInteger)len {
    let count: NSUInteger = msg![env; this count];
    fast_enumeration_helper(env, this, |env, idx| {
        if idx < count {
            msg![env; this objectAtIndex:idx]
        } else {
            nil
        }
    }, state, stackbuf, len)
}

// TODO: more init methods, etc

- (NSUInteger)count {
    env.objc.borrow::<ArrayHostObject>(this).array.len().try_into().unwrap()
}
- (id)objectAtIndex:(NSUInteger)index {
    // TODO: throw real exception rather than panic if out-of-bounds?
    env.objc.borrow::<ArrayHostObject>(this).array[index as usize]
}

- (id)description {
    build_description(env, this)
}

// [深扫修 2026-09-11] subarrayWithRange: / sortedArrayUsingSelector: 已上移到抽象 NSArray
// (可变数组也能用,且越界不再 panic),这里不再重复定义。

@end

// Special variant for use by CFArray with NULL callbacks: objects aren't
// necessarily Objective-C objects and won't be retained/released.
@implementation _touchHLE_NSArray_non_retaining: _touchHLE_NSArray

- (())dealloc {
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

@implementation _touchHLE_NSArray_ObjectEnumerator: NSEnumerator

- (id)nextObject {
    let host_obj = env.objc.borrow_mut::<ObjectEnumeratorHostObject>(this);
    host_obj.iterator.next().map_or(nil, |o| o)
}

- (())dealloc {
    let host_obj = env.objc.borrow::<ObjectEnumeratorHostObject>(this);
    release(env, host_obj.array);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

// Our private subclass that is the single implementation of NSMutableArray for
// the time being.
@implementation _touchHLE_NSMutableArray: NSMutableArray

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(ArrayHostObject {
        array: Vec::new(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithCapacity:(NSUInteger)capacity {
    env.objc.borrow_mut::<ArrayHostObject>(this).array.reserve(capacity as usize);
    this
}

// [深扫修 2026-09-11] 根因:initWithObjects: / initWithObjects:count: 原先只在兄弟类
// _touchHLE_NSArray 上,本类继承链是 NSMutableArray→NSArray→NSObject,拿不到,于是
// `[[NSMutableArray alloc] initWithObjects:...]` 与 `[NSMutableArray arrayWithObjects:count:]`
// 都落到"不响应选择子"→ 返回 nil(alloc 出的对象还泄漏)。实证可达点:-[TMAHTTPRequest init]
// (0x4b072c)用它初始化 13 个 passport 请求参数表,全为 nil → 账号菜单模式下请求表单字段全丢。
// 修法:在本类显式实现(不上移到抽象 NSArray:guest 子类如 JKArray 没有 ArrayHostObject,
// 在抽象层 borrow_mut 会 panic),主体与 _touchHLE_NSArray 共用 helper。
- (id)initWithObjects:(id)firstObj, ...args {
    let objects = retained_objects_from_varargs(env, firstObj, args);
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

- (id)initWithObjects:(ConstPtr<id>)objects_ptr count:(NSUInteger)count {
    let objects = retained_objects_from_ptr(env, objects_ptr, count);
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

- (id)initWithArray:(id)array { // NSArray*
    let mut objects = Vec::new();
    let enumerator: id = msg![env; array objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        objects.push(next);
        retain(env, next);
    }
    env.objc.borrow_mut::<ArrayHostObject>(this).array = objects;
    this
}

// NSCoding implementation
- (id)initWithCoder:(id)coder {
    init_with_coder_inner(env, this, coder)
}
- (())encodeWithCoder:(id)coder {
    encode_with_coder_inner(env, this, coder)
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    let arr: id = msg_class![env; NSArray alloc];
    let array = env.objc.borrow::<ArrayHostObject>(this).array.clone();
    for &object in &array {
        retain(env, object);
    }
    env.objc.borrow_mut::<ArrayHostObject>(arr).array = array;
    arr
}

// NSMutableCopying implementation
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    mutable_copy_inner(env, this)
}

- (())dealloc {
    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(this);
    let array = std::mem::take(&mut host_object.array);

    for object in array {
        release(env, object);
    }

    env.objc.dealloc_object(this, &mut env.mem)
}

- (())makeObjectsPerformSelector:(SEL)sel {
    let count: NSUInteger = msg![env; this count];
    for idx in 0..count {
        let obj: id = msg![env; this objectAtIndex:idx];
        let _: id = msg![env; obj performSelector:sel];
    }
}

- (id)objectEnumerator { // NSEnumerator*
    object_enumerator_inner(env, this)
}
- (id)reverseObjectEnumerator { // NSEnumerator*
    reverse_object_enumerator_inner(env, this)
}

- (())sortUsingFunction:(GuestFunction)comparator
                context:(MutVoidPtr)context {
    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(this);
    let mut array = std::mem::take(&mut host_object.array);
    let len = array.len().try_into().unwrap();
    let mut user_data = (env, &mut array);
    qsort_generic(
        &mut user_data,
        len,
        &mut |(env, array), l, r| {
            let (l, r): (usize, usize) = (l.try_into().unwrap(), r.try_into().unwrap());
            comparator.call_from_host(env, (array[l], array[r], context))
        },
        &mut |(_, array), l, r| {
            let (l, r): (usize, usize) = (l.try_into().unwrap(), r.try_into().unwrap());
            array.swap(l, r);
        },
    );
    let (env, _) = user_data;
    env.objc.borrow_mut::<ArrayHostObject>(this).array = array;
}

- (())sortUsingSelector:(SEL)comparator {
    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(this);
    let mut array = std::mem::take(&mut host_object.array);
    let len = array.len().try_into().unwrap();
    let mut user_data = (env, &mut array);
    qsort_generic(
        &mut user_data,
        len,
        &mut |(env, array), l, r| {
            let (l, r): (usize, usize) = (l.try_into().unwrap(), r.try_into().unwrap());
            let res: NSComparisonResult = msg_send(env, (array[l], comparator, array[r]));
            res
        },
        &mut |(_, array), l, r| {
            let (l, r): (usize, usize) = (l.try_into().unwrap(), r.try_into().unwrap());
            array.swap(l, r);
        },
    );

    let (env, _) = user_data;
    env.objc.borrow_mut::<ArrayHostObject>(this).array = array;
}

// NSFastEnumeration implementation
- (NSUInteger)countByEnumeratingWithState:(MutPtr<NSFastEnumerationState>)state
                                  objects:(MutPtr<id>)stackbuf
                                    count:(NSUInteger)len {
    // TODO: check that array wasn't mutated!
    let count: NSUInteger = msg![env; this count];
    fast_enumeration_helper(env, this, |env, idx| {
        if idx < count {
            msg![env; this objectAtIndex:idx]
        } else {
            nil
        }
    }, state, stackbuf, len)
}

- (NSUInteger)count {
    env.objc.borrow::<ArrayHostObject>(this).array.len().try_into().unwrap()
}
- (id)objectAtIndex:(NSUInteger)index {
    // TODO: throw real exception rather than panic if out-of-bounds?
    env.objc.borrow::<ArrayHostObject>(this).array[index as usize]
}

- (id)description {
    build_description(env, this)
}

// ---- 坏档兜底:把"被当成字典用的损坏数组"安全降级为"空字典" ----
// 个别旧存档因 NSKeyedArchiver 去重 bug(已在 ns_keyed_archiver.rs 治本)把本该是
// NSMutableDictionary 的字段(UserInfoData.achieveUnlock / attributeValue,或 mapData)
// 写成了 NSMutableArray。游戏随后仍对它发字典消息(allKeys / objectForKey: / setObject:forKey:)。
// 这些 selector 原先走 messages.rs 的 "无此方法 → 返回 nil" 兜底,后果:
//   1) GameManager loadMapByStep 的分步加载进度依赖 [mapData allKeys],拿到 nil → 进度
//      永远推不动 → 每帧重注册定时器 → 进入游戏页死循环卡死(玩家报"再进入闪退")。
//   2) VillageMenuLayer onEnter 把 achieveUnlock/attributeValue 当字典查 → 刷 allKeys 警告风暴。
// 让损坏数组对字典消息表现为"空字典":allKeys 返回空数组(count=0 而非 nil),分步加载
// 进度立即收敛(count==0 → 结束 → 取消定时器),死循环被打破;并置坏档标志供 mole_cheats
// 抑制成就重复触发(见 SAVE_HAS_DICT_AS_ARRAY)。NSArray/NSMutableArray 本无这些 selector,无冲突。
- (id)allKeys { // NSArray*
    crate::mole_cheats::note_dict_as_array_corruption();
    let empty: id = from_vec(env, Vec::new());
    autorelease(env, empty)
}
- (id)allValues { // NSArray*
    crate::mole_cheats::note_dict_as_array_corruption();
    let empty: id = from_vec(env, Vec::new());
    autorelease(env, empty)
}
- (id)objectForKey:(id)_key {
    crate::mole_cheats::note_dict_as_array_corruption();
    nil
}
- (())setObject:(id)_object forKey:(id)_key {
    // 坏档伪字典:无法在数组上按 key 存,静默吞(不崩、不写脏数据)。
    crate::mole_cheats::note_dict_as_array_corruption();
}
- (())removeObjectForKey:(id)_key {
    crate::mole_cheats::note_dict_as_array_corruption();
}

// TODO: more mutation methods

- (())insertObject:(id)object
           atIndex:(NSUInteger)index {
    retain(env, object);
    env.objc.borrow_mut::<ArrayHostObject>(this).array.insert(index as usize, object);
}

- (())addObject:(id)object {
    retain(env, object);
    env.objc.borrow_mut::<ArrayHostObject>(this).array.push(object);
}

- (())removeObject:(id)object {
    let mut to_remove = Vec::new();
    let count: NSUInteger = msg![env; this count];
    for i in 0..count {
        let curr_object: id = msg![env; this objectAtIndex:i];
        let equal: bool = msg![env; object isEqual:curr_object];
        if equal {
            to_remove.push(i);
        }
    }
    // TODO: runtime here is O(n^2), it could be O(n) instead
    // [扫描修 2026-09-15] 倒序删:正序删时前一次删除会让后面的下标整体前移,删错对象甚至越界 panic。
    for i in to_remove.into_iter().rev() {
        () = msg![env; this removeObjectAtIndex:i];
    }
}

- (())removeObjectsInArray:(id)other { // NSArray*
    // [扫描修 2026-09-15] 缺这个方法 → TMA_SSKeychain(MOLE_REAL_KEYCHAIN=1 时真跑)等调用处 unrecognized selector。
    // [复核修 2026-09-15] 默认模式下也有真实调用点:-[NetworkManager parseFriendInfoData:pos:len:islocal:]
    // @0xe4b3e(从 [GameData sharedInstance].latestVisitedUsersInfoList 删掉循环里筛出的来访记录)、
    // -[CCRibbon addPointAt:width:]@0x2ddb2e(segments_ 删 deletedSegments_)。
    // 语义同 Apple:对 other 里每个对象按 isEqual: 删除所有相等项。先取出并 retain,
    // 防止 other 与 this 是同一个数组(或持有唯一引用)时边删边释放。
    let count: NSUInteger = msg![env; other count];
    let mut objects = Vec::with_capacity(count as usize);
    for i in 0..count {
        let obj: id = msg![env; other objectAtIndex:i];
        retain(env, obj);
        objects.push(obj);
    }
    for &obj in &objects {
        () = msg![env; this removeObject:obj];
    }
    for obj in objects {
        release(env, obj);
    }
}

- (())removeObjectAtIndex:(NSUInteger)index {
    let object = env.objc.borrow_mut::<ArrayHostObject>(this).array.remove(index as usize);
    release(env, object)
}

- (())replaceObjectAtIndex:(NSUInteger)index withObject:(id)obj {
    retain(env, obj);
    let object = std::mem::replace(&mut env.objc.borrow_mut::<ArrayHostObject>(this).array[index as usize], obj);
    release(env, object);
}

- (())exchangeObjectAtIndex:(NSUInteger)idx1 withObjectAtIndex:(NSUInteger)idx2 {
    let array = &mut env.objc.borrow_mut::<ArrayHostObject>(this).array;
    array.swap(idx1 as usize, idx2 as usize);
}

- (())removeLastObject {
    let object = env.objc.borrow_mut::<ArrayHostObject>(this).array.pop().unwrap();
    release(env, object)
}

- (())removeAllObjects {
    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(this);
    let array = std::mem::take(&mut host_object.array);
    for object in array {
        release(env, object);
    }

    env.objc.borrow_mut::<ArrayHostObject>(this).array = Vec::new()
}

@end

// Special variant for use by CFArray with NULL callbacks: objects aren't
// necessarily Objective-C objects and won't be retained/released.
@implementation _touchHLE_NSMutableArray_non_retaining: _touchHLE_NSMutableArray

- (())dealloc {
    env.objc.dealloc_object(this, &mut env.mem)
}

- (())addObject:(id)object {
    env.objc.borrow_mut::<ArrayHostObject>(this).array.push(object);
}

- (())removeObjectAtIndex:(NSUInteger)index {
    env.objc.borrow_mut::<ArrayHostObject>(this).array.remove(index as usize);
}

- (())removeLastObject {
    env.objc.borrow_mut::<ArrayHostObject>(this).array.pop().unwrap();
}

@end

};

/// Shortcut for host code, roughly equivalent to
/// `[[NSArray alloc] initWithObjects:count]` but without copying.
/// The elements should already be "retained by" the `Vec`.
pub fn from_vec(env: &mut Environment, objects: Vec<id>) -> id {
    let array: id = msg_class![env; NSArray alloc];
    env.objc.borrow_mut::<ArrayHostObject>(array).array = objects;
    array
}

/// Shortcut for host code, roughly equivalent to
/// `[[NSMutableArray alloc] initWithObjects:count]` but without copying.
/// The elements should already be "retained by" the `Vec`.
pub fn mutable_from_vec(env: &mut Environment, objects: Vec<id>) -> id {
    let array: id = msg_class![env; NSMutableArray alloc];
    env.objc.borrow_mut::<ArrayHostObject>(array).array = objects;
    array
}

/// [深扫修 2026-09-11] 收集以 nil 结尾的可变参数对象列表,逐个 retain(返回的 `Vec` 持有这些引用)。
/// 供 `initWithObjects:` / `+arrayWithObjects:` 共用。
/// 按 Apple 语义:首个参数就是 nil 时列表为空(原实现会把 nil 当成第 1 个元素)。
fn retained_objects_from_varargs(env: &mut Environment, first_obj: id, args: DotDotDot) -> Vec<id> {
    let mut objects = Vec::new();
    if first_obj.is_null() {
        return objects;
    }
    retain(env, first_obj);
    objects.push(first_obj);
    let mut varargs = args.start();
    loop {
        let next_arg: id = varargs.next(env);
        if next_arg.is_null() {
            break;
        }
        retain(env, next_arg);
        objects.push(next_arg);
    }
    objects
}

/// [深扫修 2026-09-11] 从 guest 的 `id` 数组读取 `count` 个对象,逐个 retain。
/// 供两个具体类的 `initWithObjects:count:` 共用。
fn retained_objects_from_ptr(
    env: &mut Environment,
    objects_ptr: ConstPtr<id>,
    count: NSUInteger,
) -> Vec<id> {
    let mut objects = Vec::with_capacity(count as usize);
    for i in 0..count {
        let obj: id = env.mem.read(objects_ptr + i);
        retain(env, obj);
        objects.push(obj);
    }
    objects
}

/// [深扫修 2026-09-11] 等价于 `[[this alloc] initWithObjects:objects count:n]`,返回 +1 引用。
/// 用于类方法工厂:让接收者类(NSArray / NSMutableArray / guest 子类)决定构造出的具体类型,
/// 而不是像 [from_vec] 那样写死 NSArray。`initWithObjects:count:` 需要 guest 指针,
/// 所以临时在 guest 堆上放一份对象指针数组,调用完立即释放(init 内部会自行 retain 元素)。
fn new_array_of_receiver_class(env: &mut Environment, this: Class, objects: &[id]) -> id {
    let count: NSUInteger = objects.len().try_into().unwrap();
    // 至少分配 1 个槽,避免 alloc(0)。
    let slots: GuestUSize = count.max(1) * guest_size_of::<id>();
    let buf: MutPtr<id> = env.mem.alloc(slots).cast();
    for (i, &object) in objects.iter().enumerate() {
        let i: GuestUSize = i.try_into().unwrap();
        env.mem.write(buf + i, object);
    }
    let array: id = msg![env; this alloc];
    let array: id = msg![env; array initWithObjects:(buf.cast_const()) count:count];
    env.mem.free(buf.cast());
    array
}

/// A helper to build a description NSString
/// for a NSArray or a NSMutableArray.
fn build_description(env: &mut Environment, arr: id) -> id {
    // According to docs, this description should be formatted as property list.
    // But by the same docs, it's meant to be used for debugging purposes only.
    let desc: id = msg_class![env; NSMutableString new];
    let prefix: id = ns_string::from_rust_string(env, "(\n".to_string());
    () = msg![env; desc appendString:prefix];
    release(env, prefix);
    let values: Vec<id> = env.objc.borrow_mut::<ArrayHostObject>(arr).array.clone();
    for value in values {
        let value_desc: id = msg![env; value description];
        // TODO: respect nesting and padding
        let format = format!("\t{},\n", ns_string::to_rust_string(env, value_desc));
        let format = ns_string::from_rust_string(env, format);
        () = msg![env; desc appendString:format];
        release(env, format);
    }
    let suffix: id = ns_string::from_rust_string(env, ")".to_string());
    () = msg![env; desc appendString:suffix];
    release(env, suffix);
    let desc_imm = msg![env; desc copy];
    release(env, desc);
    autorelease(env, desc_imm)
}

/// A shared objectEnumerator helper method.
fn object_enumerator_inner(env: &mut Environment, arr: id) -> id {
    let array_host_object: &mut ArrayHostObject = env.objc.borrow_mut(arr);
    let vec = array_host_object.array.to_vec();
    object_enumerator_inner_helper(env, arr, vec)
}

/// A shared reverseObjectEnumerator helper method.
fn reverse_object_enumerator_inner(env: &mut Environment, arr: id) -> id {
    let array_host_object: &mut ArrayHostObject = env.objc.borrow_mut(arr);
    // TODO: avoid copying?
    let vec = array_host_object
        .array
        .iter()
        .rev()
        .cloned()
        .collect::<Vec<_>>();
    object_enumerator_inner_helper(env, arr, vec)
}

fn object_enumerator_inner_helper(env: &mut Environment, arr: id, vec: Vec<id>) -> id {
    let host_object = Box::new(ObjectEnumeratorHostObject {
        array: arr,
        iterator: vec.into_iter(),
    });
    retain(env, arr);
    let class = env
        .objc
        .get_known_class("_touchHLE_NSArray_ObjectEnumerator", &mut env.mem);
    let enumerator = env.objc.alloc_object(class, host_object, &mut env.mem);
    autorelease(env, enumerator)
}

fn mutable_copy_inner(env: &mut Environment, arr: id) -> id {
    let mut_arr: id = msg_class![env; NSMutableArray alloc];
    let array = env.objc.borrow::<ArrayHostObject>(arr).array.clone();
    for &object in &array {
        retain(env, object);
    }
    env.objc.borrow_mut::<ArrayHostObject>(mut_arr).array = array;
    mut_arr
}

fn init_with_coder_inner(env: &mut Environment, arr: id, coder: id) -> id {
    let class: Class = msg![env; coder class];
    let keyed_unarch_class: Class = msg_class![env; NSKeyedUnarchiver class];
    let nib_archive_class: Class = msg_class![env; _touchHLE_NIBArchiveDecoder class];
    // It seems that every NSArray item in an NSKeyedArchiver plist looks like:
    // {
    //   "$class" => (uid of NSArray class goes here),
    //   "NS.objects" => [
    //     // objects here
    //   ]
    // }
    // Presumably we need to call a `decodeFooBarForKey:` method on the NSCoder
    // here, passing in an NSString for "NS.objects". There is no method for
    // arrays though (maybe it's `decodeObjectForKey:`), and in any case
    // allocating an NSString here would be inconvenient, so let's just take a
    // shortcut.
    let objects = if env.objc.class_is_subclass_of(class, keyed_unarch_class) {
        ns_keyed_unarchiver::decode_current_array(env, coder)
    } else if env.objc.class_is_subclass_of(class, nib_archive_class) {
        _nib_archive_decoder::decode_current_array(env, coder)
    } else {
        unimplemented!()
    };

    let host_object: &mut ArrayHostObject = env.objc.borrow_mut(arr);
    assert!(host_object.array.is_empty());
    host_object.array = objects; // objects are already retained
    arr
}

fn encode_with_coder_inner(env: &mut Environment, arr: id, coder: id) {
    let host_obj: ArrayHostObject = std::mem::take(env.objc.borrow_mut(arr));
    let mut encoded_vals = vec![];
    for v in &host_obj.array {
        // TODO: support other type of coders, not only NSKeyedArchiver
        let vv = encode_object(env, coder, *v);
        encoded_vals.push(plist::Value::Uid(vv));
    }
    *env.objc.borrow_mut(arr) = host_obj;

    let scope = get_value_to_encode_for_current_key(env, coder);
    scope.insert("NS.objects".to_string(), plist::Value::Array(encoded_vals));
}
