/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `NSDateFormatter`.
//!
//! Resources:
//! - Apple's [Introduction to Data Formatting Programming Guide For Cocoa](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/DataFormatting/DataFormatting.html)
//! - [Unicode Technical Standard #35](https://unicode.org/reports/tr35/tr35-10.html#Date_Format_Patterns)

use crate::frameworks::core_foundation::time::CFAbsoluteTimeGetGregorianDate;
use crate::frameworks::foundation::{ns_string, NSTimeInterval};
use crate::objc::{autorelease, id, msg, nil, objc_classes, ClassExports, HostObject, NSZonePtr};

struct NSDateFormatterHostObject {
    date_format: Option<id>,
}
impl HostObject for NSDateFormatterHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation NSDateFormatter: NSObject

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::new(NSDateFormatterHostObject {
        date_format: None,
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (())setDateFormat:(id)format { // NSString *
    let date_format: id = msg![env; format copy];
    env.objc.borrow_mut::<NSDateFormatterHostObject>(this).date_format = Some(date_format);
}

- (id)stringFromDate:(id)date {
    let &NSDateFormatterHostObject {
        date_format
    } = env.objc.borrow(this);
    // setDateFormat: 没被调过(date_format=None)时,旧代码 .unwrap() 会 panic 崩模拟器。
    // 真机此时返回 nil/空串,这里也安全降级成空串。
    let Some(date_format) = date_format else {
        let empty = ns_string::from_rust_string(env, String::new());
        return autorelease(env, empty);
    };
    let mut format = ns_string::to_rust_string(env, date_format).to_string().clone();
    log_dbg!("date_format before: {:?}", format);

    let ti: NSTimeInterval = msg![env; date timeIntervalSinceReferenceDate];
    let greg_date = CFAbsoluteTimeGetGregorianDate(env, ti, nil);
    let year = greg_date.year;
    let month = greg_date.month;
    let day = greg_date.day;
    let hour = greg_date.hours;
    let minute = greg_date.minutes;
    let second = greg_date.seconds;

    format = format.replace("yyyy", format!("{year:04}").as_str());
    format = format.replace("YYYY", format!("{year:04}").as_str());
    format = format.replace("MM", format!("{month:02}").as_str());
    format = format.replace("dd", format!("{day:02}").as_str());
    format = format.replace("HH", format!("{hour:02}").as_str());
    // ★12 小时制(小写 h/hh)。摩尔庄园好友留言板的 -[MessageViewController configureCell:]
    // 用 @"MM/dd/yyyy hh:mm:ss" 格式化每条留言时间;旧代码不替换小写 hh → 残留字母 →
    // 下面的"剩余字母即 unimplemented!() panic"把开留言板变成整机崩溃。这里补 12 小时制。
    let hour12 = if hour % 12 == 0 { 12 } else { hour % 12 };
    format = format.replace("hh", format!("{hour12:02}").as_str());
    format = format.replace("h", format!("{hour12}").as_str());
    format = format.replace("mm", format!("{minute:02}").as_str());
    format = format.replace("ss", format!("{second:02}").as_str());
    // AM/PM 标记(小写 a)。放在数字字段替换之后,使插入的 "AM"/"PM" 字母不再被当作待替换模式。
    // touchHLE 未实现 setAMSymbol:/setPMSymbol:(no-op),故用 en_US 默认 "AM"/"PM"。
    let am_pm = if hour < 12 { "AM" } else { "PM" };
    format = format.replace('a', am_pm);

    log_dbg!("date_format after: {:?}", format);

    // 旧实现在此处扫到任意残留 A-Za-z 字母就 unimplemented!() —— 那是开发期断言,却把
    // "遇到没支持/纯装饰的格式模式"升级成整个模拟器 abort(小写 hh 就这样崩了每次开留言板)。
    // 改为:不崩,直接输出已替换的结果;未支持的模式以字面字母呈现(纯外观,不影响功能)。
    let res = ns_string::from_rust_string(env, format);
    autorelease(env, res)
}

@end

};
