/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITextView`.

use crate::frameworks::core_graphics::cg_context::CGContextSetRGBFillColor;
use crate::frameworks::core_graphics::cg_geometry::CGPointZero;
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::ns_string::{get_static_str, to_rust_string};
use crate::frameworks::foundation::{NSRange, NSUInteger};
use crate::frameworks::uikit::ui_color;
use crate::frameworks::uikit::ui_font::{
    break_lines_with_font, UILineBreakModeTailTruncation, UILineBreakModeWordWrap, UITextAlignment,
    UITextAlignmentLeft,
};
use crate::frameworks::uikit::ui_graphics::UIGraphicsGetCurrentContext;
use crate::frameworks::uikit::ui_view::ui_control::ui_text_field::UIReturnKeyType;
use crate::objc::{
    id, impl_HostObject_with_superclass, msg, msg_class, msg_super, nil, objc_classes, release,
    retain, todo_objc_setter, ClassExports, NSZonePtr,
};
use crate::dyld::{ConstantExports, HostConstant};
use crate::Environment;

type UIDataDetectorTypes = NSUInteger;

/// `UITextView` 文本通知名。摩尔庄园好友留言板 -[LeaveMessageLayer init] 一进来就
/// `addObserver:selector:name:UITextViewTextDidChangeNotification object:`;若不导出这个
/// NSString 常量,guest 的非惰性符号指针 `_UITextViewTextDidChangeNotification_ptr` 留 0,
/// 取常量值的 `LDR Rn,[Rn]`(Rn=0)就 null 页读 → MemoryError 整机崩(PC=0x1b1bd4)。
/// 只需非空即可:touchHLE 不实际 post 这些通知(留言框不随编辑自动反应,这里无害)。
pub const UITextViewTextDidChangeNotification: &str = "UITextViewTextDidChangeNotification";
pub const UITextViewTextDidBeginEditingNotification: &str =
    "UITextViewTextDidBeginEditingNotification";
pub const UITextViewTextDidEndEditingNotification: &str =
    "UITextViewTextDidEndEditingNotification";

/// `NSNotificationName` values.
pub const CONSTANTS: ConstantExports = &[
    (
        "_UITextViewTextDidChangeNotification",
        HostConstant::NSString(UITextViewTextDidChangeNotification),
    ),
    (
        "_UITextViewTextDidBeginEditingNotification",
        HostConstant::NSString(UITextViewTextDidBeginEditingNotification),
    ),
    (
        "_UITextViewTextDidEndEditingNotification",
        HostConstant::NSString(UITextViewTextDidEndEditingNotification),
    ),
];

pub struct UITextViewHostObject {
    superclass: super::UIScrollViewHostObject,
    editable: bool,
    /// `NSString*`
    text: id,
    /// `UIFont*`
    font: id,
    /// `UIColor*`
    text_color: id,
    text_alignment: UITextAlignment,
}
impl_HostObject_with_superclass!(UITextViewHostObject);
impl Default for UITextViewHostObject {
    fn default() -> Self {
        UITextViewHostObject {
            superclass: Default::default(),
            editable: false,
            font: nil,
            text: nil,
            text_color: nil,
            text_alignment: UITextAlignmentLeft,
        }
    }
}

// Update contentOffset and contentSize when anything that potentially affects
// contentSize like font and text change.
fn update_scroll(env: &mut Environment, this: id) {
    let bounds: CGRect = msg![env; this bounds];
    let bound_size = bounds.size;
    let font: id = msg![env; this font];
    let text: id = msg![env; this text];

    // Calculate our new contentSize
    let calculated_size: CGSize = msg![env; text sizeWithFont:font constrainedToSize:bound_size];
    () = msg![env; this setContentSize:calculated_size];

    // Reset contentOffset if we have now gone out of bounds of contentSize,
    // otherwise ignore.
    let current_content_offset: CGPoint = msg![env; this contentOffset];
    if current_content_offset.x > calculated_size.width - bounds.size.width
        || current_content_offset.y > calculated_size.height - bounds.size.height
    {
        () = msg![env; this setContentOffset:(CGPoint { x: 0.0, y: 0.0 })];
    }
}
pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITextView: UIScrollView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITextViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];
    // TODO: refactor to a common init with `initWithCoder:`
    // These aren't redundant, the setters fetch the real defaults.
    () = msg![env; this setFont:nil];
    () = msg![env; this setTextColor:nil];
    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder:coder];
    // These aren't redundant, the setters fetch the real defaults.
    () = msg![env; this setFont:nil];
    () = msg![env; this setTextColor:nil];
    this
}

- (())dealloc {
    let UITextViewHostObject {
        superclass: _,
        editable: _,
        font,
        text,
        text_color,
        text_alignment: _
    } = std::mem::take(env.objc.borrow_mut(this));

    release(env, font);
    release(env, text_color);
    release(env, text);
    msg_super![env; this dealloc]
}

- (id)text {
    let text = env.objc.borrow::<UITextViewHostObject>(this).text;
    // iOS 保证 UITextView.text 永不为 nil(未设值默认 @"")。摩尔庄园 -[GiftAndMessageLayer
    // displayUI] 直接 strlen([textView.text UTF8String]) 不做 nil 检查;若这里返回 nil →
    // [nil UTF8String]=NULL → strlen(NULL) → null 页读 MemoryError 整机崩(PC=0x884cb4,
    // 调用者 -[GiftAndMessageLayer displayUI]@0x3e11c0)。未设值时回空串,与 iOS 行为一致。
    if text == nil {
        get_static_str(env, "")
    } else {
        text
    }
}
- (())setText:(id)new_text { // NSString*
    let hostobj  = env.objc.borrow_mut::<UITextViewHostObject>(this);
    let old_text = std::mem::replace(&mut hostobj.text, new_text);
    retain(env, new_text);
    release(env, old_text);
    update_scroll(env,this);
    () = msg![env; this setNeedsDisplay];
}

- (id)textColor {
    env.objc.borrow::<UITextViewHostObject>(this).text_color
}
- (())setTextColor:(id)new_text_color { // UIColor*
    let new_text_color: id = if new_text_color == nil {
        msg_class![env; UIColor blackColor]
    } else {
        new_text_color
    };

    let hostobj  = env.objc.borrow_mut::<UITextViewHostObject>(this);
    let old_text_color = std::mem::replace(&mut hostobj.text_color, new_text_color);
    retain(env, new_text_color);
    release(env, old_text_color);
    () = msg![env; this setNeedsDisplay];
}

- (UITextAlignment)textAlignment {
    env.objc.borrow::<UITextViewHostObject>(this).text_alignment
}
- (())setTextAlignment:(UITextAlignment)new_text_alignment {
    env.objc.borrow_mut::<UITextViewHostObject>(this).text_alignment = new_text_alignment;
    () = msg![env; this setNeedsDisplay];
}

- (id)font {
    env.objc.borrow::<UITextViewHostObject>(this).font
}
- (())setFont:(id)new_font { // UIFont*
    let new_font: id = if new_font == nil {
        // reset to default
        let size: CGFloat = 17.0;
        msg_class![env; UIFont systemFontOfSize:size]
    } else {
        new_font
    };

    let hostobj  = env.objc.borrow_mut::<UITextViewHostObject>(this);
    let old_font = std::mem::replace(&mut hostobj.font, new_font);
    retain(env, new_font);
    release(env, old_font);
    update_scroll(env,this);
    () = msg![env; this setNeedsDisplay];
}

- (())flashScrollIndicators {
    // TODO
}

// TODO: Make editable actually do something
- (bool)isEditable {
    env.objc.borrow::<UITextViewHostObject>(this).editable
}
- (())setEditable:(bool)editable {
    env.objc.borrow_mut::<UITextViewHostObject>(this).editable = editable;
}

- (())scrollRangeToVisible:(NSRange)range {
    let &mut UITextViewHostObject {
        font,
        text,
        ..
    } = env.objc.borrow_mut(this);

    if range.location > msg![env; text length] {
        return;
    }

    let bounds: CGRect = msg![env; this bounds];
    let bound_size = bounds.size;

    let text = to_rust_string(env, text);

    let lines = break_lines_with_font(env, font, &text, Some((bound_size, UILineBreakModeWordWrap)));

    let mut line_count = 0;
    let mut current_position = 0;

    for (_, line) in lines {
        current_position += line.len();

        if let Some(offset) = text[current_position..].find(|c: char| !c.is_whitespace()) {
            current_position += offset;
        } else {
            current_position = text.len();
        }

        if range.location <= current_position as u32 {
            break;
        }

        line_count += 1;
    }

    let line_height: CGFloat = msg![env; font lineHeight];
    let leading: CGFloat = msg![env; font leading];

    let height_to_range_start = (line_count + 1) as f32 * line_height - leading;

    let content_offset: CGPoint = msg![env; this contentOffset];

    if height_to_range_start - line_height < content_offset.y {
        let new_scroll_y = CGPoint {x: 0.0, y: line_count as f32 * line_height};
        () = msg![env; this setContentOffset:new_scroll_y];
    }
    else if height_to_range_start > content_offset.y + bound_size.height {
        let new_scroll_y = CGPoint {x: 0.0, y: height_to_range_start- bound_size.height};
        () = msg![env; this setContentOffset:new_scroll_y];
    }

    update_scroll(env, this);
}

- (())setReturnKeyType:(UIReturnKeyType)type_ {
    todo_objc_setter!(this, type_);
}

- (())setDataDetectorTypes:(UIDataDetectorTypes)types {
    todo_objc_setter!(this, types);
}

- (())drawRect:(CGRect)_rect {
    let bounds: CGRect = msg![env; this bounds];
    let context = UIGraphicsGetCurrentContext(env);

    let &mut UITextViewHostObject {
        superclass: _,
        editable: _,
        font,
        text,
        text_color,
        text_alignment
    } = env.objc.borrow_mut(this);

    let (r, g, b, a) = ui_color::get_rgba(&env.objc, text_color);
    CGContextSetRGBFillColor(env, context, r, g, b, a);

    let content_offset: CGPoint = msg![env; this contentOffset];
    let rect = CGRect {
        origin: CGPointZero,
        // If size is not expanded by the offset,
        // the text is rendered truncated.
        size: CGSize {
            width: bounds.size.width + content_offset.x,
            height: bounds.size.height + content_offset.y,
        }
    };

    log_dbg!("UItextView text rendering in rect {:?}", rect);
    let _size: CGSize = msg![env; text drawInRect:rect
                                         withFont:font
                                    lineBreakMode:UILineBreakModeTailTruncation
                                        alignment:text_alignment];
}

@end

};
