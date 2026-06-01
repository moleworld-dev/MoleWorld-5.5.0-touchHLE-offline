/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSPredicate`.
//!
//! This is a deliberately minimal implementation. The only consumer in the
//! games we target is cocos2d-iphone's `CCTableView`, which manages its cells
//! with a handful of integer-comparison predicates created via
//! `+predicateWithFormat:` and applied via `-[NSArray filteredArrayUsingPredicate:]`:
//!
//! - `@"idx == %i"`
//! - `@"idx >= %i"`
//! - `@"idx > %i"`
//!
//! We parse the simple `<key> <op> <rhs>` shape (supporting all the standard
//! comparison operators for good measure), where the key is a KVC key resolved
//! by sending it as a getter selector to the evaluated object, and the right
//! hand side is an integer (literal, or substituted from a `%i`/`%d` argument).
//! Anything we don't understand evaluates as "matches everything" so we never
//! silently drop elements from a general-purpose array filter.

use super::ns_string::to_rust_string;
use crate::objc::{
    id, msg, msg_send, objc_classes, ClassExports, HostObject, NSZonePtr, SEL,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PredicateOp {
    /// Always evaluates to true (unsupported / un-parsed format).
    True,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

struct PredicateHostObject {
    /// KVC key on the left-hand side, e.g. "idx".
    key: String,
    op: PredicateOp,
    /// Right-hand-side integer bound.
    bound: i64,
}
impl HostObject for PredicateHostObject {}

fn parse_op(token: &str) -> Option<PredicateOp> {
    Some(match token {
        "==" | "=" => PredicateOp::Eq,
        "!=" | "<>" => PredicateOp::Ne,
        "<" => PredicateOp::Lt,
        "<=" | "=<" => PredicateOp::Le,
        ">" => PredicateOp::Gt,
        ">=" | "=>" => PredicateOp::Ge,
        _ => return None,
    })
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSPredicate: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(PredicateHostObject {
        key: String::new(),
        op: PredicateOp::True,
        bound: 0,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)predicateWithFormat:(id)format, ...args { // NSString* format
    let raw = to_rust_string(env, format);
    let format_str = raw.trim().to_string();

    // Tokenise on whitespace. The formats we care about are "<key> <op> <rhs>".
    let tokens: Vec<&str> = format_str.split_whitespace().collect();
    let mut varargs = args.start();

    let parsed: Option<(String, PredicateOp, i64)> = (|| {
        if tokens.len() != 3 {
            return None;
        }
        let op = parse_op(tokens[1])?;
        let rhs = tokens[2];
        let bound: i64 = match rhs {
            "%i" | "%d" | "%u" => varargs.next::<i32>(env) as i64,
            "%lld" | "%ld" => varargs.next::<i64>(env),
            _ => rhs.parse::<i64>().ok()?,
        };
        Some((tokens[0].to_string(), op, bound))
    })();

    let (key, op, bound) = match parsed {
        Some(parsed) => parsed,
        None => {
            log_dbg!("NSPredicate: unsupported format {:?}", format_str);
            (String::new(), PredicateOp::True, 0)
        }
    };

    let predicate: id = msg![env; this alloc];
    let host_object = env.objc.borrow_mut::<PredicateHostObject>(predicate);
    host_object.key = key;
    host_object.op = op;
    host_object.bound = bound;
    let predicate: id = msg![env; predicate autorelease];
    predicate
}

- (bool)evaluateWithObject:(id)object {
    let (key, op, bound) = {
        let h = env.objc.borrow::<PredicateHostObject>(this);
        (h.key.clone(), h.op, h.bound)
    };

    if op == PredicateOp::True {
        return true;
    }

    // Resolve the KVC key as a getter selector and read the integer value.
    let sel: SEL = env.objc.register_host_selector(key, &mut env.mem);
    let responds: bool = msg![env; object respondsToSelector:sel];
    if !responds {
        return false;
    }
    let lhs: i64 = {
        let v: i32 = msg_send(env, (object, sel));
        v as i64
    };

    match op {
        PredicateOp::Eq => lhs == bound,
        PredicateOp::Ne => lhs != bound,
        PredicateOp::Lt => lhs < bound,
        PredicateOp::Le => lhs <= bound,
        PredicateOp::Gt => lhs > bound,
        PredicateOp::Ge => lhs >= bound,
        PredicateOp::True => true,
    }
}

@end

};
