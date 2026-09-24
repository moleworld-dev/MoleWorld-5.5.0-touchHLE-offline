/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! The `NSSet` class cluster, including `NSMutableSet` and `NSCountedSet`.

use super::ns_array;
use super::ns_dictionary::DictionaryHostObject;
use super::ns_enumerator::{fast_enumeration_helper, NSFastEnumerationState};
use super::NSUInteger;
use crate::abi::DotDotDot;
use crate::environment::Environment;
use crate::mem::MutPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, retain, ClassExports, HostObject, NSZonePtr,
};

/// Belongs to _touchHLE_NSSet
#[derive(Debug, Default)]
struct SetHostObject {
    dict: DictionaryHostObject,
}
impl HostObject for SetHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// NSSet is an abstract class. A subclass must provide:
// - (NSUInteger)count;
// - (id)member:(id)object;
// - (NSEnumerator*)objectEnumerator;
// We can pick whichever subclass we want for the various alloc methods.
// For the time being, that will always be _touchHLE_NSSet.
@implementation NSSet: NSObject

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSSet might be subclassed by something which needs allocWithZone:
    // to have the normal behaviour. Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSSet", &mut env.mem));
    msg_class![env; _touchHLE_NSSet allocWithZone:zone]
}

+ (id)set {
    let set: id = msg![env; this new];
    autorelease(env, set)
}

+ (id)setWithObject:(id)object {
    assert!(object != nil);
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithObject:object];
    autorelease(env, new)
}

+ (id)setWithObjects:(id)first_obj, ...args {
    assert!(this == env.objc.get_known_class("NSSet", &mut env.mem));
    let new: id = msg![env; this alloc];
    env.objc.borrow_mut::<SetHostObject>(new).dict = set_from_objects(env, first_obj, args);
    autorelease(env, new)
}

// [深扫修 2026-09-12] 游戏二进制引用了 setWithArray:/setWithSet:/initWithArray:/initWithSet:/
// minusSet:,引擎原先一个都没有,消息落空返回 nil,调用方拿到 nil 集合后 containsObject:
// 恒为 NO,属于静默错误。实现只依赖 objectEnumerator,数组和集合都适用;类方法经
// `this alloc` 分派,NSMutableSet 调用时得到可变集合。
+ (id)setWithArray:(id)array {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithArray:array];
    autorelease(env, new)
}

+ (id)setWithSet:(id)set {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithSet:set];
    autorelease(env, new)
}

// NSCopying implementation
- (id)copyWithZone:(NSZonePtr)_zone {
    retain(env, this)
}

// NSMutableCopying implementation. Returns a new (independent) NSMutableSet
// containing the same objects, owned by the caller (+1), per convention.
// Defined on NSSet so every subclass (including _touchHLE_NSMutableSet)
// inherits it. cocos2d-iphone's CCTouchDispatcher does `[touches mutableCopy]`
// and only dispatches to *standard* touch delegates when that copy is
// non-empty; without this method the copy was nil, so standard delegates
// (e.g. MoleWorld's VillageLayer / StoryLayer) never received any touches.
- (id)mutableCopyWithZone:(NSZonePtr)_zone {
    let new: id = msg_class![env; NSMutableSet alloc];
    let new: id = msg![env; new init];
    let enumerator: id = msg![env; this objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        () = msg![env; new addObject:next];
    }
    new
}

- (bool)containsObject:(id)object {
    let enumerator: id = msg![env; this objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            return false;
        }
        if msg![env; next isEqual:object] {
            return true;
        }
    }
}

@end

// NSMutableSet is an abstract class. A subclass must provide everything
// NSSet provides, plus:
// - (void)addObject:(id)object;
// - (void)removeObject:(id)object;
// Note that it inherits from NSSet, so we must ensure we override any default
// methods that would be inappropriate for mutability.
@implementation NSMutableSet: NSSet

+ (id)allocWithZone:(NSZonePtr)zone {
    // NSSet might be subclassed by something which needs allocWithZone:
    // to have the normal behaviour. Unimplemented: call superclass alloc then.
    assert!(this == env.objc.get_known_class("NSMutableSet", &mut env.mem));
    msg_class![env; _touchHLE_NSMutableSet allocWithZone:zone]
}

+ (id)setWithObjects:(id)first_obj, ...args {
    assert!(this == env.objc.get_known_class("NSMutableSet", &mut env.mem));
    let new: id = msg![env; this alloc];
    env.objc.borrow_mut::<SetHostObject>(new).dict = set_from_objects(env, first_obj, args);
    autorelease(env, new)
}

+ (id)setWithCapacity:(NSUInteger)capacity {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithCapacity:capacity];
    autorelease(env, new)
}

