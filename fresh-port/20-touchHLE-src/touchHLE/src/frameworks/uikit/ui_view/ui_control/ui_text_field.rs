/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UITextField`.
//!
//! Useful resources:
//! - [UITextFieldDelegate overview](https://developer.apple.com/documentation/uikit/uitextfielddelegate?language=objc)

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_graphics::cg_context::{
    CGContextClearRect, CGContextFillRect, CGContextRef, CGContextSetRGBFillColor,
};
use crate::frameworks::core_graphics::{CGFloat, CGPoint, CGRect, CGSize};
use crate::frameworks::foundation::{ns_string, NSInteger, NSRange, NSUInteger};
use crate::frameworks::uikit::ui_font::{UITextAlignment, UITextAlignmentLeft};
use crate::frameworks::uikit::ui_graphics::UIGraphicsGetCurrentContext;
use crate::frameworks::uikit::ui_view::ui_window::{
    UIKeyboardAnimationCurveUserInfoKey, UIKeyboardAnimationDurationUserInfoKey,
    UIKeyboardBoundsUserInfoKey, UIKeyboardCenterBeginUserInfoKey, UIKeyboardCenterEndUserInfoKey,
    UIKeyboardDidHideNotification, UIKeyboardDidShowNotification, UIKeyboardFrameBeginUserInfoKey,
    UIKeyboardFrameEndUserInfoKey, UIKeyboardWillHideNotification, UIKeyboardWillShowNotification,
};
use crate::impl_HostObject_with_superclass;
use crate::objc::{
    id, msg, msg_class, msg_super, nil, objc_classes, release, retain, todo_objc_setter,
    ClassExports, NSZonePtr, SEL,
};
use crate::Environment;

type UIKeyboardAppearance = NSInteger;
type UIKeyboardType = NSInteger;
pub type UIReturnKeyType = NSInteger;
type UITextAutocapitalizationType = NSInteger;
type UITextAutocorrectionType = NSInteger;

/// [2026-09-16] C-06:`UITextBorderStyle`。
type UITextBorderStyle = NSInteger;
const UITextBorderStyleNone: UITextBorderStyle = 0;
const UITextBorderStyleLine: UITextBorderStyle = 1;
const UITextBorderStyleBezel: UITextBorderStyle = 2;
const UITextBorderStyleRoundedRect: UITextBorderStyle = 3;

/// [2026-09-16] C-06:`UITextFieldViewMode`(leftView / 清除按钮何时显示)。
type UITextFieldViewMode = NSInteger;
const UITextFieldViewModeNever: UITextFieldViewMode = 0;
const UITextFieldViewModeWhileEditing: UITextFieldViewMode = 1;
const UITextFieldViewModeUnlessEditing: UITextFieldViewMode = 2;
const UITextFieldViewModeAlways: UITextFieldViewMode = 3;

/// [2026-09-16] C-06:`UIControlContentVerticalAlignment`。
type UIControlContentVerticalAlignment = NSInteger;
const UIControlContentVerticalAlignmentCenter: UIControlContentVerticalAlignment = 0;
const UIControlContentVerticalAlignmentTop: UIControlContentVerticalAlignment = 1;
const UIControlContentVerticalAlignmentBottom: UIControlContentVerticalAlignment = 2;
const UIControlContentVerticalAlignmentFill: UIControlContentVerticalAlignment = 3;

const UITextFieldTextDidChangeNotification: &str = "UITextFieldTextDidChangeNotification";

/// `NSNotificationName` values.
pub const CONSTANTS: ConstantExports = &[(
    "_UITextFieldTextDidChangeNotification",
    HostConstant::NSString(UITextFieldTextDidChangeNotification),
)];

struct UITextFieldHostObject {
    superclass: super::UIControlHostObject,
    delegate: id,
    editing: bool,
    /// 显示层 `UILabel*`:正文(secureTextEntry 时是同长度圆点)。强引用,同时是子视图。
    text_label: id,
    /// [2026-09-16] C-06 ⑦:原文 `NSString*`(copy 持有)。`text` getter 和键盘输入处理都读写它,
    /// text_label 只负责显示,这样密码框显示圆点时游戏仍能取到原文。
    text: id,
    /// [2026-09-16] C-06 ④:占位符显示层 `UILabel*`(灰 0.7),原文为空时显示;`placeholder` 属性就存在它的
    /// text 里。强引用,同时是子视图。
    placeholder_label: id,
    /// [2026-09-16] C-06 ②:`background` 属性 `UIImage*`(强引用)。
    background: id,
    /// [2026-09-16] C-06 ②:铺满输入框、垫在最底层的 `UIImageView*`(强引用,同时是子视图);没设底图时为 nil。
    background_view: id,
    /// [2026-09-16] C-06 ③:`leftView`(强引用);布局时按 left_view_mode 挂上或摘下。
    left_view: id,
    left_view_mode: UITextFieldViewMode,
    border_style: UITextBorderStyle,
    /// [2026-09-16] C-06:只存值,清除按钮暂不绘制。
    clear_button_mode: UITextFieldViewMode,
    secure: bool,
    content_vertical_alignment: UIControlContentVerticalAlignment,
}
impl_HostObject_with_superclass!(UITextFieldHostObject);
impl Default for UITextFieldHostObject {
    fn default() -> Self {
        UITextFieldHostObject {
            superclass: Default::default(),
            delegate: nil,
            editing: false,
            text_label: nil,
            text: nil,
            placeholder_label: nil,
            background: nil,
            background_view: nil,
            left_view: nil,
            left_view_mode: UITextFieldViewModeNever,
            border_style: UITextBorderStyleNone,
            clear_button_mode: UITextFieldViewModeNever,
            secure: false,
            content_vertical_alignment: UIControlContentVerticalAlignmentCenter,
        }
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UITextField: UIControl

// TODO: clear button rendering, more properties
// TODO: notifications

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UITextFieldHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithFrame:(CGRect)frame {
    let this: id = msg_super![env; this initWithFrame:frame];

    // [2026-09-16] C-06 ①:iOS 的 UITextField 默认 backgroundColor 为 nil、borderStyle 为 None,即透明、
    // 不画框。原来这里把输入框自身和内部 text_label 都设成不透明白底:游戏把输入框设成 clearColor 后
    // label 仍盖着一块白矩形,兑换码/邀请码框的美术底图和灰色提示都被挡住。白框改由 drawRect: 在
    // borderStyle 为 RoundedRect 时画出(必须同批落地:改名框、注册页、海底寻宝米米号都靠这个样式显示白框)。
    // 不透明必须关掉:合成器对 opaque 且没有背景色的图层关闭混合,透明位图会画成黑块。
    () = msg![env; this setOpaque:false];
    init_subviews(env, this);

    this
}

- (id)initWithCoder:(id)coder {
    let this: id = msg_super![env; this initWithCoder: coder];

    // TODO: actual decoding of properties

    init_subviews(env, this);

    this
}

- (())dealloc {
    // [2026-09-16] C-06 复审:leftView 是游戏传进来的对象。下面的 mem::take 会连同内嵌的 UIViewHostObject
    // 一起清空(subviews 表丢失,UIView dealloc 不再摘子视图),leftView 的 superview 就一直指着已释放的
    // 本输入框;游戏之后若把它挂到别处,removeFromSuperview 会去读这块已释放的对象。所以先在宿主对象
    // 完好时把它摘下。内部建的 label/底图不外露,沿用原来的写法。
    let left_view = env.objc.borrow::<UITextFieldHostObject>(this).left_view;
    if left_view != nil {
        let left_superview: id = msg![env; left_view superview];
        if left_superview == this {
            () = msg![env; left_view removeFromSuperview];
        }
    }

    let UITextFieldHostObject {
        text_label,
        text,
        placeholder_label,
        background,
        background_view,
        left_view,
        ..
    } = std::mem::take(env.objc.borrow_mut(this));

    release(env, text_label);
    release(env, text);
    release(env, placeholder_label);
    release(env, background);
    release(env, background_view);
    release(env, left_view);
    msg_super![env; this dealloc]
}

- (())layoutSubviews {
    layout_text_field(env, this);
}

// [2026-09-16] C-06:iOS 在视图尺寸变化时会重新布局子视图,touchHLE 的 UIView setFrame:/setBounds: 不打
// 布局脏标记。这里补上,让正文、占位符、底图、leftView 跟着输入框尺寸走;真正的布局在合成前统一进行
// (setNeedsLayout 只打标记,不同步跑 layoutSubviews)。
- (())setFrame:(CGRect)frame {
    () = msg_super![env; this setFrame:frame];
    () = msg![env; this setNeedsLayout];
}
- (())setBounds:(CGRect)bounds {
    () = msg_super![env; this setBounds:bounds];
    () = msg![env; this setNeedsLayout];
}

- (id)text {
    let text = env.objc.borrow::<UITextFieldHostObject>(this).text;
    // iOS 保证 UITextField.text 永不为 nil(未设值默认 @"")。原文未设时为 nil,
    // 若原样透出,调用方 strlen([textField.text UTF8String]) → strlen(NULL) → MemoryError 崩
    // (同 UITextView 的 -[GiftAndMessageLayer displayUI] 崩因)。未设值回空串,与 iOS 一致。
    if text == nil {
        ns_string::get_static_str(env, "")
    } else {
        text
    }
}
- (())setText:(id)text { // NSString*
    store_text(env, this, text);

    // This will work only if all the text changes will call setText:!
    // This is the case right now.
    // (see `handle_text` and `handle_backspace` helper functions below)
    // TODO: actually check if setText: send this notif on each change
    // (e.g. does it send the notif if text hasn't changed)
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let name = ns_string::get_static_str(env, UITextFieldTextDidChangeNotification);
    // TODO: userInfo
    let _: () = msg![env; center postNotificationName:name object:this userInfo:nil];
}

- (())setTextColor:(id)color { // UIColor*
    let text_label = env.objc.borrow_mut::<UITextFieldHostObject>(this).text_label;
    msg![env; text_label setTextColor:color]
}

- (())setTextAlignment:(UITextAlignment)text_alignment {
    let &UITextFieldHostObject { text_label, placeholder_label, .. } = env.objc.borrow(this);
    () = msg![env; text_label setTextAlignment:text_alignment];
    () = msg![env; placeholder_label setTextAlignment:text_alignment];
}

- (())setFont:(id)new_font { // UIFont*
    // [2026-09-16] C-06 ④⑤:占位符与正文同字体;字体行高影响贴顶/贴底排版,需要重新布局。
    let &UITextFieldHostObject { text_label, placeholder_label, .. } = env.objc.borrow(this);
    () = msg![env; text_label setFont:new_font];
    () = msg![env; placeholder_label setFont:new_font];
    () = msg![env; this setNeedsLayout];
}

- (())setMinimumFontSize:(CGFloat)size {
    let text_label = env.objc.borrow_mut::<UITextFieldHostObject>(this).text_label;
    () = msg![env; text_label setMinimumFontSize:size];
}

- (())setClearsOnBeginEditing:(bool)clear {
    todo_objc_setter!(this, clear);
}

// [2026-09-16] C-06:只存值。清除按钮(编辑中右侧的圆形 ×)暂不绘制,也不响应点按。
- (())setClearButtonMode:(UITextFieldViewMode)mode {
    env.objc.borrow_mut::<UITextFieldHostObject>(this).clear_button_mode = mode;
}
- (UITextFieldViewMode)clearButtonMode {
    env.objc.borrow::<UITextFieldHostObject>(this).clear_button_mode
}

// [2026-09-16] C-06 ⑦:账号菜单的改密码/设密码/换号/申请淘米号/改邮箱视图(TMAPasswordModifyView 等)设
// secureTextEntry,并在 textFieldShouldBeginEditing: / shouldChangeCharacters 里读 isSecureTextEntry。
// 只在显示层换成圆点,原文仍存在宿主对象里。
- (())setSecureTextEntry:(bool)secure {
    env.objc.borrow_mut::<UITextFieldHostObject>(this).secure = secure;
    refresh_display(env, this);
}
- (bool)isSecureTextEntry {
    env.objc.borrow::<UITextFieldHostObject>(this).secure
}

// [2026-09-16] C-06 ④:兑换码/邀请码/海底寻宝米米号输入框的灰色提示。几个游戏委托在
// textFieldShouldBeginEditing: 里 setPlaceholder:nil、在 textFieldShouldEndEditing: 里设回,点击后提示消失
// 靠的是游戏自己这段逻辑;这里只按 iOS 规则"原文为空且有占位符才显示"。
- (id)placeholder {
    let placeholder_label = env.objc.borrow::<UITextFieldHostObject>(this).placeholder_label;
    msg![env; placeholder_label text]
}
- (())setPlaceholder:(id)placeholder { // NSString*
    let placeholder_label = env.objc.borrow::<UITextFieldHostObject>(this).placeholder_label;
    () = msg![env; placeholder_label setText:placeholder];
    refresh_display(env, this);
}

- (())setPosition:(CGPoint)position {
    todo_objc_setter!(this, position);
}

// weak/non-retaining
- (())setDelegate:(id)delegate { // something implementing UITextFieldDelegate
    log_dbg!("setDelegate:{:?}", delegate);
    let host_object = env.objc.borrow_mut::<UITextFieldHostObject>(this);
    host_object.delegate = delegate;
}
- (id)delegate {
    env.objc.borrow::<UITextFieldHostObject>(this).delegate
}

// UITextInputTraits implementation
- (())setAutocapitalizationType:(UITextAutocapitalizationType)type_ {
    todo_objc_setter!(this, type_);
}
- (())setAutocorrectionType:(UITextAutocorrectionType)type_ {
    todo_objc_setter!(this, type_);
}
- (())setReturnKeyType:(UIReturnKeyType)type_ {
    todo_objc_setter!(this, type_);
}
- (())setKeyboardAppearance:(UIKeyboardAppearance)appearance {
    todo_objc_setter!(this, appearance);
}
- (())setKeyboardType:(UIKeyboardType)type_ {
    todo_objc_setter!(this, type_);
}
- (())setEnablesReturnKeyAutomatically:(bool)enables {
    todo_objc_setter!(this, enables);
}

// [2026-09-16] C-06 ⑥:游戏里 setBorderStyle: 的取值只有两种(反汇编 movs r2):3 = RoundedRect
// (-[AvatarLayer showTextField] 改名框、-[RegisterView init]、-[InviteFriendsLayer init]、
// -[SeabedSeekingTreasureMainLayer showMimiNumberTextField]、-[MAlertView addTextField:placeHolder:]),
// 0 = None(-[VerifyInviteCodeLayer showRequestTextField]、-[ActionCodeLayer showActionTextField],配底图)。
- (())setBorderStyle:(UITextBorderStyle)style {
    env.objc.borrow_mut::<UITextFieldHostObject>(this).border_style = style;
    () = msg![env; this setNeedsLayout];
    () = msg![env; this setNeedsDisplay];
}
- (UITextBorderStyle)borderStyle {
    env.objc.borrow::<UITextFieldHostObject>(this).border_style
}

// [2026-09-16] C-06 ②③⑤:邀请码/兑换码输入框(-[VerifyInviteCodeLayer showRequestTextField]
// @0x37a4e4~0x37a552、-[ActionCodeLayer showActionTextField]@0x3c7022~)依次设 background
// (request_back_input.png / action_code_edit_back.png 输入框底图)、leftView(5×20 的留白 UIView)、
// leftViewMode 3(Always)、contentVerticalAlignment 0(居中)。此前这 4 个只是空 setter,底图不显示、
// 文字贴着左边。
- (())setBackground:(id)image { // UIImage*
    retain(env, image);
    let host_obj = env.objc.borrow_mut::<UITextFieldHostObject>(this);
    let old_image = std::mem::replace(&mut host_obj.background, image);
    let background_view = host_obj.background_view;
    release(env, old_image);

    if image == nil {
        if background_view != nil {
            env.objc.borrow_mut::<UITextFieldHostObject>(this).background_view = nil;
            () = msg![env; background_view removeFromSuperview];
            release(env, background_view);
        }
    } else if background_view == nil {
        let new_view: id = msg_class![env; UIImageView alloc];
        let new_view: id = msg![env; new_view initWithImage:image];
        env.objc.borrow_mut::<UITextFieldHostObject>(this).background_view = new_view;
        // 垫在最底层:正文、占位符、leftView 都在它上面。
        let index: NSInteger = 0;
        () = msg![env; this insertSubview:new_view atIndex:index];
    } else {
        () = msg![env; background_view setImage:image];
    }

    () = msg![env; this setNeedsLayout];
    // 有无底图决定 Line/Bezel 边框画不画。
    () = msg![env; this setNeedsDisplay];
}
- (id)background {
    env.objc.borrow::<UITextFieldHostObject>(this).background
}

- (())setLeftView:(id)view { // UIView*
    retain(env, view);
    let old_view = std::mem::replace(
        &mut env.objc.borrow_mut::<UITextFieldHostObject>(this).left_view,
        view
    );
    if old_view != nil && old_view != view {
        let old_superview: id = msg![env; old_view superview];
        if old_superview == this {
            () = msg![env; old_view removeFromSuperview];
        }
    }
    release(env, old_view);
    () = msg![env; this setNeedsLayout];
}
- (id)leftView {
    env.objc.borrow::<UITextFieldHostObject>(this).left_view
}

- (())setLeftViewMode:(UITextFieldViewMode)mode {
    env.objc.borrow_mut::<UITextFieldHostObject>(this).left_view_mode = mode;
    () = msg![env; this setNeedsLayout];
}
- (UITextFieldViewMode)leftViewMode {
    env.objc.borrow::<UITextFieldHostObject>(this).left_view_mode
}

- (())setContentVerticalAlignment:(UIControlContentVerticalAlignment)alignment {
    env.objc.borrow_mut::<UITextFieldHostObject>(this).content_vertical_alignment = alignment;
    () = msg![env; this setNeedsLayout];
}
- (UIControlContentVerticalAlignment)contentVerticalAlignment {
    env.objc.borrow::<UITextFieldHostObject>(this).content_vertical_alignment
}

- (())drawRect:(CGRect)rect {
    // [2026-09-16] C-06 复审:用 drawLayer:inContext: 传进来的矩形(原点已归零,与图层位图坐标一致),
    // 不读 [self bounds]:bounds 原点非零时边框会画偏出位图。
    let &UITextFieldHostObject { border_style, background, .. } = env.objc.borrow(this);
    draw_border(env, rect, border_style, background != nil);
}

- (())touchesBegan:(id)_touches // NSSet* of UITouch*
         withEvent:(id)_event { // UIEvent*
    let _: bool = msg![env; this becomeFirstResponder];
}

- (bool)isEditing {
    env.objc.borrow::<UITextFieldHostObject>(this).editing
}

- (bool)becomeFirstResponder {
    if env.objc.borrow::<UITextFieldHostObject>(this).editing {
        return true;
    }

    let delegate: id = env.objc.borrow::<UITextFieldHostObject>(this).delegate;
    let sel: SEL = env.objc.register_host_selector("textFieldShouldBeginEditing:".to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if delegate != nil && responds && !msg![env; delegate textFieldShouldBeginEditing:this] {
        return false;
    }

    // If text is nil, it becomes an empty string
    // on becoming the first responder.
    // This behaviour was validated on the Aspen Simulator
    if env.objc.borrow::<UITextFieldHostObject>(this).text == nil {
        let empty = ns_string::get_static_str(env, "");
        store_text(env, this, empty);
    }

    // [2026-09-16] C-02:键盘通知带上 userInfo(原来传 nil)。观察者读 FrameEnd 等键取矩形,
    // 见 new_keyboard_notification_user_info。
    let user_info = new_keyboard_notification_user_info(env);
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let name = ns_string::get_static_str(env, UIKeyboardWillShowNotification);
    let _: () = msg![env; center postNotificationName:name object:this userInfo:user_info];

    env.framework_state.uikit.ui_responder.first_responder = this;
    env.on_parent_stack_in_coroutine(|window, _| window.start_text_input());

    let name = ns_string::get_static_str(env, UIKeyboardDidShowNotification);
    let _: () = msg![env; center postNotificationName:name object:this userInfo:user_info];
    release(env, user_info);

    // TODO: is it the right spot?
    env.objc.borrow_mut::<UITextFieldHostObject>(this).editing = true;
    // [2026-09-16] C-06 ③:leftViewMode 为 WhileEditing / UnlessEditing 时要重新挂或摘 leftView。
    () = msg![env; this setNeedsLayout];

    let sel: SEL = env.objc.register_host_selector("textFieldDidBeginEditing:".to_string(), &mut env.mem);
    if msg![env; delegate respondsToSelector:sel] {
        () = msg![env; delegate textFieldDidBeginEditing:this];
    }

    true
}

- (bool)resignFirstResponder {
    log_dbg!("resignFirstResponder");

    if !env.objc.borrow::<UITextFieldHostObject>(this).editing {
        return true;
    }

    let delegate: id = env.objc.borrow::<UITextFieldHostObject>(this).delegate;
    let sel: SEL = env.objc.register_host_selector("textFieldShouldEndEditing:".to_string(), &mut env.mem);
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    if delegate != nil && responds && !msg![env; delegate textFieldShouldEndEditing:this] {
        return false;
    }

    // [2026-09-16] C-02:同 becomeFirstResponder,收起键盘的通知也带 userInfo
    // (-[RegisterView keyboardWasHidden:] 同样读 FrameEnd 键)。
    let user_info = new_keyboard_notification_user_info(env);
    let center: id = msg_class![env; NSNotificationCenter defaultCenter];
    let name = ns_string::get_static_str(env, UIKeyboardWillHideNotification);
    let _: () = msg![env; center postNotificationName:name object:this userInfo:user_info];

    env.framework_state.uikit.ui_responder.first_responder = nil;
    env.on_parent_stack_in_coroutine(|window, _| window.stop_text_input());

    let name = ns_string::get_static_str(env, UIKeyboardDidHideNotification);
    let _: () = msg![env; center postNotificationName:name object:this userInfo:user_info];
    release(env, user_info);

    // TODO: is it the right spot?
    env.objc.borrow_mut::<UITextFieldHostObject>(this).editing = false;
    // [2026-09-16] C-06 ③:同 becomeFirstResponder。
    () = msg![env; this setNeedsLayout];

    let sel: SEL = env.objc.register_host_selector("textFieldDidEndEditing:".to_string(), &mut env.mem);
    if msg![env; delegate respondsToSelector:sel] {
        () = msg![env; delegate textFieldDidEndEditing:this];
    }

    true
}

@end

};

/// [2026-09-16] C-06 ①④:建内部显示层(正文、占位符),initWithFrame: 与 initWithCoder: 共用。
/// 两个 label 都是透明底(UILabel 的 setBackgroundColor:nil 会回落成白色,所以显式给 clearColor)。
fn init_subviews(env: &mut Environment, this: id) {
    let clear_color: id = msg_class![env; UIColor clearColor];

    let text_label: id = msg_class![env; UILabel new];
    () = msg![env; text_label setBackgroundColor:clear_color];
    () = msg![env; text_label setTextAlignment:UITextAlignmentLeft];
    let text_color: id = msg_class![env; UIColor blackColor];
    () = msg![env; text_label setTextColor:text_color];

    // iOS 占位符是 70% 灰。
    let placeholder_label: id = msg_class![env; UILabel new];
    () = msg![env; placeholder_label setBackgroundColor:clear_color];
    () = msg![env; placeholder_label setTextAlignment:UITextAlignmentLeft];
    let white: CGFloat = 0.7;
    let alpha: CGFloat = 1.0;
    let placeholder_color: id = msg_class![env; UIColor colorWithWhite:white alpha:alpha];
    () = msg![env; placeholder_label setTextColor:placeholder_color];
    () = msg![env; placeholder_label setHidden:true];

    let host_obj = env.objc.borrow_mut::<UITextFieldHostObject>(this);
    host_obj.text_label = text_label;
    host_obj.placeholder_label = placeholder_label;

    // 两个 label 同一时刻只显示一个,正文在上。
    () = msg![env; this addSubview:placeholder_label];
    () = msg![env; this addSubview:text_label];
    // 没有这个标记的话,运行期新建的输入框永远不会跑 layoutSubviews(UIView 不会自动打标记),
    // text_label 的 frame 一直是 0。
    () = msg![env; this setNeedsLayout];
}

/// [2026-09-16] C-06 ⑦:换原文(copy 持有)并刷新显示层。不发 UITextFieldTextDidChangeNotification,
/// 由调用方决定(setText: 发,键盘输入路径沿用原来的不发)。
fn store_text(env: &mut Environment, text_field: id, new_text: id) {
    let new_text: id = msg![env; new_text copy];
    let old_text = std::mem::replace(
        &mut env.objc.borrow_mut::<UITextFieldHostObject>(text_field).text,
        new_text,
    );
    release(env, old_text);
    refresh_display(env, text_field);
}

/// [2026-09-16] C-06 ④⑦:按原文刷新显示层:secureTextEntry 时 text_label 显示同长度的「•」;
/// 原文为空且设了占位符时显示 placeholder_label。
fn refresh_display(env: &mut Environment, text_field: id) {
    let &UITextFieldHostObject {
        text,
        text_label,
        placeholder_label,
        secure,
        ..
    } = env.objc.borrow(text_field);

    let text_len: NSUInteger = if text == nil {
        0
    } else {
        msg![env; text length]
    };
    if secure && text_len > 0 {
        // 「•」是 1 个 UTF-16 码元,圆点个数与原文 length 一致。
        let dots = ns_string::from_rust_string(env, "•".repeat(text_len as usize));
        () = msg![env; text_label setText:dots];
        release(env, dots);
    } else {
        () = msg![env; text_label setText:text];
    }

    let placeholder: id = msg![env; placeholder_label text];
    let placeholder_len: NSUInteger = if placeholder == nil {
        0
    } else {
        msg![env; placeholder length]
    };
    () = msg![env; placeholder_label setHidden:(text_len > 0 || placeholder_len == 0)];
}

/// [2026-09-16] C-06 ②③⑤⑥:统一排版(layoutSubviews 调用,运行在合成前的布局阶段,不在游戏帧栈上)。
/// - 底图铺满输入框;RoundedRect 样式下 iOS 忽略 background(自己画白框),底图隐藏;
/// - leftView 按 mode 挂上或摘下,放左侧并竖直居中,正文右移让开;
/// - 正文和占位符共用一个文字区:水平去掉边框内边距与 leftView,竖直按 contentVerticalAlignment;
/// - 新增子视图都挂在输入框内部,UI43 宽屏右移输入框时跟着一起移。
fn layout_text_field(env: &mut Environment, this: id) {
    let bounds: CGRect = msg![env; this bounds];
    let &UITextFieldHostObject {
        editing,
        text_label,
        placeholder_label,
        background_view,
        left_view,
        left_view_mode,
        border_style,
        content_vertical_alignment,
        ..
    } = env.objc.borrow(this);

    if background_view != nil {
        () = msg![env; background_view setFrame:bounds];
        () = msg![env; background_view setHidden:(border_style == UITextBorderStyleRoundedRect)];
    }

    // 边框占掉的水平内边距(近似 iOS 的 textRectForBounds:)。
    let border_inset: CGFloat = match border_style {
        UITextBorderStyleLine | UITextBorderStyleBezel => 2.0,
        UITextBorderStyleRoundedRect => 7.0,
        _ => 0.0,
    };
    let mut text_x = bounds.origin.x + border_inset;

    if left_view != nil {
        let visible = match left_view_mode {
            UITextFieldViewModeWhileEditing => editing,
            UITextFieldViewModeUnlessEditing => !editing,
            UITextFieldViewModeAlways => true,
            UITextFieldViewModeNever => false,
            _ => false,
        };
        let left_superview: id = msg![env; left_view superview];
        if visible {
            if left_superview != this {
                () = msg![env; this addSubview:left_view];
            }
            let left_frame: CGRect = msg![env; left_view frame];
            let new_left_frame = CGRect {
                origin: CGPoint {
                    x: bounds.origin.x,
                    y: bounds.origin.y + (bounds.size.height - left_frame.size.height) / 2.0,
                },
                size: left_frame.size,
            };
            () = msg![env; left_view setFrame:new_left_frame];
            text_x = text_x.max(bounds.origin.x + left_frame.size.width);
        } else if left_superview == this {
            () = msg![env; left_view removeFromSuperview];
        }
    }
    let text_right = bounds.origin.x + bounds.size.width - border_inset;
    let text_width = (text_right - text_x).max(0.0);

    // UILabel 总在自己的 frame 里竖直居中,所以贴顶/贴底时把 label 高度收成一行。
    let (text_y, text_height) = match content_vertical_alignment {
        UIControlContentVerticalAlignmentTop | UIControlContentVerticalAlignmentBottom => {
            let font: id = msg![env; text_label font];
            let line_height: CGFloat = if font == nil {
                bounds.size.height
            } else {
                msg![env; font lineHeight]
            };
            let line_height = line_height.min(bounds.size.height);
            if content_vertical_alignment == UIControlContentVerticalAlignmentTop {
                (bounds.origin.y, line_height)
            } else {
                (bounds.origin.y + bounds.size.height - line_height, line_height)
            }
        }
        UIControlContentVerticalAlignmentCenter | UIControlContentVerticalAlignmentFill => {
            (bounds.origin.y, bounds.size.height)
        }
        _ => (bounds.origin.y, bounds.size.height),
    };
    let text_frame = CGRect {
        origin: CGPoint { x: text_x, y: text_y },
        size: CGSize {
            width: text_width,
            height: text_height,
        },
    };
    () = msg![env; text_label setFrame:text_frame];
    () = msg![env; placeholder_label setFrame:text_frame];
    // 图层位图按 bounds 建,尺寸变了不会自动重画(needsDisplayOnBoundsChange 默认关),这里显式重画,
    // 免得旧位图被拉伸。布局只在打过标记时才跑,不是每帧。
    () = msg![env; text_label setNeedsDisplay];
    () = msg![env; placeholder_label setNeedsDisplay];
    if border_style != UITextBorderStyleNone {
        () = msg![env; this setNeedsDisplay];
    }
}

/// [2026-09-16] C-06 ⑥:borderStyle 的系统外观近似(位图上下文只有矩形填充,边线 1pt)。
/// - RoundedRect:灰色 1pt 圆角边 + 白底(iOS 这个样式自带白底、忽略 background);本游戏原来的"白框"
///   外观(改名框、注册页、邀请好友、海底寻宝米米号、MAlertView 输入框)由这里接手。
/// - Line / Bezel:没设 background 时画 1pt 边线,内部保持透明;设了底图时 iOS 用底图代替边框,这里不画。
///   本游戏没有用到这两种。
/// - None:不画。
fn draw_border(
    env: &mut Environment,
    bounds: CGRect,
    border_style: UITextBorderStyle,
    has_background: bool,
) {
    let inner = CGRect {
        origin: CGPoint {
            x: bounds.origin.x + 1.0,
            y: bounds.origin.y + 1.0,
        },
        size: CGSize {
            width: (bounds.size.width - 2.0).max(0.0),
            height: (bounds.size.height - 2.0).max(0.0),
        },
    };
    match border_style {
        UITextBorderStyleRoundedRect => {
            let context = UIGraphicsGetCurrentContext(env);
            CGContextSetRGBFillColor(env, context, 0.6, 0.6, 0.6, 1.0);
            fill_rounded_rect(env, context, bounds, 6.0);
            CGContextSetRGBFillColor(env, context, 1.0, 1.0, 1.0, 1.0);
            fill_rounded_rect(env, context, inner, 5.0);
        }
        UITextBorderStyleLine | UITextBorderStyleBezel if !has_background => {
            let context = UIGraphicsGetCurrentContext(env);
            let gray: CGFloat = if border_style == UITextBorderStyleLine {
                0.2
            } else {
                0.5
            };
            CGContextSetRGBFillColor(env, context, gray, gray, gray, 1.0);
            CGContextFillRect(env, context, bounds);
            CGContextClearRect(env, context, inner);
        }
        _ => {}
    }
}

/// [2026-09-16] C-06 ⑥:填圆角矩形。位图上下文没有路径填充,四角逐行用水平条带逼近圆弧(无抗锯齿)。
fn fill_rounded_rect(env: &mut Environment, context: CGContextRef, rect: CGRect, radius: CGFloat) {
    let radius = radius
        .min(rect.size.width / 2.0)
        .min(rect.size.height / 2.0)
        .max(0.0);
    let middle = CGRect {
        origin: CGPoint {
            x: rect.origin.x,
            y: rect.origin.y + radius,
        },
        size: CGSize {
            width: rect.size.width,
            height: rect.size.height - 2.0 * radius,
        },
    };
    if middle.size.width > 0.0 && middle.size.height > 0.0 {
        CGContextFillRect(env, context, middle);
    }
    let rows = radius.ceil() as u32;
    for row in 0..rows {
        let row_top = row as CGFloat;
        let row_height = (radius - row_top).min(1.0);
        // 这一行的中线到圆心的竖直距离 → 圆弧在这一行向内缩进多少。
        let dy = radius - (row_top + row_height / 2.0);
        let dx = radius - (radius * radius - dy * dy).max(0.0).sqrt();
        let width = rect.size.width - 2.0 * dx;
        if width <= 0.0 || row_height <= 0.0 {
            continue;
        }
        let top_row = CGRect {
            origin: CGPoint {
                x: rect.origin.x + dx,
                y: rect.origin.y + row_top,
            },
            size: CGSize {
                width,
                height: row_height,
            },
        };
        let bottom_row = CGRect {
            origin: CGPoint {
                x: rect.origin.x + dx,
                y: rect.origin.y + rect.size.height - row_top - row_height,
            },
            size: CGSize {
                width,
                height: row_height,
            },
        };
        CGContextFillRect(env, context, top_row);
        CGContextFillRect(env, context, bottom_row);
    }
}

/// [2026-09-16] C-02:键盘通知的 userInfo。返回 +1 的 NSMutableDictionary,调用方发完通知后 release
/// (NSNotification 自己 copy 一份持有)。
///
/// 游戏订阅键盘通知的观察者:-[RegisterView keyboardWasShown:/keyboardWasHidden:](在线新号注册页)、
/// LeaveMessageLayer / GiftAndMessageLayer / CrowPriestMessageLayer 读 FrameEnd 的矩形;
/// TMALoginViewController 读 Bounds。它们都不依赖具体数值:RegisterView 的 moveUp:size:@0x191db8 不用 size
/// (按 isIpad 固定上移 90/180),另外三个只在 !isIpad 时记下尺寸。桌面/模拟器没有真实软键盘,
/// 矩形和中心点一律给 0,动画时长 0、曲线 0(EaseInOut),不改变任何布局。
fn new_keyboard_notification_user_info(env: &mut Environment) -> id {
    let zero_rect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 0.0,
            height: 0.0,
        },
    };
    let zero_point = CGPoint { x: 0.0, y: 0.0 };
    let rect_value: id = msg_class![env; NSValue valueWithCGRect:zero_rect];
    let point_value: id = msg_class![env; NSValue valueWithCGPoint:zero_point];
    let duration: f64 = 0.0;
    let duration_value: id = msg_class![env; NSNumber numberWithDouble:duration];
    let curve: NSInteger = 0;
    let curve_value: id = msg_class![env; NSNumber numberWithInteger:curve];

    let user_info: id = msg_class![env; NSMutableDictionary new];
    let entries: [(&'static str, id); 7] = [
        (UIKeyboardFrameBeginUserInfoKey, rect_value),
        (UIKeyboardFrameEndUserInfoKey, rect_value),
        (UIKeyboardBoundsUserInfoKey, rect_value),
        (UIKeyboardCenterBeginUserInfoKey, point_value),
        (UIKeyboardCenterEndUserInfoKey, point_value),
        (UIKeyboardAnimationDurationUserInfoKey, duration_value),
        (UIKeyboardAnimationCurveUserInfoKey, curve_value),
    ];
    for &(key, value) in entries.iter() {
        let key: id = ns_string::get_static_str(env, key);
        () = msg![env; user_info setObject:value forKey:key];
    }
    user_info
}

pub fn handle_text(env: &mut Environment, text_field: id, text: String) {
    log_dbg!("Calling handle_text for {:?} with '{}'", text_field, text);
    let txt = ns_string::from_rust_string(env, text);
    let txt_len: NSUInteger = msg![env; txt length];
    // [MoleWorld] 原 assert_eq!(txt_len, 1) 假设每次文本输入恰好 1 个 UTF-16 码元,但:
    //  · 中文等输入法一次会提交多字(如"摩尔" = 2 码元);
    //  · emoji / 补充平面字符是代理对(2 码元);
    //  · 粘贴是任意长度。
    // → 改庄园名(尤其中文)时 txt_len≠1,断言失败 → 全平台稳定闪退。
    // 改为接受任意长度:空串直接跳过,其余整串作为替换文本插入(下方本就按整串
    // stringByAppendingString: 处理,只有这个断言写死成 1)。
    if txt_len == 0 {
        release(env, txt);
        return;
    }

    // [2026-09-16] C-06 ⑦:读写宿主对象里的原文(secureTextEntry 时 text_label 里只有圆点)。
    let mut curr_text: id = env
        .objc
        .borrow::<UITextFieldHostObject>(text_field)
        .text;
    if curr_text == nil {
        curr_text = ns_string::get_static_str(env, "");
    }
    // 委托回调里可能 setText: 换掉原文(旧串随之被释放),先持有一份再往下拼接。
    retain(env, curr_text);
    log_dbg!(
        "handle_text, curr_text: {}",
        ns_string::to_rust_string(env, curr_text)
    );

    let len = msg![env; curr_text length];
    let range = NSRange {
        location: len,
        length: 0,
    };

    let delegate: id = env
        .objc
        .borrow::<UITextFieldHostObject>(text_field)
        .delegate;
    let sel: SEL = env.objc.register_host_selector(
        "textField:shouldChangeCharactersInRange:replacementString:".to_string(),
        &mut env.mem,
    );
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    let should = delegate == nil
        || !responds
        || msg![env; delegate textField:text_field shouldChangeCharactersInRange:range replacementString:txt];
    if should {
        let new_text: id = msg![env; curr_text stringByAppendingString:txt];
        log_dbg!(
            "handle_text, new_text: {}",
            ns_string::to_rust_string(env, new_text)
        );
        // TODO: refactor this to proper update() method
        store_text(env, text_field, new_text);
        () = msg![env; text_field setNeedsDisplay];
        release(env, new_text);
    }
    release(env, curr_text);
    release(env, txt);
}

pub fn handle_backspace(env: &mut Environment, text_field: id) {
    log_dbg!("Calling handle_backspace for {:?}", text_field);
    // [2026-09-16] C-06 ⑦:同 handle_text,操作宿主对象里的原文。
    let curr_text: id = env
        .objc
        .borrow::<UITextFieldHostObject>(text_field)
        .text;

    let len: NSUInteger = msg![env; curr_text length];
    if len == 0 {
        return;
    }
    // 同 handle_text:委托回调可能换掉原文,先持有。
    retain(env, curr_text);
    let range = NSRange {
        location: len - 1,
        length: 1,
    };
    let empty = ns_string::get_static_str(env, "");

    let delegate: id = env
        .objc
        .borrow::<UITextFieldHostObject>(text_field)
        .delegate;
    let sel: SEL = env.objc.register_host_selector(
        "textField:shouldChangeCharactersInRange:replacementString:".to_string(),
        &mut env.mem,
    );
    let responds: bool = msg![env; delegate respondsToSelector:sel];
    let should = delegate == nil
        || !responds
        || msg![env; delegate textField:text_field shouldChangeCharactersInRange:range replacementString:empty];
    if should {
        let new_text: id = msg![env; curr_text substringToIndex:(len-1)];
        log_dbg!(
            "handle_backspace, new_text: {}",
            ns_string::to_rust_string(env, new_text)
        );
        // TODO: refactor this to proper update() method
        store_text(env, text_field, new_text);
        () = msg![env; text_field setNeedsDisplay];
        release(env, new_text);
    }
    release(env, curr_text);
}

pub fn handle_return(env: &mut Environment, text_field: id) {
    log_dbg!("Calling handle_return for {:?}", text_field);
    let delegate: id = env
        .objc
        .borrow::<UITextFieldHostObject>(text_field)
        .delegate;
    let sel: SEL = env
        .objc
        .register_host_selector("textFieldShouldReturn:".to_string(), &mut env.mem);
    if msg![env; delegate respondsToSelector:sel] {
        log_dbg!("handle_return");
        () = msg![env; delegate textFieldShouldReturn:text_field];
    }
}
