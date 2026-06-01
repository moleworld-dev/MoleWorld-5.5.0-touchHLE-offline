/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSProcessInfo`.

use super::NSTimeInterval;
use crate::frameworks::foundation::ns_string::{self, from_rust_string};
use crate::libc::mach::host::PHYSICAL_MEMORY;
use crate::objc::{id, msg, msg_class, objc_classes, ClassExports};
use crate::Environment;
use std::time::Instant;

#[derive(Default)]
pub struct State {
    /// `NSProcessInfo*`
    process_info: Option<id>,
    /// Monotonic counter for `globallyUniqueString`.
    unique_counter: u64,
}

fn assert_process_info_singleton(env: &mut Environment, this: id) {
    assert_eq!(
        this,
        env.framework_state
            .foundation
            .ns_process_info
            .process_info
            .unwrap()
    );
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSProcessInfo: NSObject

+ (id)processInfo {
    if let Some(existing) = env.framework_state.foundation.ns_process_info.process_info {
        existing
    } else {
        let process_info: id = msg![env; this new];
        env.framework_state.foundation.ns_process_info.process_info = Some(process_info);
        process_info
    }
}

- (NSTimeInterval)systemUptime {
    assert_process_info_singleton(env, this); // TODO
    Instant::now().duration_since(env.startup_time).as_secs_f64()
}

- (u64)physicalMemory {
    assert_process_info_singleton(env, this); // TODO
    PHYSICAL_MEMORY.into()
}

- (id)processName {
    // This function probably just needs to return a unique value
    // Testing on macOS appears CFBundleName is used
    assert_process_info_singleton(env, this); // TODO
    let main_bundle: id = msg_class![env; NSBundle mainBundle];
    let name_key: id = ns_string::get_static_str(env, "CFBundleName");
    msg![env; main_bundle objectForInfoDictionaryKey:name_key]
}

// Returns a globally unique string (process-scoped). Real iOS returns
// "<process>-<pid>-<counter>". We just need a stable-per-call unique value;
// OpenUDID (used by MoleWorld via UIDeviceIdentifierAddition) hashes it into a
// device identifier. Use a monotonic counter so repeated calls differ.
- (id)globallyUniqueString {
    let counter = {
        let c = &mut env.framework_state.foundation.ns_process_info.unique_counter;
        *c += 1;
        *c
    };
    let s = format!("touchHLE-MoleWorld-{counter:08X}-0000-0000");
    let str = from_rust_string(env, s);
    crate::objc::autorelease(env, str)
}

- (id)operatingSystemVersionString {
    // Matches what -[UIDevice systemVersion] reports elsewhere in touchHLE.
    let s = from_rust_string(env, "Version 2.0 (Build touchHLE)".to_string());
    crate::objc::autorelease(env, s)
}

- (u32)operatingSystem {
    // NSMACHOperatingSystem = 5 (the value for iPhone OS / OS X era).
    5
}

- (id)operatingSystemName {
    let s = from_rust_string(env, "NSMACHOperatingSystem".to_string());
    crate::objc::autorelease(env, s)
}

- (id)hostName {
    let s = from_rust_string(env, "localhost".to_string());
    crate::objc::autorelease(env, s)
}

- (u32)processorCount {
    1
}
- (u32)activeProcessorCount {
    1
}

@end

};
