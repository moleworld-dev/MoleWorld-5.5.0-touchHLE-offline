/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFPropertyList` — just enough to parse a plist (XML or binary) from data.
//!
//! MoleWorld decrypts its bundled data files (shop items, buildings, quests,
//! map objects, …) to binary-plist bytes and parses them with the C function
//! `CFPropertyListCreateFromXMLData`. Without this the call linked to a no-op
//! stub returning 0, so every such list came back empty (e.g. the shop/build
//! menus showed no items). The function name says "XML" but Apple's
//! implementation accepts binary plists too, so we reuse the shared plist
//! deserializer which sniffs the format.

use super::cf_allocator::CFAllocatorRef;
use super::cf_data::CFDataRef;
use super::CFTypeRef;
use crate::dyld::FunctionExports;
use crate::export_c_func;
use crate::frameworks::foundation::ns_data::to_rust_slice;
use crate::frameworks::foundation::ns_property_list_serialization::{
    deserialize_plist_from_bytes, NSPropertyListMutabilityOptions,
};
use crate::mem::MutPtr;
use crate::objc::{id, nil};
use crate::Environment;

type CFPropertyListRef = CFTypeRef;
type CFOptionFlags = u32;
type CFStringRef = CFTypeRef;

/// `CFPropertyListRef CFPropertyListCreateFromXMLData(CFAllocatorRef allocator,
///   CFDataRef xmlData, CFOptionFlags mutabilityOption, CFStringRef *errorString)`
///
/// Returns a +1-retained property list (toll-free bridged NSArray/NSDictionary/
/// …), or NULL on failure. `errorString` is cleared to NULL.
fn CFPropertyListCreateFromXMLData(
    env: &mut Environment,
    _allocator: CFAllocatorRef,
    xml_data: CFDataRef,
    mutability_option: CFOptionFlags,
    error_string: MutPtr<CFStringRef>,
) -> CFPropertyListRef {
    if !error_string.is_null() {
        env.mem.write(error_string, nil);
    }
    if xml_data == nil {
        return nil;
    }
    // Copy the bytes out first (deserialize borrows env.mem mutably).
    let bytes = to_rust_slice(env, xml_data).to_vec();
    let plist: id = deserialize_plist_from_bytes(
        env,
        &bytes,
        mutability_option as NSPropertyListMutabilityOptions,
    );
    // deserialize_plist_from_bytes returns a +1-owned object, which matches the
    // CF "Create" ownership convention — hand it straight back to the caller.
    plist
}

pub const FUNCTIONS: FunctionExports =
    &[export_c_func!(CFPropertyListCreateFromXMLData(_, _, _, _))];
