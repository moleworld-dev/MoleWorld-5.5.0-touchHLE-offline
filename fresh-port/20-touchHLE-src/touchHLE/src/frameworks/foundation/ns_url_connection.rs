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
use crate::mem::{GuestUSize, MutPtr};
use crate::objc::{
    autorelease, id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject,
    NSZonePtr,
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
    /// Strong reference to the originating NSURLRequest (for the serverlist hook).
    request: id,
}
impl HostObject for NSURLConnectionHostObject {}

/// Minimal NSHTTPURLResponse so the serverlist fetch's `!resp || [resp statusCode]==404`
/// guard passes in online mode (the game checks the response, then reads the body
/// separately via NSData initWithContentsOfURL:).
struct NSHTTPURLResponseHostObject {
    status_code: NSInteger,
}
impl HostObject for NSHTTPURLResponseHostObject {}

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

/// MoleWorld online mode (`--allow-network-access`): the game fetches its server
/// list from the dead Taomee URL `http://imolelogin.61.com:8080/dynamic/online.imole`.
/// When online mode is on and a request targets that URL, hand the game OUR private
/// server instead. Body mirrors the real serverlist: "<areaId>\n<host>:<port>\n".
/// Override host:port via the MOLE_SERVER env var (default login.moleworld.net:7821).
fn serverlist_override(env: &mut Environment, request: id) -> Option<Vec<u8>> {
    if !env.options.network_access || request == nil {
        return None;
    }
    let url = url_string_from_request(env, request);
    if !(url.contains("online.imole") || url.contains("imolelogin")) {
        return None;
    }
    let server =
        std::env::var("MOLE_SERVER").unwrap_or_else(|_| "login.moleworld.net:7821".to_string());
    log!(
        "[serverlist hook] {} -> private server '{}' (online mode)",
        url,
        server
    );
    Some(format!("1\n{}\n", server).into_bytes())
}

/// Build an autoreleased NSData copying `bytes` into guest memory.
fn nsdata_from_bytes(env: &mut Environment, bytes: &[u8]) -> id {
    let len = bytes.len() as GuestUSize;
    let buf = env.mem.alloc(len);
    env.mem.bytes_at_mut(buf.cast(), len).copy_from_slice(bytes);
    msg_class![env; NSData dataWithBytesNoCopy:buf length:len]
}

/// Deliver serverlist bytes to an async connection's delegate, mimicking a
/// successful load: (optional) didReceiveResponse, then didReceiveData, then
/// connectionDidFinishLoading.
fn deliver_serverlist(env: &mut Environment, connection: id, delegate: id, body: &[u8]) {
    if delegate == nil {
        return;
    }
    let data = nsdata_from_bytes(env, body);
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "connection:didReceiveResponse:")
    {
        () = msg![env; delegate connection:connection didReceiveResponse:nil];
    }
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "connection:didReceiveData:")
    {
        () = msg![env; delegate connection:connection didReceiveData:data];
    }
    if env
        .objc
        .object_has_method_named(&env.mem, delegate, "connectionDidFinishLoading:")
    {
        () = msg![env; delegate connectionDidFinishLoading:connection];
    }
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
    // MoleWorld online mode: intercept the dead Taomee serverlist URL and hand
    // the game our private server synchronously.
    if let Some(body) = serverlist_override(env, request) {
        let data = nsdata_from_bytes(env, &body);
        if !response.is_null() {
            // The caller (HttpManager) guards on `!resp || [resp statusCode]==404`,
            // so hand back a non-nil 200 response. (It reads the body separately via
            // NSData initWithContentsOfURL:, which we also intercept.)
            let resp: id = msg_class![env; NSHTTPURLResponse alloc];
            let resp: id = msg![env; resp init];
            autorelease(env, resp);
            env.mem.write(response, resp);
        }
        if !out_error.is_null() {
            env.mem.write(out_error, nil);
        }
        log!("[NSURLConnection sendSynchronousRequest] serverlist hook -> {} bytes + 200 resp", body.len());
        return data;
    }
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
    retain(env, request);
    env.objc.borrow_mut::<NSURLConnectionHostObject>(this).request = request;
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
    let delegate = env.objc.borrow::<NSURLConnectionHostObject>(this).delegate;
    let request = env.objc.borrow::<NSURLConnectionHostObject>(this).request;
    // MoleWorld online mode: serve our private server for the serverlist URL.
    if let Some(body) = serverlist_override(env, request) {
        deliver_serverlist(env, this, delegate, &body);
        return;
    }
    // No real networking: report failure to the delegate so the game proceeds
    // down its offline / connection-error path instead of waiting forever.
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
    let request = env.objc.borrow::<NSURLConnectionHostObject>(this).request;
    if request != nil {
        release(env, request);
    }
    env.objc.dealloc_object(this, &mut env.mem)
}

@end

// Minimal NSHTTPURLResponse: enough for the serverlist fetch's statusCode check.
@implementation NSHTTPURLResponse: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSHTTPURLResponseHostObject { status_code: 200 });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)init {
    this
}

- (NSInteger)statusCode {
    env.objc.borrow::<NSHTTPURLResponseHostObject>(this).status_code
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
