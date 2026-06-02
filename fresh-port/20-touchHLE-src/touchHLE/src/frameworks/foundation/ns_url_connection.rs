/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSURLConnection`.
//!
//! touchHLE has no real networking (and we run MoleWorld fully offline: its
//! servers are gone). Rather than hang waiting for a response that never comes,
//! an NSURLConnection here immediately reports failure to its delegate with
//! NSURLErrorNotConnectedToInternet. The game then takes its "couldn't connect"
//! path instead of spinning forever.

use super::{ns_string, NSInteger};
use crate::environment::Environment;
use crate::mem::MutPtr;
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
};
use std::borrow::Cow;

const NSURLErrorDomain: &str = "NSURLErrorDomain";

/// Our helper type, Foundation just uses ints.
type NSURLErrorCode = NSInteger;
const NSURLErrorNotConnectedToInternet: NSURLErrorCode = -1009;

#[derive(Default)]
struct NSURLConnectionHostObject {
    /// Strong reference to the delegate (may be nil).
    delegate: id,
}
impl HostObject for NSURLConnectionHostObject {}

/// Deliver a "not connected to the internet" failure to a connection's
/// delegate, if it implements `connection:didFailWithError:`.
fn fail_offline(env: &mut Environment, connection: id, delegate: id) {
    if delegate == nil {
        return;
    }
    if !env
        .objc
        .object_has_method_named(&env.mem, delegate, "connection:didFailWithError:")
    {
        return;
    }
    let domain = ns_string::get_static_str(env, NSURLErrorDomain);
    let error: id = msg_class![env; NSError alloc];
    let error: id = msg![env; error initWithDomain:domain
                                              code:NSURLErrorNotConnectedToInternet
                                          userInfo:nil];
    autorelease(env, error);
    () = msg![env; delegate connection:connection didFailWithError:error];
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSURLConnection: NSObject

+ (id)allocWithZone:(crate::objc::NSZonePtr)_zone {
    let host_object = Box::<NSURLConnectionHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)sendSynchronousRequest:(id)request // NSURLRequest *
           returningResponse:(MutPtr<id>)response // NSURLResponse **
                       error:(MutPtr<id>)out_error { // NSError **
    // [crash log] 离线下分析 SDK(如 TalkingData)会每帧重试同步上报,这条会刷爆日志
    // (实测一份日志里 400+ 条同样的行),把崩溃前真正的操作淹没、文件也可能撑大。
    // 只完整记录首条,之后同类离线同步请求不再每条刷屏。行为不变(始终返回 nil)。
    {
        use std::sync::atomic::{AtomicBool, Ordering};
        static LOGGED_OFFLINE_ONCE: AtomicBool = AtomicBool::new(false);
        if !LOGGED_OFFLINE_ONCE.swap(true, Ordering::Relaxed) {
            log!(
                "[NSURLConnection sendSynchronousRequest:{:?} ('{}')] -> nil (offline) [后续同类离线同步请求不再每条记录]",
                request,
                url_string_from_request(env, request),
            );
        }
    }
    if !response.is_null() {
        env.mem.write(response, nil);
    }
    if !out_error.is_null() {
        let domain = ns_string::get_static_str(env, NSURLErrorDomain);
        let error = msg_class![env; NSError alloc];
        let error = msg![env; error initWithDomain:domain code:NSURLErrorNotConnectedToInternet userInfo:nil];
        autorelease(env, error);
        env.mem.write(out_error, error);
    }
    nil
}

+ (id)connectionWithRequest:(id)request // NSURLRequest *
                   delegate:(id)delegate {
    let new: id = msg![env; this alloc];
    let new: id = msg![env; new initWithRequest:request delegate:delegate];
    autorelease(env, new)
}

- (id)init {
    this
}

- (id)initWithRequest:(id)request // NSURLRequest *
             delegate:(id)delegate {
    msg![env; this initWithRequest:request delegate:delegate startImmediately:true]
}

- (id)initWithRequest:(id)request // NSURLRequest *
             delegate:(id)delegate
     startImmediately:(bool)start_immediately {
    log!(
        "[(NSURLConnection *){:?} initWithRequest:('{}') delegate:{:?} startImmediately:{}] (offline)",
        this,
        url_string_from_request(env, request),
        delegate,
        start_immediately,
    );
    retain(env, delegate);
    env.objc.borrow_mut::<NSURLConnectionHostObject>(this).delegate = delegate;
    if start_immediately {
        () = msg![env; this start];
    }
    this
}

- (())setDelegateQueue:(id)_queue {
}
- (())scheduleInRunLoop:(id)_run_loop forMode:(id)_mode {
}
- (())unscheduleFromRunLoop:(id)_run_loop forMode:(id)_mode {
}

- (())start {
    // No real networking: report failure to the delegate so the game proceeds
    // down its offline / connection-error path instead of waiting forever.
    let delegate = env.objc.borrow::<NSURLConnectionHostObject>(this).delegate;
    fail_offline(env, this, delegate);
}

- (())cancel {
    let delegate = env.objc.borrow_mut::<NSURLConnectionHostObject>(this).delegate;
    if delegate != nil {
        release(env, delegate);
        env.objc.borrow_mut::<NSURLConnectionHostObject>(this).delegate = nil;
    }
}

- (())dealloc {
    let delegate = env.objc.borrow::<NSURLConnectionHostObject>(this).delegate;
    if delegate != nil {
        release(env, delegate);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

};

fn url_string_from_request(env: &mut Environment, request: id) -> Cow<'static, str> {
    if request == nil {
        Cow::from("(null)")
    } else {
        let url = msg![env; request URL];
        let ns_string = msg![env; url absoluteString];
        ns_string::to_rust_string(env, ns_string)
    }
}
