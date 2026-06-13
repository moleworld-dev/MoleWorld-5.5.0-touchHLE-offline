/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSObject`, the root of most class hierarchies in Objective-C.
//!
//! Resources:
//! - Apple's [Advanced Memory Management Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/MemoryMgmt/Articles/MemoryMgmt.html)
//!   explains how reference counting works. Note that we are interested in what
//!   it calls "manual retain-release", not ARC.
//! - Apple's [Key-Value Coding Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/KeyValueCoding/SearchImplementation.html)
//!   explains the algorithm `setValue:forKey:` should follow.
//!
//! See also: [crate::objc], especially the `objects` module.

use super::ns_string::{from_rust_string, to_rust_string};
use super::ns_value::NSNumberHostObject;
use super::{NSTimeInterval, NSUInteger};
use crate::frameworks::foundation::ns_run_loop::{add_perform_request, cancel_perform_requests};
use crate::frameworks::foundation::ns_thread::detach_new_thread_inner;
use crate::libc::semaphore::{host_destroy_semaphore, sem_wait};
use crate::mem::{GuestUSize, MutPtr, MutVoidPtr, Ptr};
use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, msg_send_no_type_checking, nil, objc_classes,
    retain, Class, ClassExports, NSZonePtr, ObjC, TrivialHostObject, SEL,
};
use crate::Environment;

/// A scalar unwrapped from an `NSNumber` for Key-Value Coding assignment. KVC
/// must unbox `NSNumber`/`NSValue` before storing into a scalar setter argument
/// or ivar (otherwise the boxed object pointer is written where a number is
/// expected — and touchHLE used to just assert-crash on this).
#[derive(Clone, Copy)]
enum KvcScalar {
    Bool(bool),
    I32(i32),
    U32(u32),
    I64(i64),
    U64(u64),
    F32(f32),
    F64(f64),
    I16(i16),
    U16(u16),
    I8(i8),
}

fn kvc_unwrap_number(env: &mut Environment, value: id) -> KvcScalar {
    match env.objc.borrow::<NSNumberHostObject>(value) {
        NSNumberHostObject::Bool(x) => KvcScalar::Bool(*x),
        NSNumberHostObject::UnsignedLongLong(x) => KvcScalar::U64(*x),
        NSNumberHostObject::UnsignedInt(x) => KvcScalar::U32(*x),
        NSNumberHostObject::Int(x) => KvcScalar::I32(*x),
        NSNumberHostObject::LongLong(x) => KvcScalar::I64(*x),
        NSNumberHostObject::Float(x) => KvcScalar::F32(*x),
        NSNumberHostObject::Double(x) => KvcScalar::F64(*x),
        NSNumberHostObject::Short(x) => KvcScalar::I16(*x),
        NSNumberHostObject::UnsignedShort(x) => KvcScalar::U16(*x),
        NSNumberHostObject::Char(x) => KvcScalar::I8(*x),
    }
}

/// Invoke a `set<Key>:` accessor with the unwrapped scalar. Uses the
/// no-type-checking send so a slight encoding mismatch can't re-introduce a
/// crash; the boxed type should already match the accessor's argument.
fn kvc_call_setter_scalar(env: &mut Environment, this: id, sel: SEL, s: KvcScalar) {
    match s {
        KvcScalar::Bool(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::I32(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::U32(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::I64(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::U64(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::F32(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::F64(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x)); }
        KvcScalar::I16(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x as i32)); }
        KvcScalar::U16(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x as i32)); }
        KvcScalar::I8(x) => { let _: () = msg_send_no_type_checking(env, (this, sel, x as i32)); }
    }
}

/// Write the unwrapped scalar directly into an ivar at its native width.
fn kvc_write_scalar_ivar(env: &mut Environment, ivar_ptr: MutPtr<GuestUSize>, s: KvcScalar) {
    match s {
        KvcScalar::Bool(x) => env.mem.write(ivar_ptr.cast(), x as u8),
        KvcScalar::I32(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::U32(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::I64(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::U64(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::F32(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::F64(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::I16(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::U16(x) => env.mem.write(ivar_ptr.cast(), x),
        KvcScalar::I8(x) => env.mem.write(ivar_ptr.cast(), x as u8),
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSObject

+ (id)alloc {
    msg![env; this allocWithZone:(MutVoidPtr::null())]
}
+ (id)allocWithZone:(NSZonePtr)_zone { // struct _NSZone*
    log_dbg!("[{:?} allocWithZone:]", this);
    env.objc.alloc_object(this, Box::new(TrivialHostObject), &mut env.mem)
}

+ (id)new {
    let new_object: id = msg![env; this alloc];
    msg![env; new_object init]
}

+ (Class)class {
    this
}
+ (bool)isSubclassOfClass:(Class)class {
    env.objc.class_is_subclass_of(this, class)
}

// See the instance method section for the normal versions of these.
+ (id)retain {
    this // classes are not refcounted
}
+ (())release {
    // classes are not refcounted
}
+ (())autorelease {
    // classes are not refcounted
}

+ (bool)instancesRespondToSelector:(SEL)selector {
    env.objc.class_has_method(this, selector)
}

// See the instance-method `methodForSelector:` below and method_for_selector.
+ (MutVoidPtr)methodForSelector:(SEL)selector {
    method_for_selector(env, this, selector)
}
+ (MutVoidPtr)instanceMethodForSelector:(SEL)selector {
    method_for_selector(env, this, selector)
}

+ (())cancelPreviousPerformRequestsWithTarget:(id)target selector:(SEL)selector object:(id)arg {
    let run_loop: id = msg_class![env; NSRunLoop currentRunLoop];
    cancel_perform_requests(env, run_loop, target, selector, arg);
}

+ (bool)accessInstanceVariablesDirectly {
    true
}

+ (id)description {
    let name = env.objc.get_class_name(this);
    let str = from_rust_string(env, name.to_string());
    autorelease(env, str)
}

+ (id)debugDescription {
    msg![env; this description]
}

+ (id)instanceMethodSignatureForSelector:(SEL)sel {
    // TODO: support `host` method signatures
    let sig = *env.objc.class_get_method_signature(this, sel).unwrap();
    log_dbg!("instanceMethodSignatureForSelector: '{}' -> {:?}", sel.as_str(&env.mem), env.mem.cstr_at_utf8(sig));
    msg_class![env; NSMethodSignature signatureWithObjCTypes:sig]
}


+ (())initialize {
    // Do nothing
}

- (id)init {
    this
}

// Instance variant. MoleWorld's village/dialog UI layers (VillageMenuLayer,
// EditMenuLayer, ReceiveGiftLayer, OnTouchPopZhongXinLayer, ...) call this as
// part of message forwarding / NSInvocation. Return the real signature when the
// selector resolves on the object's class; otherwise return nil (Cocoa's
// documented behaviour for an unknown selector) rather than aborting.
- (id)methodSignatureForSelector:(SEL)sel {
    let class = ObjC::read_isa(this, &env.mem);
    match env.objc.class_get_method_signature(class, sel) {
        Some(&sig) => {
            log_dbg!("methodSignatureForSelector: '{}' -> {:?}", sel.as_str(&env.mem), env.mem.cstr_at_utf8(sig));
            msg_class![env; NSMethodSignature signatureWithObjCTypes:sig]
        }
        None => {
            log_dbg!("methodSignatureForSelector: '{}' -> (unknown) nil", sel.as_str(&env.mem));
            nil
        }
    }
}

- (NSUInteger)retainCount {
    env.objc.get_refcount(this).into()
}

- (id)retain {
    log_dbg!("[{:?} retain]", this);
    env.objc.increment_refcount(this);
    this
}
- (())release {
    log_dbg!("[{:?} release]", this);
    if env.objc.decrement_refcount(this) {
        () = msg![env; this dealloc];
    }
}
- (id)autorelease {
    () = msg_class![env; NSAutoreleasePool addObject:this];
    this
}

- (())dealloc {
    log_dbg!("[{:?} dealloc]", this);
    env.objc.dealloc_object(this, &mut env.mem)
}

- (Class)class {
    ObjC::read_isa(this, &env.mem)
}
- (bool)isMemberOfClass:(Class)class {
    let this_class: Class = msg![env; this class];
    class == this_class
}
- (bool)isKindOfClass:(Class)class {
    let this_class: Class = msg![env; this class];
    env.objc.class_is_subclass_of(this_class, class)
}

- (NSUInteger)hash {
    this.to_bits()
}

// To not confuse with isEqualTo:, which is
// a category of NSWhoseSpecifier!
// Reference https://nshipster.com/equality
- (bool)isEqual:(id)other {
    this == other
}

// TODO: Instance description and debugDescription.
// This is not hard to add, but before adding a fallback implementation of it,
// we should make sure all the Foundation classes' overrides of it are there,
// to prevent weird behavior.
// TODO: localized description methods also? (not sure if NSObject has them)

// Helper for NSCopying
- (id)copy {
    msg![env; this copyWithZone:(MutVoidPtr::null())]
}

// Helper for NSMutableCopying
- (id)mutableCopy {
    msg![env; this mutableCopyWithZone:(MutVoidPtr::null())]
}

// NSKeyValueCoding
// https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/KeyValueCoding/SearchImplementation.html
- (())setValue:(id)value
       forKey:(id)key { // NSString*
    let key_string = to_rust_string(env, key); // TODO: avoid copy?
    assert!(key_string.is_ascii()); // TODO: do we have to handle non-ASCII keys?
    let camel_case_key_string = format!("{}{}", key_string.as_bytes()[0].to_ascii_uppercase() as char, &key_string[1..]);

    let class = msg![env; this class];

    // TODO: If value is nil, the target ivar/method argument type must be
    // checked. If it's non-object type, invoke setNilValueForKey:
    assert!(value != nil);

    // If the value is a boxed scalar (NSNumber), unwrap it so it can be assigned
    // to a scalar setter argument or ivar. Without this, KVC with e.g. @(int)
    // (which a few mini-games use) used to hit an assert and crash the emulator.
    // A non-NSNumber NSValue (e.g. a boxed CGPoint) is rare here; we log it and
    // fall back to assigning the boxed object.
    let value_class = msg![env; value class];
    let ns_number_class = env.objc.get_known_class("NSNumber", &mut env.mem);
    let scalar: Option<KvcScalar> = if env.objc.class_is_subclass_of(value_class, ns_number_class) {
        Some(kvc_unwrap_number(env, value))
    } else {
        let ns_value_class = env.objc.get_known_class("NSValue", &mut env.mem);
        if env.objc.class_is_subclass_of(value_class, ns_value_class) {
            log!(
                "Warning: setValue:forKey:\"{}\" got a non-NSNumber NSValue; \
                 assigning the boxed object as-is.",
                key_string
            );
        }
        None
    };

    // Look for the first accessor named set<Key>: or _set<Key>:, in that order.
    // If found, invoke it with the (unwrapped, if boxed) value and finish.
    for prefix in ["set", "_set"] {
        if let Some(sel) =
            env.objc.lookup_selector(&format!("{prefix}{camel_case_key_string}:"))
        {
            if env.objc.class_has_method(class, sel) {
                match scalar {
                    Some(s) => {
                        kvc_call_setter_scalar(env, this, sel, s);
                    }
                    None => {
                        () = msg_send(env, (this, sel, value));
                    }
                }
                return;
            }
        }
    }

    // If no simple accessor is found, and if the class method
    // accessInstanceVariablesDirectly returns YES, look for an instance
    // variable with a name like _<key>, _is<Key>, <key>, or is<Key>,
    // in that order. If found, set the variable directly.
    let sel = env.objc.lookup_selector("accessInstanceVariablesDirectly").unwrap();
    let accessInstanceVariablesDirectly = msg_send(env, (class, sel));
    if accessInstanceVariablesDirectly {
        if let Some(ivar_ptr) = env.objc.object_lookup_ivar(&env.mem, this, &format!("_{key_string}"))
            .or_else(|| env.objc.object_lookup_ivar(&env.mem, this, &format!("_is{camel_case_key_string}")))
            .or_else(|| env.objc.object_lookup_ivar(&env.mem, this, &format!("{key_string}")))
            .or_else(|| env.objc.object_lookup_ivar(&env.mem, this, &format!("is{camel_case_key_string}"))
        ) {
            match scalar {
                Some(s) => kvc_write_scalar_ivar(env, ivar_ptr, s),
                None => {
                    retain(env, value);
                    env.mem.write(ivar_ptr.cast(), value);
                }
            }
            return;
        }
    }

    // Upon finding no accessor or instance variable,
    // invoke setValue:forUndefinedKey:.
    // This raises an exception by default, but a subclass of NSObject
    // may provide key-specific behavior.
    let sel = env.objc.lookup_selector("setValue:forUndefinedKey:").unwrap();
    () = msg_send(env, (this, sel, value, key));
}

- (())setValue:(id)_value
forUndefinedKey:(id)key { // NSString*
    // TODO: Raise NSUnknownKeyException
    let class: Class = ObjC::read_isa(this, &env.mem);
    let class_name_string = env.objc.get_class_name(class).to_owned(); // TODO: Avoid copying
    let key_string = to_rust_string(env, key);
    panic!("Object {:?} of class {:?} ({:?}) does not have a setter for {} ({:?})\
        \nAvailable selectors: {}\nAvailable ivars: {}",
        this, class_name_string, class, key_string, key,
        env.objc.debug_all_class_selectors_as_strings(&env.mem, class).join(", "),
        env.objc.debug_all_class_ivars_as_strings(class).join(", "));
}

- (bool)respondsToSelector:(SEL)selector {
    env.objc.object_has_method(&env.mem, this, selector)
}

// Returns the IMP for a selector. touchHLE doesn't expose per-method IMP
// addresses, so we return objc_msgSend's guest trampoline as a universal IMP
// (see method_for_selector). Used by e.g. JSONKit to cache IMPs.
- (MutVoidPtr)methodForSelector:(SEL)selector {
    method_for_selector(env, this, selector)
}
- (MutVoidPtr)instanceMethodForSelector:(SEL)selector {
    method_for_selector(env, this, selector)
}

- (id)performSelector:(SEL)sel {
    assert!(!sel.is_null());
    msg_send_no_type_checking(env, (this, sel))
}

- (id)performSelector:(SEL)sel
           withObject:(id)o1 {
    assert!(!sel.is_null());
    msg_send_no_type_checking(env, (this, sel, o1))
}

- (id)performSelector:(SEL)sel
           withObject:(id)o1
           withObject:(id)o2 {
    assert!(!sel.is_null());
    msg_send_no_type_checking(env, (this, sel, o1, o2))
}

- (())performSelectorInBackground:(SEL)sel
                       withObject:(id)arg {
    detach_new_thread_inner(env, sel, this, arg, /* tolerate_type_mismatch: */ true)
}

- (())performSelector:(SEL)sel withObject:(id)arg afterDelay:(NSTimeInterval)delay {
    let run_loop: id = msg_class![env; NSRunLoop currentRunLoop];
    add_perform_request(env, run_loop, this, sel, arg, Some(delay), false);
}

- (())performSelector:(SEL)sel withObject:(id)arg afterDelay:(NSTimeInterval)delay inModes:(id)_modes {
    // We don't model run-loop modes, so schedule exactly like the plain afterDelay:
    // variant. CocoaAsyncSocket's AsyncSocket uses this to schedule its connect/read
    // work on the run loop; without it the call was a silent no-op and the socket never
    // progressed (no getaddrinfo, no connection).
    let run_loop: id = msg_class![env; NSRunLoop currentRunLoop];
    add_perform_request(env, run_loop, this, sel, arg, Some(delay), false);
}

- (())performSelectorOnMainThread:(SEL)sel withObject:(id)arg waitUntilDone:(bool)wait {
    log_dbg!("performSelectorOnMainThread:{} withObject:{:?} waitUntilDone:{}", sel.as_str(&env.mem), arg, wait);
    if wait && env.current_thread == 0 {
        if sel.as_str(&env.mem).ends_with(':') {
            () = msg_send(env, (this, sel, arg));
        } else {
            assert!(arg.is_null());
            () = msg_send(env, (this, sel));
        }
        return;
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.POP") && (sel == env.objc.lookup_selector("startMovie:").unwrap() || sel == env.objc.lookup_selector("stopMovie").unwrap()) && wait {
        log!("Applying game-specific hack for PoP: WW: ignoring performSelectorOnMainThread:SEL({}) waitUntilDone:true", sel.as_str(&env.mem));
        return;
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.Asphalt5") && (sel == env.objc.lookup_selector("startMovie:").unwrap() || sel == env.objc.lookup_selector("stopMovie:").unwrap()) && wait {
        log!("Applying game-specific hack for Asphalt5: ignoring performSelectorOnMainThread:SEL({}) waitUntilDone:true", sel.as_str(&env.mem));
        return;
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.SplinterCell") && sel == env.objc.lookup_selector("startMovie:").unwrap() && wait {
        log!("Applying game-specific hack for SplinterCell: ignoring performSelectorOnMainThread:SEL({}) waitUntilDone:true", sel.as_str(&env.mem));
        return;
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.AssassinsCreed") && sel == env.objc.lookup_selector("moviePlayerInit:").unwrap() && wait {
        log!("Applying game-specific hack for AssassinsCreed: ignoring performSelectorOnMainThread:SEL(moviePlayerInit:) waitUntilDone:true");
        return;
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.Ferrari") && wait {
        if sel == env.objc.lookup_selector("startMovie:").unwrap() {
            log!("Applying game-specific hack for Ferrari GT: ignoring performSelectorOnMainThread:SEL({}) waitUntilDone:true", sel.as_str(&env.mem));
            return;
        }
        if sel == env.objc.lookup_selector("initTextInput:").unwrap() || sel == env.objc.lookup_selector("removeTextField:").unwrap() {
            log!("Applying game-specific hack for Ferrari GT: performing performSelectorOnMainThread:SEL({}) waitUntilDone:true on thread {}", sel.as_str(&env.mem), env.current_thread);
            () = msg_send(env, (this, sel, arg));
            return;
        }
    }
    if env.bundle.bundle_identifier().starts_with("com.gameloft.HOS2") && wait {
        if sel == env.objc.lookup_selector("loadMovie:").unwrap() || sel == env.objc.lookup_selector("sendGameInfo").unwrap() || sel == env.objc.lookup_selector("setStatusBar:").unwrap() {
            log!("Applying game-specific hack for HOS2: performing performSelectorOnMainThread:SEL({}) waitUntilDone:true on thread {}", sel.as_str(&env.mem), env.current_thread);
            if sel.as_str(&env.mem).ends_with(':') {
                () = msg_send(env, (this, sel, arg));
            } else {
                assert!(arg.is_null());
                () = msg_send(env, (this, sel));
            }
            return;
        }
        if sel == env.objc.lookup_selector("startMovie:").unwrap() || sel == env.objc.lookup_selector("stopMovie:").unwrap() {
            log!("Applying game-specific hack for HOS2: ignoring performSelectorOnMainThread:SEL({}) waitUntilDone:true", sel.as_str(&env.mem));
            return;
        }
    }

    let run_loop: id = msg_class![env; NSRunLoop mainRunLoop];
    let sem = add_perform_request(env, run_loop, this, sel, arg, None, wait);
    if wait {
        sem_wait(env, sem);
        host_destroy_semaphore(env, sem);
    }
}

// UINibLoadingAdditions protocol
- (())awakeFromNib {
    // no-op
}

@end

};

/// Implementation of `methodForSelector:` / `instanceMethodForSelector:`.
///
/// touchHLE has no real per-method IMP function pointers (host methods are Rust
/// fns, guest methods are Thumb/ARM code reached via the method table). Instead
/// of synthesizing a per-(class,selector) trampoline, we return the guest
/// trampoline for `objc_msgSend`. Calling the returned IMP as
/// `imp(receiver, selector, args...)` is then exactly `objc_msgSend(receiver,
/// selector, args...)`, which performs the normal method lookup and dispatch.
///
/// This is functionally correct (only adds one extra method lookup per call) and
/// is what libraries like JSONKit need: they call `methodForSelector:` once in
/// `+initialize` to cache an IMP, then invoke it many times for fast boxing of
/// NSNumber/NSString/NSArray/NSDictionary values.
fn method_for_selector(env: &mut Environment, _this: id, _selector: SEL) -> MutVoidPtr {
    let gf = env
        .dyld
        .create_proc_address(&mut env.mem, &mut env.cpu, "_objc_msgSend")
        .unwrap();
    let ret = Ptr::from_bits(gf.addr_with_thumb_bit());
    ret
}
