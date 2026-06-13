/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Security framework — minimal keychain (SecItem*) stub.
//!
//! MoleWorld's login reads/writes the account password via SFHFKeychainUtils
//! (+[PBSFHFKeychainUtils getPasswordForUsername:andServiceName:error:]), which uses
//! `SecItemCopyMatching` + the `kSec*` query-key constants. touchHLE never implemented
//! Security.framework, so offline those `kSec*` data symbols are unresolved (GOT == 0)
//! and the very first `[NSArray initWithObjects:kSecClass, ...]` load faults reading
//! address 0 — which crashed the app the moment online mode reached the keychain path.
//!
//! There is no real keychain here, so we export the `kSec*` keys as harmless non-null
//! CFString sentinels and make the SecItem* calls report "item not found"
//! (errSecItemNotFound). The keychain helpers then take their clean "no stored password"
//! branch and return nil instead of crashing. Good enough for online login: accounts
//! with no server-side password are accepted leniently, and MOLE_PASSWORD (when set) is
//! injected separately via the taomeePassword hook in mole_cheats.

use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::CFTypeRef;
use crate::mem::MutPtr;
use crate::objc::nil;
use crate::Environment;

/// errSecItemNotFound — "the item could not be found".
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

fn SecItemCopyMatching(
    env: &mut Environment,
    _query: CFTypeRef,
    result: MutPtr<CFTypeRef>,
) -> i32 {
    if !result.is_null() {
        env.mem.write(result, nil);
    }
    ERR_SEC_ITEM_NOT_FOUND
}

fn SecItemAdd(env: &mut Environment, _attributes: CFTypeRef, result: MutPtr<CFTypeRef>) -> i32 {
    // Pretend the write succeeded (no real keychain); don't return an item.
    if !result.is_null() {
        env.mem.write(result, nil);
    }
    0 // errSecSuccess
}

fn SecItemUpdate(_env: &mut Environment, _query: CFTypeRef, _attributes_to_update: CFTypeRef) -> i32 {
    0 // errSecSuccess
}

fn SecItemDelete(_env: &mut Environment, _query: CFTypeRef) -> i32 {
    0 // errSecSuccess
}

pub const CONSTANTS: ConstantExports = &[
    ("_kSecClass", HostConstant::NSString("kSecClass")),
    (
        "_kSecClassGenericPassword",
        HostConstant::NSString("kSecClassGenericPassword"),
    ),
    ("_kSecAttrAccount", HostConstant::NSString("kSecAttrAccount")),
    ("_kSecAttrService", HostConstant::NSString("kSecAttrService")),
    ("_kSecAttrGeneric", HostConstant::NSString("kSecAttrGeneric")),
    ("_kSecValueData", HostConstant::NSString("kSecValueData")),
    ("_kSecReturnData", HostConstant::NSString("kSecReturnData")),
    (
        "_kSecReturnAttributes",
        HostConstant::NSString("kSecReturnAttributes"),
    ),
    ("_kSecMatchLimit", HostConstant::NSString("kSecMatchLimit")),
    (
        "_kSecMatchLimitOne",
        HostConstant::NSString("kSecMatchLimitOne"),
    ),
];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(SecItemCopyMatching(_, _)),
    export_c_func!(SecItemAdd(_, _)),
    export_c_func!(SecItemUpdate(_, _)),
    export_c_func!(SecItemDelete(_)),
];

pub const DYLIB: crate::dyld::HostDylib = crate::dyld::HostDylib {
    path: "/System/Library/Frameworks/Security.framework/Security",
    aliases: &[],
    class_exports: &[],
    constant_exports: &[CONSTANTS],
    function_exports: &[FUNCTIONS],
};
