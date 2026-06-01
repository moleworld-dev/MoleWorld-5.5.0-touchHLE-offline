/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `UIAlertView`.
//!
//! touchHLE has no real alert UI, but many apps (MoleWorld included) put a modal
//! `UIAlertView` in front of everything — typically an offline/"can't reach
//! server" notice — and only continue once the user taps a button, via the
//! delegate callbacks `alertView:clickedButtonAtIndex:` and
//! `alertView:didDismissWithButtonIndex:`. If we never deliver those, the game
//! sits behind an invisible modal forever (its menu buttons are never shown).
//!
//! So we implement a minimal, headless UIAlertView: remember the delegate and
//! the button count, and when `show` is called, immediately auto-dismiss by
//! firing the cancel button (index 0) callbacks. That unblocks the game's flow
//! exactly as if the user had tapped the default button.

use crate::frameworks::foundation::ns_string;
use crate::frameworks::foundation::NSInteger;
use crate::impl_HostObject_with_superclass;
use crate::objc::{
    id, msg, msg_super, nil, objc_classes, release, retain, ClassExports, NSZonePtr,
};
use std::borrow::Cow;

pub(crate) struct UIAlertViewHostObject {
    superclass: super::UIViewHostObject,
    /// Not retained (delegates are weak in UIKit).
    delegate: id,
    /// `NSString*`, retained. Delegates (e.g. MoleWorld's offline-notice handler)
    /// read these back via -title / -message, so we must actually keep them.
    title: id,
    message: id,
    /// Number of buttons added (cancel + others). At least 1 once shown.
    num_buttons: NSInteger,
}
impl_HostObject_with_superclass!(UIAlertViewHostObject);
impl Default for UIAlertViewHostObject {
    fn default() -> Self {
        UIAlertViewHostObject {
            superclass: Default::default(),
            delegate: nil,
            title: nil,
            message: nil,
            num_buttons: 0,
        }
    }
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation UIAlertView: UIView

+ (id)allocWithZone:(NSZonePtr)_zone {
    let host_object = Box::<UIAlertViewHostObject>::default();
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

- (id)initWithTitle:(id)title
                      message:(id)message
                     delegate:(id)delegate
            cancelButtonTitle:(id)cancelButtonTitle
            otherButtonTitles:(id)otherButtonTitles {
    let msg_s = if message == nil { Cow::from("(nil)") } else { ns_string::to_rust_string(env, message) };
    let title_s = if title == nil { Cow::from("(nil)") } else { ns_string::to_rust_string(env, title) };
    log!("UIAlertView init: title {:?}, message {:?} (delegate {:?})", title_s, msg_s, delegate);

    let this: id = msg_super![env; this init];

    let mut num_buttons: NSInteger = 0;
    if cancelButtonTitle != nil {
        num_buttons += 1;
    }
    // `otherButtonTitles` is a nil-terminated vararg list; the game usually passes
    // just one or nil. We can't easily walk the varargs here, but the count only
    // needs to be >=1 for the cancel-button dismiss path, so this is enough.
    if otherButtonTitles != nil {
        num_buttons += 1;
    }
    if num_buttons == 0 {
        num_buttons = 1;
    }

    let title_copy: id = if title != nil { retain(env, title); title } else { nil };
    let message_copy: id = if message != nil { retain(env, message); message } else { nil };
    let host = env.objc.borrow_mut::<UIAlertViewHostObject>(this);
    host.delegate = delegate;
    host.title = title_copy;
    host.message = message_copy;
    host.num_buttons = num_buttons;
    this
}

- (())setDelegate:(id)delegate {
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).delegate = delegate;
}
- (id)delegate {
    env.objc.borrow::<UIAlertViewHostObject>(this).delegate
}
- (id)title {
    env.objc.borrow::<UIAlertViewHostObject>(this).title
}
- (())setTitle:(id)title {
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIAlertViewHostObject>(this).title, nil);
    let new: id = if title != nil { retain(env, title); title } else { nil };
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).title = new;
    if old != nil { release(env, old); }
}
- (id)message {
    env.objc.borrow::<UIAlertViewHostObject>(this).message
}
- (())setMessage:(id)message {
    let old = std::mem::replace(&mut env.objc.borrow_mut::<UIAlertViewHostObject>(this).message, nil);
    let new: id = if message != nil { retain(env, message); message } else { nil };
    env.objc.borrow_mut::<UIAlertViewHostObject>(this).message = new;
    if old != nil { release(env, old); }
}

- (NSInteger)addButtonWithTitle:(id)_title {
    let host = env.objc.borrow_mut::<UIAlertViewHostObject>(this);
    let idx = host.num_buttons;
    host.num_buttons += 1;
    idx
}

- (NSInteger)numberOfButtons {
    env.objc.borrow::<UIAlertViewHostObject>(this).num_buttons
}
- (NSInteger)cancelButtonIndex {
    0
}

- (())show {
    // We have no real alert UI, so we auto-dismiss by firing the cancel button
    // (index 0). CRUCIALLY this must happen ASYNCHRONOUSLY, not inline here:
    // callers like -[LoadingScene init] create the alert and call `show` while
    // still inside their own init (before the object has become the running
    // scene / had onEnter run). Their alertView:clickedButtonAtIndex: handler
    // does real work (e.g. LoadingScene's calls scheduleUpdate to start loading),
    // which only takes effect once the scene is active. So we schedule the
    // dismissal on the run loop for the next iteration, matching real UIKit where
    // the alert is modal and dismissed later by user interaction.
    log!("UIAlertView show: scheduling async auto-dismiss (index 0)");
    // Keep ourselves alive until the deferred dismissal runs.
    retain(env, this);
    let sel = env.objc.lookup_selector("_touchHLE_autoDismiss")
        .expect("_touchHLE_autoDismiss selector should be registered");
    () = msg![env; this performSelector:sel withObject:nil afterDelay:0.05f64];
}

- (())_touchHLE_autoDismiss {
    let dismiss_index: NSInteger = 0;
    let delegate = env.objc.borrow::<UIAlertViewHostObject>(this).delegate;
    log!("UIAlertView async auto-dismiss firing: index {} (delegate {:?})", dismiss_index, delegate);

    if delegate != nil {
        retain(env, delegate);
        if let Some(sel) = env.objc.lookup_selector("alertView:clickedButtonAtIndex:") {
            let responds: bool = msg![env; delegate respondsToSelector:sel];
            if responds {
                () = msg![env; delegate alertView:this clickedButtonAtIndex:dismiss_index];
            }
        }
        if let Some(sel) = env.objc.lookup_selector("alertView:didDismissWithButtonIndex:") {
            let responds: bool = msg![env; delegate respondsToSelector:sel];
            if responds {
                () = msg![env; delegate alertView:this didDismissWithButtonIndex:dismiss_index];
            }
        }
        release(env, delegate);
    }
    // Balance the retain in `show`.
    release(env, this);
}

- (())dismissWithClickedButtonIndex:(NSInteger)button_index
                           animated:(bool)_animated {
    let delegate = env.objc.borrow::<UIAlertViewHostObject>(this).delegate;
    if delegate != nil {
        if let Some(sel) = env.objc.lookup_selector("alertView:didDismissWithButtonIndex:") {
            let responds: bool = msg![env; delegate respondsToSelector:sel];
            if responds {
                () = msg![env; delegate alertView:this didDismissWithButtonIndex:button_index];
            }
        }
    }
}

@end

};
