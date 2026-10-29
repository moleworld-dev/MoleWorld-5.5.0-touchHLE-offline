/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! AVAudioSession

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::foundation::ns_string;
use crate::mem::MutPtr;
use crate::objc::{id, objc_classes, ClassExports, TrivialHostObject};
use crate::todo_objc_setter;

type AVAudioSessionCategory = id; // NSString *

#[derive(Default)]
pub struct State {
    /// [AVAudioSession sharedInstance]
    shared_instance: Option<id>,
}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

// This is a singleton.
@implementation AVAudioSession: NSObject

+ (id)sharedInstance {
    if let Some(audio_session) =
        env.framework_state.avfoundation.av_audio_session.shared_instance {
        audio_session
    } else {
        let new = env.objc.alloc_static_object(
            this,
            Box::new(TrivialHostObject),
            &mut env.mem
        );
        env.framework_state.avfoundation.av_audio_session.shared_instance = Some(new);
        new
    }
}

- (id)retain { this }
- (())release {}
- (id)autorelease { this }

- (())setDelegate:(id)delegate {
    todo_objc_setter!(this, delegate);
}

- (bool)setCategory:(AVAudioSessionCategory)category
              error:(MutPtr<id>)error { // NSError **
    // [2026-10-29 R18-C-1] -[CDAudioManager audioSessionSetCategory:]@0x2fc470 按 isOtherAudioPlaying 选 Ambient / SoloAmbient,
    // 交给宿主音频会话(见 mole_host);其余类别本游戏不用,照旧只记日志。
    let name = ns_string::to_rust_string(env, category);
    match &*name {
        AVAudioSessionCategoryAmbient => {
            crate::mole_host::set_audio_category(crate::mole_host::AudioCategory::Ambient);
        }
        AVAudioSessionCategorySoloAmbient => {
            crate::mole_host::set_audio_category(crate::mole_host::AudioCategory::SoloAmbient);
        }
        _ => {
            log!(
                "TODO: [(AVAudioSession *){:?} setCategory:'{}' error:{:?}] -> true",
                this,
                name,
                error
            );
        }
    }
    true
}

- (bool)setActive:(bool)active
            error:(MutPtr<id>)error { // NSError **
    log!(
        "TODO: [(AVAudioSession *){:?} setActive:{} error:{:?}] -> true",
        this,
        active,
        error
    );
    true
}

@end

};

// Values might not be correct, but as these are linked symbol constants, it
// shouldn't matter.
const AVAudioSessionCategoryAmbient: &str = "AVAudioSessionCategoryAmbient";
const AVAudioSessionCategoryMultiRoute: &str = "AVAudioSessionCategoryMultiRoute";
const AVAudioSessionCategoryPlayAndRecord: &str = "AVAudioSessionCategoryPlayAndRecord";
const AVAudioSessionCategoryPlayback: &str = "AVAudioSessionCategoryPlayback";
const AVAudioSessionCategoryRecord: &str = "AVAudioSessionCategoryRecord";
const AVAudioSessionCategorySoloAmbient: &str = "AVAudioSessionCategorySoloAmbient";
const AVAudioSessionCategoryAudioProcessing: &str = "AVAudioSessionCategoryAudioProcessing";

/// `AVAudioSessionCategory` constants
pub const CONSTANTS: ConstantExports = &[
    (
        "_AVAudioSessionCategoryAmbient",
        HostConstant::NSString(AVAudioSessionCategoryAmbient),
    ),
    (
        "_AVAudioSessionCategoryMultiRoute",
        HostConstant::NSString(AVAudioSessionCategoryMultiRoute),
    ),
    (
        "_AVAudioSessionCategoryPlayAndRecord",
        HostConstant::NSString(AVAudioSessionCategoryPlayAndRecord),
    ),
    (
        "_AVAudioSessionCategoryPlayback",
        HostConstant::NSString(AVAudioSessionCategoryPlayback),
    ),
    (
        "_AVAudioSessionCategoryRecord",
        HostConstant::NSString(AVAudioSessionCategoryRecord),
    ),
    (
        "_AVAudioSessionCategorySoloAmbient",
        HostConstant::NSString(AVAudioSessionCategorySoloAmbient),
    ),
    (
        "_AVAudioSessionCategoryAudioProcessing",
        HostConstant::NSString(AVAudioSessionCategoryAudioProcessing),
    ),
];
