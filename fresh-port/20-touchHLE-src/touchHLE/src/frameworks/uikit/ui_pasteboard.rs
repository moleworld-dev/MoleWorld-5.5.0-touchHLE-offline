/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIPasteboard`.
//!
//! Minimal in-memory pasteboard. Offline, single-app, so there's nothing to
//! share between apps; we just keep a per-instance string/items value so code
//! that round-trips through the pasteboard doesn't crash. Some games (e.g.
//! MoleWorld) call +pasteboardWithName:create: during boot.

use crate::objc::{id, msg, msg_class, nil, objc_classes, ClassExports, HostObject, NSZonePtr};

#[derive(Default)]
struct UIPasteboardHostObject {
    string: id,
    items: id,
}
impl HostObject for UIPasteboardHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIPasteboard: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIPasteboardHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)generalPasteboard {
    let new: id = msg![env; this alloc];
    msg![env; new init]
}

+ (id)pasteboardWithName:(id)_name // NSString*
                  create:(bool)_create {
    let new: id = msg![env; this alloc];
    msg![env; new init]
}

+ (id)pasteboardWithUniqueName {
    let new: id = msg![env; this alloc];
    msg![env; new init]
}

+ (())removePasteboardWithName:(id)_name {
    // No-op: we don't persist named pasteboards.
}

- (id)name {
    nil
}
- (())setPersistent:(bool)_persistent {
    // We don't persist pasteboards; accept and ignore.
}
- (bool)isPersistent {
    false
}

- (id)string {
    env.objc.borrow::<UIPasteboardHostObject>(this).string
}
- (())setString:(id)string {
    env.objc.borrow_mut::<UIPasteboardHostObject>(this).string = string;
}

- (id)items {
    env.objc.borrow::<UIPasteboardHostObject>(this).items
}
- (())setItems:(id)items {
    env.objc.borrow_mut::<UIPasteboardHostObject>(this).items = items;
}

// Typed data accessors. We don't model UTI types; the pasteboard starts empty,
// so reads return nil and writes are accepted but not stored per-type.
- (id)dataForPasteboardType:(id)_type { // NSString*
    nil
}
- (())setData:(id)_data // NSData*
   forPasteboardType:(id)_type { // NSString*
}
- (id)valueForPasteboardType:(id)_type {
    nil
}
- (())setValue:(id)_value forPasteboardType:(id)_type {
}
- (bool)containsPasteboardTypes:(id)_types {
    false
}
- (id)pasteboardTypes {
    msg_class![env; NSArray array]
}

@end

};
