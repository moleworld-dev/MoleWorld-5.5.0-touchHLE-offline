/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFData` and `CFMutableData`.
//!
//! These are toll-free bridged to `NSData` and `NSMutableData` in Apple's
//! implementation. Here they are the same types.

use super::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use super::{CFIndex, CFRange};
use crate::dyld::FunctionExports;
use crate::export_c_func;
use crate::frameworks::foundation::{NSRange, NSUInteger};
use crate::mem::{ConstPtr, ConstVoidPtr, MutPtr, MutVoidPtr};
use crate::objc::{id, msg, msg_class};
use crate::Environment;

pub type CFDataRef = super::CFTypeRef;

pub fn CFDataCreate(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    bytes: ConstPtr<u8>,
    length: CFIndex,
) -> CFDataRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    let bytes: ConstVoidPtr = bytes.cast();
    let length: NSUInteger = length.try_into().unwrap();
    let new: id = msg_class![env; NSData alloc];
    msg![env; new initWithBytes:bytes length:length]
}

fn CFDataCreateWithBytesNoCopy(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    bytes: ConstPtr<u8>,
    length: CFIndex,
    deallocator: CFAllocatorRef,
) -> CFDataRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    // [同步上游 0.3.0 2026-10-02] 上游 25ba5501 只支持 deallocator = kCFAllocatorNull(不释放缓冲),
    // 写法 env.mem.read(deallocator) 在传 NULL 时直接读空指针崩溃。按 Apple 文档,NULL /
    // kCFAllocatorDefault / kCFAllocatorSystemDefault 表示「CFData 释放时用默认分配器释放这块缓冲」,
    // 即 freeWhenDone:YES。游戏里 11 份 JSONKit(如 -[JKSerializer serializeObject:…]@0x1faf9a)
    // 都是 CFDataCreateWithBytesNoCopy(NULL, 自己 malloc 的输出缓冲, 长度, NULL) 把缓冲交给 CFData;
    // 分叉版此前本函数未实现(链接成返回 0 的空操作),上游实现后这条路径会必崩,所以补上默认分配器分支。
    let free_when_done = if deallocator == kCFAllocatorDefault
        || env.mem.read(deallocator).is_system_default()
    {
        true
    } else {
        assert!(env.mem.read(deallocator).is_null()); // unimplemented
        false
    };
    let bytes: MutVoidPtr = bytes.cast().cast_mut();
    let length: NSUInteger = length.try_into().unwrap();
    let new: id = msg_class![env; NSData alloc];
    msg![env; new initWithBytesNoCopy:bytes length:length freeWhenDone:free_when_done]
}

pub fn CFDataGetLength(env: &mut Environment, data: CFDataRef) -> CFIndex {
    let len: NSUInteger = msg![env; data length];
    len.try_into().unwrap()
}

pub fn CFDataGetBytePtr(env: &mut Environment, data: CFDataRef) -> ConstPtr<u8> {
    let ptr: ConstVoidPtr = msg![env; data bytes];
    ptr.cast()
}

fn CFDataGetBytes(env: &mut Environment, data: CFDataRef, range: CFRange, buffer: MutPtr<u8>) {
    let range = NSRange {
        location: range.location.try_into().unwrap(),
        length: range.length.try_into().unwrap(),
    };
    msg![env; data getBytes:buffer range:range]
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFDataCreate(_, _, _)),
    export_c_func!(CFDataCreateWithBytesNoCopy(_, _, _, _)),
    export_c_func!(CFDataGetLength(_)),
    export_c_func!(CFDataGetBytePtr(_)),
    export_c_func!(CFDataGetBytes(_, _, _)),
];
