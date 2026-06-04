/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSKeyedArchiver` and serialization of its object graph format.
//!
//! Resources:
//! - You can get a good intuitive grasp of how the format works just by staring
//!   at a pretty-print of a simple archive file from something that can parse
//!   plists, e.g. `plutil -p` or `println!("{:#?}", plist::Value::...);`.
//! - Apple's [Archives and Serializations Programming Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Archiving/Articles/archives.html)

use std::collections::HashMap;
use std::io::Cursor;

use plist::{to_writer_binary, Dictionary, Uid, Value};

use crate::frameworks::foundation::ns_keyed_unarchiver::NSKeyedArchiveRootObjectKey;
use crate::frameworks::foundation::ns_string::{get_static_str, to_rust_string};
use crate::frameworks::foundation::NSUInteger;
use crate::mem::{ConstPtr, GuestUSize};
use crate::objc::{
    id, msg, msg_class, nil, objc_classes, release, retain, ClassExports, HostObject, NSZonePtr,
};
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::uikit::ui_geometry::{
    NSStringFromCGPoint, NSStringFromCGRect, NSStringFromCGSize,
};
use crate::Environment;

struct NSKeyedArchiverHostObject {
    plist: Dictionary,
    encoded_data: id, // NSData *
    /// `NSMutableData*` supplied via initForWritingWithMutableData:, retained.
    /// When set, finishEncoding appends the archive bytes to it (old-style API).
    output_data: id,
    current_key: Option<Uid>,
    /// map of id => Uid
    already_archived: HashMap<id, Uid>,
    /// Every *instance* archived during this session, retained until the
    /// archiver is deallocated. See `encode_object` for why: without keeping
    /// archived objects alive, a guest pointer freed mid-archive can be reused
    /// by a later object, causing `already_archived` (keyed on the raw pointer)
    /// to false-hit and collapse distinct fields into one — corrupting saves.
    retained_objects: Vec<id>,
}
impl HostObject for NSKeyedArchiverHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSKeyedArchiver: NSCoder

+ (id)allocWithZone:(NSZonePtr)_zone {
    let mut plist = Dictionary::new();
    // Archives made by NSKeyedArchiver have the plist pre-populated with these
    plist.insert("$archiver".into(), "NSKeyedArchiver".into());
    // The $objects begins with nil, stored as the string "$null".
    plist.insert("$objects".into(), Value::Array(vec![Value::String("$null".into())]));
    plist.insert("$top".into(), Dictionary::new().into());
    plist.insert("$version".into(), 100000.into());
    // Map nil to the first element in the $objects array ("$null")
    let mut already_archived = HashMap::new();
    already_archived.insert(nil, Uid::new(0));
    env.objc.alloc_object(this, Box::new(NSKeyedArchiverHostObject {
        plist,
        encoded_data: nil,
        output_data: nil,
        current_key: None,
        already_archived,
        retained_objects: Vec::new(),
    }), &mut env.mem)
}


+ (id)archivedDataWithRootObject:(id)root_object { // NSCoding *
    let key = get_static_str(env, NSKeyedArchiveRootObjectKey);
    let instance: id = msg_class![env; NSKeyedArchiver new];
    () = msg![env; instance encodeObject:root_object forKey:key];
    let data: id = msg![env; instance encodedData];
    let data: id = msg_class![env; NSData dataWithData:data];
    release(env, instance);
    data
}

+ (bool)archiveRootObject:(id)root_object // NSCoding *
                   toFile:(id)file { // NSString *
    log_dbg!("[NSKeyedArchiver archiveRootObject:{:?} toFile:{:?}('{}')]", root_object, file, to_rust_string(env, file));
    let data: id = msg![env; this archivedDataWithRootObject:root_object];
    msg![env; data writeToFile:file atomically:true]
}

// Old-style API: -[[NSKeyedArchiver alloc] initForWritingWithMutableData:]. The
// archiver writes the finished archive into `data` on finishEncoding. MoleWorld
// uses this to save game state.
- (id)initForWritingWithMutableData:(id)data { // NSMutableData *
    retain(env, data);
    env.objc.borrow_mut::<NSKeyedArchiverHostObject>(this).output_data = data;
    this
}

- (())encodeObject:(id)object // NSCoding *
            forKey:(id)key { // NSString *
    let key = normalize_key(env, key);
    encode_object_for_key(env, this, object, key);
}

- (())encodeInt:(i32)val
         forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Integer(val.into()));
}

// NSInteger / SInt32 / SInt64 variants — MoleWorld uses these (plus Double/Bool)
// to save its game state via -[NSKeyedArchiver initForWritingWithMutableData:].
- (())encodeInteger:(i32)val // NSInteger (32-bit guest)
             forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Integer(val.into()));
}
- (())encodeInt32:(i32)val
           forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Integer(val.into()));
}
- (())encodeInt64:(i64)val
           forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Integer(val.into()));
}
- (())encodeBool:(bool)val
          forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Boolean(val));
}
- (())encodeDouble:(f64)val
            forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Real(val));
}
- (())encodeFloat:(f32)val
           forKey:(id)key {
    let key = normalize_key(env, key);
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Real(val as f64));
}

- (())encodeBytes:(ConstPtr<u8>)bytes
           length:(NSUInteger)length
           forKey:(id)key { // NSString *
    let key = normalize_key(env, key);
    let data = env.mem.bytes_at(bytes.cast(), length).to_vec();
    let scope = get_value_to_encode_for_current_key(env, this);
    assert!(!scope.contains_key(&key));
    scope.insert(key, Value::Data(data));
}

// -[NSCoder encodeCGPoint:forKey:] / encodeCGSize: / encodeCGRect: (the UIKit
// "UIGeometry keyed coding" category). These store the struct as the string
// "{x, y}" / "{w, h}" / "{{x, y}, {w, h}}" under the key, which the matching
// decodeCG*ForKey: reads back with CG*FromString. Without them, every saved
// building/decoration/farmland CGPoint position was silently dropped, so on
// reload they all collapsed to (0,0) and vanished off-screen — and the player's
// camera/scroll position was never remembered either.
- (())encodeCGPoint:(CGPoint)point
             forKey:(id)key { // NSString *
    let string: id = NSStringFromCGPoint(env, point);
    () = msg![env; this encodeObject:string forKey:key];
}
- (())encodeCGSize:(CGSize)size
            forKey:(id)key { // NSString *
    let string: id = NSStringFromCGSize(env, size);
    () = msg![env; this encodeObject:string forKey:key];
}
- (())encodeCGRect:(CGRect)rect
            forKey:(id)key { // NSString *
    let string: id = NSStringFromCGRect(env, rect);
    () = msg![env; this encodeObject:string forKey:key];
}

- (())finishEncoding {
    let plist = &env.objc.borrow::<NSKeyedArchiverHostObject>(this).plist;
    let mut buffer = Vec::new();
    let cursor = Cursor::new(&mut buffer);
    to_writer_binary(cursor, plist).unwrap();
    let len = buffer.len() as GuestUSize;
    let guest_buffer = env.mem.alloc(len);
    env.mem.bytes_at_mut(guest_buffer.cast(), len).copy_from_slice(&buffer[..]);
    let encoded_data: id = msg_class![env; NSData dataWithBytesNoCopy:guest_buffer length:len];
    env.objc.borrow_mut::<NSKeyedArchiverHostObject>(this).encoded_data = encoded_data;
    retain(env, encoded_data);

    // Old-style API: also append the archive bytes into the NSMutableData that
    // was passed to initForWritingWithMutableData:, which is where the caller
    // reads the result from.
    let output_data = env.objc.borrow::<NSKeyedArchiverHostObject>(this).output_data;
    if output_data != nil {
        () = msg![env; output_data appendData:encoded_data];
    }
}

- (id)encodedData {
    if env.objc.borrow::<NSKeyedArchiverHostObject>(this).encoded_data == nil {
        () = msg![env; this finishEncoding];
    }
    env.objc.borrow::<NSKeyedArchiverHostObject>(this).encoded_data
}

- (())dealloc {
    let NSKeyedArchiverHostObject { encoded_data, output_data, .. } = *env.objc.borrow::<NSKeyedArchiverHostObject>(this);
    // Release every instance we kept alive during archiving (see encode_object).
    let retained = std::mem::take(
        &mut env.objc.borrow_mut::<NSKeyedArchiverHostObject>(this).retained_objects,
    );
    for obj in retained {
        release(env, obj);
    }
    release(env, encoded_data);
    if output_data != nil { release(env, output_data); }
    env.objc.dealloc_object(this, &mut env.mem);
}

@end

};

fn normalize_key(env: &mut Environment, key: id) -> String {
    assert_ne!(key, nil);
    let key = to_rust_string(env, key);
    assert!(!key.starts_with('$')); // TODO: Mangle keys with $ prefix
    key.to_string()
}

pub fn set_value_to_encode_for_current_key(env: &mut Environment, archiver: id, value: Value) {
    assert_eq!(
        env.objc
            .borrow::<NSKeyedArchiverHostObject>(archiver)
            .encoded_data,
        nil
    );
    let host_object = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(archiver);
    let current_key_idx = host_object.current_key.unwrap().get() as usize;
    host_object
        .plist
        .get_mut("$objects")
        .unwrap()
        .as_array_mut()
        .unwrap()[current_key_idx] = value;
}

pub fn get_value_to_encode_for_current_key(env: &mut Environment, archiver: id) -> &mut Dictionary {
    assert_eq!(
        env.objc
            .borrow::<NSKeyedArchiverHostObject>(archiver)
            .encoded_data,
        nil
    );
    let host_object = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(archiver);
    match host_object.current_key {
        Some(uid) => host_object
            .plist
            .get_mut("$objects")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .get_mut(uid.get() as usize)
            .unwrap(),
        None => host_object.plist.get_mut("$top").unwrap(),
    }
    .as_dictionary_mut()
    .unwrap()
}

pub fn encode_object(env: &mut Environment, archiver: id, object: id) -> Uid {
    let class = msg![env; object class];
    let host_object = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(archiver);
    if let Some(existing_uid) = host_object.already_archived.get(&object).cloned() {
        // Object has already been archived, just insert a UID reference
        existing_uid
    } else {
        // Object has not been archived yet, encode it and insert reference
        host_object
            .plist
            .get_mut("$objects")
            .unwrap()
            .as_array_mut()
            .unwrap()
            .push(Dictionary::new().into());
        let len = host_object.plist["$objects"].as_array().unwrap().len();
        let new_uid = Uid::new(len as u64 - 1);
        if object == class {
            // If the class selector returns itself, we're encoding a Class
            let mut classname = None;
            let mut classes = Vec::new();
            let mut current_class = class;
            while current_class != nil {
                let class_name = env.objc.get_class_name(current_class);
                // We don't want to encode classes of our private
                // implementations! Instead, we only encode `public`
                // classes. We also assume following general inheritance chain:
                // Class1 -> ... -> ClassN -> _touchHLE_ClassA ->
                // ... -> _touchHLE_ClassZ
                // In that case an instance of _touchHLE_ClassZ would be
                // encoded as instance of ClassN. And classes Class1 to
                // ClassN would be encoded as well.
                if !class_name.starts_with("_touchHLE") {
                    if classname.is_none() {
                        classname = Some(Value::String(class_name.into()));
                    }
                    classes.push(Value::String(class_name.into()));
                }
                current_class = env.objc.get_superclass(current_class);
            }
            let host_object = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(archiver);
            let entry = host_object
                .plist
                .get_mut("$objects")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .get_mut(new_uid.get() as usize)
                .unwrap()
                .as_dictionary_mut()
                .unwrap();
            entry.insert("$classes".into(), Value::Array(classes));
            entry.insert("$classname".into(), classname.unwrap());
        } else {
            let previous_key = env
                .objc
                .borrow_mut::<NSKeyedArchiverHostObject>(archiver)
                .current_key
                .replace(new_uid);
            let class: id = msg![env; object class];
            // TODO: it seems that NSString class itself is _not_ encoded??
            let str_class = env.objc.get_known_class("NSString", &mut env.mem);
            if !env.objc.class_is_subclass_of(class, str_class) {
                encode_object_for_key(env, archiver, class, "$class".into());
            }
            () = msg![env; object encodeWithCoder:archiver];
            env.objc
                .borrow_mut::<NSKeyedArchiverHostObject>(archiver)
                .current_key = previous_key;
        }
        // Keep this archived instance alive until the archiver is deallocated.
        // -[UserInfoData encodeWithCoder:] allocs a temporary wrapper container
        // per field (npcs / achieveUnlock / attributeValue), encodes it, then
        // releases it; the next field's wrapper would otherwise reuse the
        // just-freed guest address and false-hit `already_archived`, collapsing
        // the three distinct fields into one object and corrupting the save.
        // Retaining prevents the address from being recycled mid-archive.
        // Classes (object == class) are not refcounted instances; skip them.
        if object != class {
            retain(env, object);
            env.objc
                .borrow_mut::<NSKeyedArchiverHostObject>(archiver)
                .retained_objects
                .push(object);
        }
        let host_object = env.objc.borrow_mut::<NSKeyedArchiverHostObject>(archiver);
        host_object.already_archived.insert(object, new_uid);
        new_uid
    }
}

fn encode_object_for_key(env: &mut Environment, archiver: id, object: id, normalized_key: String) {
    let uid = encode_object(env, archiver, object);
    let scope = get_value_to_encode_for_current_key(env, archiver);
    assert!(!scope.contains_key(&normalized_key));
    scope.insert(normalized_key, Value::Uid(uid));
}
