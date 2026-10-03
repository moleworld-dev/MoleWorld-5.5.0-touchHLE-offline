/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFHTTPMessage`

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use crate::frameworks::core_foundation::cf_data::CFDataRef;
use crate::frameworks::core_foundation::cf_string::CFStringRef;
use crate::frameworks::core_foundation::cf_url::CFURLRef;
use crate::frameworks::core_foundation::CFTypeRef;
use crate::frameworks::foundation::ns_string;
use crate::mem::Ptr;
use crate::objc::msg;
use crate::Environment;

// Note: on iOS SDK side this type is defined as a pointer to an opaque struct
type CFHTTPMessageRef = CFTypeRef;
type CFReadStreamRef = CFTypeRef;

fn CFHTTPMessageCreateRequest(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    request_method: CFStringRef,
    url: CFURLRef,
    http_version: CFStringRef,
) -> CFHTTPMessageRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    let url_desc = msg![env; url description];
    log!(
        "TODO: CFHTTPMessageCreateRequest({}, '{}', {}) -> NULL",
        ns_string::to_rust_string(env, request_method),
        ns_string::to_rust_string(env, url_desc),
        ns_string::to_rust_string(env, http_version),
    );
    Ptr::null()
}

fn CFHTTPMessageSetHeaderFieldValue(
    env: &mut Environment,
    message: CFHTTPMessageRef,
    header_field: CFStringRef,
    value: CFStringRef,
) {
    if !message.is_null() {
        todo!(
            "CFHTTPMessageSetHeaderFieldValue({:?}, {}, {})",
            message,
            ns_string::to_rust_string(env, header_field),
            ns_string::to_rust_string(env, value)
        );
    }
}

fn CFHTTPMessageSetBody(_env: &mut Environment, message: CFHTTPMessageRef, body_data: CFDataRef) {
    if !message.is_null() {
        todo!("CFHTTPMessageSetBody({:?}, {:?})", message, body_data);
    }
}

fn CFReadStreamCreateForHTTPRequest(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    message: CFHTTPMessageRef,
) -> CFReadStreamRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    if !message.is_null() {
        todo!(
            "CFReadStreamCreateForHTTPRequest({:?}, {:?})",
            allocator,
            message,
        );
    }
    Ptr::null()
}

// [同步上游 0.3.0 2026-10-02] 上游 4b69415d 在这里加了 CFReadStreamSetProperty /
// CFReadStreamSetClient 两个桩(流非空就 todo! 崩溃,并注明「TODO: move to correct location」)。
// 我方在 core_foundation/cf_stream.rs 早已有真实现(摩尔庄园联网主协议走 AsyncSocket:
// CFStreamCreatePairWithSocketToHost + CFRead/WriteStreamSetClient + ...Open,背后是宿主 TCP)。
// 两处同名导出时,链接器按 DYLIB_LIST 顺序先命中 cf_network(排在 core_foundation 前面),
// 联网一建连就会落到这里的 todo! 崩溃,所以删掉这两个桩,只保留 cf_stream.rs 的实现。

const kCFHTTPVersion1_0: &str = "kCFHTTPVersion1_0";
const kCFHTTPVersion1_1: &str = "kCFHTTPVersion1_1";

// TODO: move to correct location
const kCFStreamPropertySSLSettings: &str = "kCFStreamPropertySSLSettings";
const kCFStreamPropertyHTTPResponseHeader: &str = "kCFStreamPropertyHTTPResponseHeader";

pub const CONSTANTS: ConstantExports = &[
    (
        "_kCFHTTPVersion1_0",
        HostConstant::NSString(kCFHTTPVersion1_0),
    ),
    (
        "_kCFHTTPVersion1_1",
        HostConstant::NSString(kCFHTTPVersion1_1),
    ),
    (
        "_kCFStreamPropertySSLSettings",
        HostConstant::NSString(kCFStreamPropertySSLSettings),
    ),
    (
        "_kCFStreamPropertyHTTPResponseHeader",
        HostConstant::NSString(kCFStreamPropertyHTTPResponseHeader),
    ),
    // TODO: move to correct location
    (
        "_kCFStreamSocketSecurityLevelNegotiatedSSL",
        HostConstant::NSString("kCFStreamSocketSecurityLevelNegotiatedSSL"),
    ),
    (
        "_kCFStreamSSLValidatesCertificateChain",
        HostConstant::NSString("kCFStreamSSLValidatesCertificateChain"),
    ),
    (
        "_kCFStreamSSLPeerName",
        HostConstant::NSString("kCFStreamSSLPeerName"),
    ),
    (
        "_kCFStreamSSLLevel",
        HostConstant::NSString("kCFStreamSSLLevel"),
    ),
    (
        "_kCFStreamSSLAllowsExpiredRoots",
        HostConstant::NSString("kCFStreamSSLAllowsExpiredRoots"),
    ),
    (
        "_kCFStreamSSLAllowsExpiredCertificates",
        HostConstant::NSString("_kCFStreamSSLAllowsExpiredCertificates"),
    ),
    (
        "_kCFStreamSSLAllowsAnyRoot",
        HostConstant::NSString("kCFStreamSSLAllowsAnyRoot"),
    ),
];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFHTTPMessageCreateRequest(_, _, _, _)),
    export_c_func!(CFHTTPMessageSetHeaderFieldValue(_, _, _)),
    export_c_func!(CFHTTPMessageSetBody(_, _)),
    export_c_func!(CFReadStreamCreateForHTTPRequest(_, _)),
    // [同步上游 0.3.0 2026-10-02] CFReadStreamSetProperty / CFReadStreamSetClient 由
    // core_foundation/cf_stream.rs 导出(真实现),这里不再重复导出,见上方说明。
];
