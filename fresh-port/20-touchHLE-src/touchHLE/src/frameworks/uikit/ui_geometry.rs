//! `UIGeometry.h`
//!
//! See also [crate::frameworks::core_graphics::cg_geometry].

use crate::dyld::{export_c_func, FunctionExports};
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string;
use crate::objc::{autorelease, id};
use crate::Environment;

// Apple's documentation says all of these return zeroes if the input is not
// well-formed. A nil string counts as not-well-formed (ObjC nil semantics) —
// the NSKeyedUnarchiver decodeCG*ForKey: helpers pass through whatever
// decodeObjectForKey: returned, which is nil for a key absent from an old save,
// so we must treat nil as zero here rather than letting to_rust_string panic.
pub fn CGPointFromString(env: &mut Environment, string: id) -> CGPoint {
    if string == crate::objc::nil {
        return Default::default();
    }
    // TODO: avoid copy
    ns_string::to_rust_string(env, string)
        .parse()
        .unwrap_or_default()
}
pub fn CGSizeFromString(env: &mut Environment, string: id) -> CGSize {
    if string == crate::objc::nil {
        return Default::default();
    }
    // TODO: avoid copy
    ns_string::to_rust_string(env, string)
        .parse()
        .unwrap_or_default()
}
pub fn CGRectFromString(env: &mut Environment, string: id) -> CGRect {
    if string == crate::objc::nil {
        return Default::default();
    }
    // TODO: avoid copy
    ns_string::to_rust_string(env, string)
        .parse()
        .unwrap_or_default()
}

pub fn NSStringFromCGPoint(env: &mut Environment, point: CGPoint) -> id {
    let s = ns_string::from_rust_string(env, point.to_string());
    autorelease(env, s)
}
pub fn NSStringFromCGSize(env: &mut Environment, size: CGSize) -> id {
    let s = ns_string::from_rust_string(env, size.to_string());
    autorelease(env, s)
}
pub fn NSStringFromCGRect(env: &mut Environment, rect: CGRect) -> id {
    let s = ns_string::from_rust_string(env, rect.to_string());
    autorelease(env, s)
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CGPointFromString(_)),
    export_c_func!(CGSizeFromString(_)),
    export_c_func!(CGRectFromString(_)),
    export_c_func!(NSStringFromCGPoint(_)),
    export_c_func!(NSStringFromCGSize(_)),
    export_c_func!(NSStringFromCGRect(_)),
];