// NSCopying implementation
// [深扫修 2026-09-12] 原来是 todo!(),对可变集合发 copy 会直接 panic 整个模拟器。
// 按 Apple 语义返回独立的不可变 NSSet(调用方持有 +1),内容为当前全部元素。
- (id)copyWithZone:(NSZonePtr)_zone {
    let null: id = msg_class![env; NSNull null];
    let objects: id = msg![env; this allObjects];
    let count: NSUInteger = msg![env; objects count];
    let mut dict = <DictionaryHostObject as Default>::default();
    for i in 0..count {
        let object: id = msg![env; objects objectAtIndex:i];
        dict.insert(env, object, null, /* copy_key: */ false);
    }
    let new: id = msg_class![env; NSSet alloc];
    env.objc.borrow_mut::<SetHostObject>(new).dict = dict;
    new
}

@end

// Our private subclass that is the single implementation of NSSet for the
// time being.
@implementation _touchHLE_NSSet: NSSet

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(SetHostObject {
        dict: Default::default(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithObject:(id)object {
    let null: id = msg_class![env; NSNull null];

    let mut dict = <DictionaryHostObject as Default>::default();
    dict.insert(env, object, null, /* copy_key: */ false);

    env.objc.borrow_mut::<SetHostObject>(this).dict = dict;

    this
}

- (id)initWithObjects:(id)first_obj, ...args {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_objects(env, first_obj, args);
    this
}

- (id)initWithArray:(id)array {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_collection(env, array);
    this
}

- (id)initWithSet:(id)set {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_collection(env, set);
    this
}

- (())dealloc {
    std::mem::take(&mut env.objc.borrow_mut::<SetHostObject>(this).dict).release(env);
    env.objc.dealloc_object(this, &mut env.mem)
}

// TODO: more init methods, etc

// TODO: accessors
- (NSUInteger)count {
    env.objc.borrow_mut::<SetHostObject>(this).dict.count
}

- (id)anyObject {
    let object_or_none = env.objc.borrow_mut::<SetHostObject>(this).dict.iter_keys().next();
    match object_or_none {
        Some(object) => object,
        None => nil
    }
}

- (id)allObjects {
    // [审查修 2026-09-13] 根因:原实现把宿主快照直接 from_vec 返回,数组是 +1 且从不
    // autorelease,元素也没 retain。宿主调用方(本文件的 objectEnumerator /
    // countByEnumeratingWithState: / copyWithZone: / minusSet:,以及 UIKit 触摸分发)和游戏
    // 都按 Apple 语义把返回值当 autoreleased 用、从不 release,于是每调一次泄漏一个数组;
    // 而若在调用方补 release,数组 dealloc 会对没 retain 过的元素多 release 一次。
    // 修法:按 Apple 语义,元素逐个 retain 交给数组持有(与 _touchHLE_NSArray dealloc 的逐个
    // release 配平),数组 autorelease 后返回。顺带让遍历期间集合被改也不会留下悬垂指针。
    // 取舍:没有活动池时 autorelease 仍会泄漏(与原来相同),且此时元素也随数组留存;
    // 宿主的触摸分发、NSTimer/CADisplayLink 回调都包了池,主循环路径不受影响。
    let objects: Vec<id> = env.objc.borrow::<SetHostObject>(this).dict.iter_keys().collect();
    for &object in &objects {
        retain(env, object);
    }
    let array: id = ns_array::from_vec(env, objects);
    autorelease(env, array)
}

- (id)objectEnumerator { // NSEnumerator*
    let array: id = msg![env; this allObjects];
    msg![env; array objectEnumerator]
}

// NSFastEnumeration implementation
- (NSUInteger)countByEnumeratingWithState:(MutPtr<NSFastEnumerationState>)state
                                  objects:(MutPtr<id>)stackbuf
                                    count:(NSUInteger)len {
    // We assume that order in which objects are reported is consistent
    // between calls!
    let objects: id = msg![env; this allObjects];
    let count: NSUInteger = msg![env; objects count];
    fast_enumeration_helper(env, this, |env, idx| {
        if idx < count {
            msg![env; objects objectAtIndex:idx]
        } else {
            nil
        }
    }, state, stackbuf, len)
}

@end

// Our private subclass that is the single implementation of NSMutableSet for
// the time being.
@implementation _touchHLE_NSMutableSet: NSMutableSet

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(SetHostObject {
        dict: Default::default(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithObject:(id)object {
    let null: id = msg_class![env; NSNull null];

    let mut dict = <DictionaryHostObject as Default>::default();
    dict.insert(env, object, null, /* copy_key: */ false);

    env.objc.borrow_mut::<SetHostObject>(this).dict = dict;

    this
}

- (id)initWithObjects:(id)first_obj, ...args {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_objects(env, first_obj, args);
    this
}

- (id)initWithArray:(id)array {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_collection(env, array);
    this
}

- (id)initWithSet:(id)set {
    env.objc.borrow_mut::<SetHostObject>(this).dict = set_from_collection(env, set);
    this
}

- (id)initWithCapacity:(NSUInteger)_capacity {
    // TODO: capacity
    msg![env; this init]
}

- (())dealloc {
    std::mem::take(&mut env.objc.borrow_mut::<SetHostObject>(this).dict).release(env);
    env.objc.dealloc_object(this, &mut env.mem)
}

// TODO: init methods etc

- (NSUInteger)count {
    env.objc.borrow_mut::<SetHostObject>(this).dict.count
}

- (id)anyObject {
    let object_or_none = env.objc.borrow_mut::<SetHostObject>(this).dict.iter_keys().next();
    match object_or_none {
        Some(object) => object,
        None => nil
    }
}

- (id)allObjects {
    // [审查修 2026-09-13] 同 _touchHLE_NSSet 的 allObjects:元素逐个 retain 交给数组持有,
    // 数组 autorelease 返回(原来 +1 不释放 = 每调一次泄漏一个数组)。根因与取舍见该处注释。
    let objects: Vec<id> = env.objc.borrow::<SetHostObject>(this).dict.iter_keys().collect();
    for &object in &objects {
        retain(env, object);
    }
    let array: id = ns_array::from_vec(env, objects);
    autorelease(env, array)
}

- (id)objectEnumerator { // NSEnumerator*
    let array: id = msg![env; this allObjects];
    msg![env; array objectEnumerator]
}

// NSFastEnumeration implementation
- (NSUInteger)countByEnumeratingWithState:(MutPtr<NSFastEnumerationState>)state
                                  objects:(MutPtr<id>)stackbuf
                                    count:(NSUInteger)len {
    // TODO: check that set wasn't mutated!
    // We assume that order in which objects are reported is consistent
    // between calls!
    let objects: id = msg![env; this allObjects];
    let count: NSUInteger = msg![env; objects count];
    fast_enumeration_helper(env, this, |env, idx| {
        if idx < count {
            msg![env; objects objectAtIndex:idx]
        } else {
            nil
        }
    }, state, stackbuf, len)
}

// TODO: more mutation methods

- (())addObject:(id)object {
    let null: id = msg_class![env; NSNull null];
    let mut host_obj: SetHostObject = std::mem::take(env.objc.borrow_mut(this));
    host_obj.dict.insert(env, object, null, /* copy_key: */ false);
    *env.objc.borrow_mut(this) = host_obj;
}

- (())removeObject:(id)object {
    let mut host_obj: SetHostObject = std::mem::take(env.objc.borrow_mut(this));
    host_obj.dict.remove(env, object);
    *env.objc.borrow_mut(this) = host_obj;
}

- (())removeAllObjects {
    let mut old_host_obj = std::mem::replace(
        env.objc.borrow_mut(this),
        SetHostObject {
            dict: Default::default(),
        },
    );
    old_host_obj.dict.release(env);
}

- (())unionSet:(id)other { // NSSet *
    let enumerator: id = msg![env; other objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        () = msg![env; this addObject:next];
    }
}

- (())minusSet:(id)other { // NSSet *
    if other == nil {
        return;
    }
    // 先取快照再删,other == this 时也不会边遍历边改。
    // [审查修 2026-09-13] allObjects 现在返回 autoreleased 数组且元素已 retain,删除期间元素不会被
    // 提前释放;这里不要 release 它。
    let objects: id = msg![env; other allObjects];
    let count: NSUInteger = msg![env; objects count];
    for i in 0..count {
        let object: id = msg![env; objects objectAtIndex:i];
        () = msg![env; this removeObject:object];
    }
}

@end

};

/// Helper shared by `initWithArray:` / `initWithSet:` of `_touchHLE_NSSet` and
/// `_touchHLE_NSMutableSet`: any collection answering `objectEnumerator`.
fn set_from_collection(env: &mut Environment, collection: id) -> DictionaryHostObject {
    let null: id = msg_class![env; NSNull null];
    let mut dict = <DictionaryHostObject as Default>::default();
    if collection == nil {
        return dict;
    }
    let enumerator: id = msg![env; collection objectEnumerator];
    loop {
        let next: id = msg![env; enumerator nextObject];
        if next == nil {
            break;
        }
        dict.insert(env, next, null, /* copy_key: */ false);
    }
    dict
}

/// Helper method shared between `initWithObjects:` of `_touchHLE_NSSet` and
/// `_touchHLE_NSMutableSet`
fn set_from_objects(env: &mut Environment, first_obj: id, args: DotDotDot) -> DictionaryHostObject {
    let null: id = msg_class![env; NSNull null];

    let mut dict = <DictionaryHostObject as Default>::default();
    dict.insert(env, first_obj, null, /* copy_key: */ false);
    let mut varargs = args.start();
    loop {
        let next_arg: id = varargs.next(env);
        if next_arg == nil {
            break;
        }
        dict.insert(env, next_arg, null, /* copy_key: */ false);
    }
    dict
}
