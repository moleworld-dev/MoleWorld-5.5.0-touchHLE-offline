/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Grand Central Dispatch (libdispatch): minimal subset.
//!
//! Currently just `dispatch_once`, which guest code uses pervasively for
//! one-time initialisation (singletons). MoleWorld's immob SDK
//! `+[IMCommonUtil reloadWebViewUserAgent]` is the first caller that needs it.

use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{ConstPtr, GuestUSize, MutPtr};
use crate::frameworks::foundation::ns_string::{from_rust_string, to_rust_string};
use crate::objc::{id, nil};
use crate::Environment;

/// `void dispatch_once(dispatch_once_t *predicate, dispatch_block_t block)`
///
/// `dispatch_once_t` is a `long` (4 bytes on the 32-bit guest), 0 until the
/// block has run. We run the block the first time, then set the predicate to
/// `~0` — the same DISPATCH_ONCE_DONE sentinel libdispatch uses — so later
/// calls (including any inlined fast-path that tests `*predicate == ~0`) skip it.
///
/// There is no dedicated block-invocation helper in touchHLE, so we invoke the
/// block directly via its ABI: a block literal on the 32-bit guest is
/// `{ void *isa; int flags; int reserved; void (*invoke)(void *, ...); ... }`,
/// i.e. the `invoke` function pointer is at byte offset 12 and is called with
/// the block itself as its first argument.
fn dispatch_once(env: &mut Environment, predicate: MutPtr<i32>, block: id) {
    if env.mem.read(predicate) != 0 {
        return;
    }
    if !block.is_null() {
        let invoke_addr: GuestUSize =
            env.mem.read(ConstPtr::<GuestUSize>::from_bits(block.to_bits() + 12));
        let invoke_fn = GuestFunction::from_addr_with_thumb_bit(invoke_addr);
        () = invoke_fn.call_from_host(env, (block,));
    }
    env.mem.write(predicate, -1i32);
}

/// `void _dyld_register_func_for_add_image(void (*func)(const struct mach_header *, intptr_t))`
///
/// Registers a callback that dyld would invoke for every loaded Mach-O image
/// (both already-loaded and future). touchHLE does not deliver image-add
/// notifications, so this is a no-op. Guest code that uses it — crash reporters
/// and analytics image tracking, reached here via MoleWorld's immob/Flurry SDKs
/// — runs fine without the callback in our offline port.
///
/// (Lives here for convenience alongside the other small runtime stubs; not
/// strictly a libdispatch function.)
#[allow(non_snake_case)]
fn _dyld_register_func_for_add_image(_env: &mut Environment, _func: GuestFunction) {}

/// `CFStringRef CFURLCreateStringByAddingPercentEscapes(CFAllocatorRef,
///  CFStringRef originalString, CFStringRef charsToLeaveUnescaped,
///  CFStringRef legalCharsToBeEscaped, CFStringEncoding)`
///
/// Percent-encodes a string for use in a URL. We ignore the allocator, the
/// leave/escape refinement sets and the encoding, and just RFC-3986
/// percent-encode everything outside the unreserved set. Reached at startup via
/// MoleWorld's immob SDK building a tracking URL from the web-view user agent;
/// offline the URL is never sent, so exact escaping doesn't matter — it only
/// needs to not abort. Returns a +1 (owned) string, per the CF "Create" rule.
#[allow(non_snake_case)]
fn CFURLCreateStringByAddingPercentEscapes(
    env: &mut Environment,
    _allocator: id,
    original_string: id,
    _chars_to_leave_unescaped: id,
    _legal_chars_to_be_escaped: id,
    _encoding: u32,
) -> id {
    if original_string.is_null() {
        return nil;
    }
    let s = to_rust_string(env, original_string);
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    from_rust_string(env, out)
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(dispatch_once(_, _)),
    export_c_func!(_dyld_register_func_for_add_image(_)),
    export_c_func!(CFURLCreateStringByAddingPercentEscapes(_, _, _, _, _)),
];
