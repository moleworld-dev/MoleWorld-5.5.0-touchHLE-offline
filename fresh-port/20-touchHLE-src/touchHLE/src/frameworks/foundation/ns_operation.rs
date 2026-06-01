/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSOperation`, `NSInvocationOperation` and `NSOperationQueue`.
//!
//! touchHLE executes guest code on a single thread, so we run operations
//! synchronously at the point they are added to a queue (or when `start` is
//! called directly). This preserves ordering and "the work actually runs"
//! semantics; it just isn't truly concurrent. For the offline single-player
//! games we target that is enough. MoleWorld adds NSInvocationOperations to
//! background NSOperationQueues during boot (e.g. resource/network managers).

use crate::objc::{
    autorelease, id, msg, msg_class, msg_send, nil, objc_classes, release, retain, ClassExports,
    HostObject, NSZonePtr, SEL,
};

struct NSOperationHostObject {
    /// Strong reference: the invocation target (nil for a plain NSOperation).
    target: id,
    /// `None` for a plain NSOperation (whose `main` does nothing); `Some` for
    /// an NSInvocationOperation built from target+selector.
    selector: Option<SEL>,
    /// Strong reference: the invocation argument (may be nil).
    object: id,
    finished: bool,
}
impl HostObject for NSOperationHostObject {}

struct NSOperationQueueHostObject {
    max_concurrent: i32,
}
impl HostObject for NSOperationQueueHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSOperation: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host = Box::new(NSOperationHostObject {
        target: nil,
        selector: None,
        object: nil,
        finished: false,
    });
    env.objc.alloc_object(this, host, &mut env.mem)
}

- (id)init {
    this
}

- (())start {
    () = msg![env; this main];
    env.objc.borrow_mut::<NSOperationHostObject>(this).finished = true;
}

- (())main {
    // A plain NSOperation does nothing; subclasses override this. An
    // NSInvocationOperation runs its stored target/selector/object.
    let &NSOperationHostObject { target, selector, object, .. } =
        env.objc.borrow(this);
    if let Some(sel) = selector {
        if target != nil {
            if sel.as_str(&env.mem).ends_with(':') {
                let _: id = msg_send(env, (target, sel, object));
            } else {
                let _: id = msg_send(env, (target, sel));
            }
        }
    }
}

- (bool)isFinished {
    env.objc.borrow::<NSOperationHostObject>(this).finished
}
- (bool)isExecuting {
    false
}
- (bool)isCancelled {
    false
}
- (bool)isReady {
    true
}
- (())cancel {
    // We run synchronously, so there's nothing to cancel.
}
- (())setCompletionBlock:(id)_block {
    // TODO: block support. Ignored for now.
}
- (())setQueuePriority:(i32)_priority {
}
- (())setThreadPriority:(f64)_priority {
}
- (())addDependency:(id)_op {
    // We run synchronously in submission order, dependencies are satisfied.
}

- (())dealloc {
    let &NSOperationHostObject { target, object, .. } = env.objc.borrow(this);
    release(env, target);
    release(env, object);
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

@implementation NSInvocationOperation: NSOperation

- (id)initWithTarget:(id)target
            selector:(SEL)selector
              object:(id)object {
    retain(env, target);
    retain(env, object);
    let host = env.objc.borrow_mut::<NSOperationHostObject>(this);
    host.target = target;
    host.selector = Some(selector);
    host.object = object;
    this
}

- (id)result {
    // We don't capture the invocation return value yet.
    nil
}

@end

@implementation NSOperationQueue: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host = Box::new(NSOperationQueueHostObject { max_concurrent: -1 });
    env.objc.alloc_object(this, host, &mut env.mem)
}

+ (id)mainQueue {
    // Return a fresh queue; since we execute synchronously it behaves the same.
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new init];
    autorelease(env, new)
}

+ (id)currentQueue {
    nil
}

- (id)init {
    this
}

- (())setMaxConcurrentOperationCount:(i32)count {
    env.objc.borrow_mut::<NSOperationQueueHostObject>(this).max_concurrent = count;
}
- (i32)maxConcurrentOperationCount {
    env.objc.borrow::<NSOperationQueueHostObject>(this).max_concurrent
}
- (())setName:(id)_name {
}
- (())setSuspended:(bool)_suspended {
}

- (())addOperation:(id)op {
    // Run it right now on this thread (synchronous emulation of concurrency).
    let pool: id = msg_class![env; NSAutoreleasePool new];
    () = msg![env; op start];
    release(env, pool);
}

- (())addOperations:(id)ops // NSArray*
       waitUntilFinished:(bool)_wait {
    let count: u32 = msg![env; ops count];
    for i in 0..count {
        let op: id = msg![env; ops objectAtIndex:i];
        () = msg![env; this addOperation:op];
    }
}

- (NSZonePtr)operations {
    // We never keep operations around (they run immediately), so return an
    // empty array.
    msg_class![env; NSArray array]
}
- (u32)operationCount {
    0
}

- (())cancelAllOperations {
}
- (())waitUntilAllOperationsAreFinished {
    // Everything already ran synchronously.
}

@end

};
