/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSNull`.

use crate::msg;
use crate::objc::{id, objc_classes, ClassExports, NSZonePtr, TrivialHostObject};

#[derive(Default)]
pub struct State {
    null: Option<id>,
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// This is a singleton that takes the place of nil in collections which don't
// allow that value.
@implementation NSNull: NSObject

+ (id)null {
    if let Some(null) = env.framework_state.foundation.ns_null.null {
        null
    } else {
        let new = env.objc.alloc_static_object(
            this,
            Box::new(TrivialHostObject),
            &mut env.mem
        );
        env.framework_state.foundation.ns_null.null = Some(new);
        new
   }
}

// [2026-10-29 R18-A-1] 与 Apple 一样,alloc 也拿单例:NSKeyedUnarchiver 解到 $class=NSNull 时是 alloc + initWithCoder:,
// 以前 alloc 走 NSObject 新建一个实例、initWithCoder: 又落到「不响应」兜底返回接收者,解出来的不是单例。
// 游戏 -[TMMapDataWaterTower initWithCoder:]@0xced7a、-[TMMapDataRewardBox initWithCoder:]@0xce6aa 拿它和 [NSNull null] 比,
// 判等失败就把这个假 NSNull 存进 _unreceivedRewards(目前被下游 count 判断兜住),每次读档还泄漏一个对象、多打三条告警。
+ (id)allocWithZone:(NSZonePtr)_zone {
    msg![env; this null]
}

// NSCoding:Apple 的归档里 NSNull 只有 $class,没有别的字段。
- (id)initWithCoder:(id)_coder {
    this
}
- (())encodeWithCoder:(id)_coder {}

- (id)retain { this }
- (())release {}
- (id)autorelease { this }

@end

};
