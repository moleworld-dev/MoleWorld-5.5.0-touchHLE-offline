/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! [2026-10-29 第十八轮] 宿主平台集成:把游戏用到的系统服务接到 iOS / 安卓宿主上,桌面平台空操作。
//!
//! iOS 端的原生实现在 vendor/SDL/src/video/uikit/SDL_uikitmoleworld.m(SDL 的 UIKit 后端按通配符编译该目录);
//! 安卓端在 android/app/src/main/java/org/touchhle/android/MainActivity.java,宿主→Java 走 SDL_AndroidSendMessage,
//! Java→宿主走本文件导出的 JNI 原生函数。

#[cfg(target_os = "android")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicI8, Ordering};

// ───────────────────────── 音频会话(R18-C-1) ─────────────────────────
//
// 原版 CocosDenshion:-[GameSoundManager setUpAudioManager]@0x151fe 用模式 2(kAMM_FxPlusMusicIfNoOtherAudio)初始化,
// -[CDAudioManager setMode:]@0x2fc8a8 在 0x2fc8d8 问 isOtherAudioPlaying(AudioSessionGetProperty('othr')):
// 有别的 App 在放音乐 → 会话设成 Ambient(混音)且 willPlayBackgroundMusic=0(-[CDAudioManager playBackgroundMusic:loop:]
// 0x2fd07c 据此不放背景音乐,只出音效);没有 → SoloAmbient(独占,打断别的音频)并放背景音乐。
// 以前 'othr' 写死 0、setCategory: 空操作:iOS 宿主停在系统默认 SoloAmbient,一出声就打断用户的音乐;安卓不申请音频焦点,
// 两路音乐叠着放。

#[cfg(target_os = "android")]
static ANDROID_OTHER_AUDIO: AtomicBool = AtomicBool::new(false);

/// 安卓 MainActivity 在 onCreate / onResume 时调用(AudioManager.isMusicActive())。
#[cfg(target_os = "android")]
#[no_mangle]
pub extern "C" fn Java_org_touchhle_android_MainActivity_nativeSetOtherAudioPlaying(
    _env: *mut std::ffi::c_void,
    _class: *mut std::ffi::c_void,
    playing: u8,
) {
    ANDROID_OTHER_AUDIO.store(playing != 0, Ordering::Relaxed);
}

/// 有没有别的 App 在放音频(原版 kAudioSessionProperty_OtherAudioIsPlaying)。桌面平台恒为假。
pub fn other_audio_playing() -> bool {
    #[cfg(target_os = "ios")]
    {
        extern "C" {
            fn MoleWorld_Audio_OtherIsPlaying() -> std::ffi::c_int;
        }
        unsafe { MoleWorld_Audio_OtherIsPlaying() != 0 }
    }
    #[cfg(target_os = "android")]
    {
        ANDROID_OTHER_AUDIO.load(Ordering::Relaxed)
    }
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCategory {
    /// 与别的 App 的音频混音(AVAudioSessionCategoryAmbient / 'ambi')。
    Ambient,
    /// 独占,激活时打断别的 App 的音频(AVAudioSessionCategorySoloAmbient / 'solo',系统默认)。
    SoloAmbient,
}

/// 上次交给宿主的类别(-1 = 还没设过);游戏每次 setMode:/回前台都会重设,相同就不再打扰宿主。
static LAST_CATEGORY: AtomicI8 = AtomicI8::new(-1);

/// 把游戏请求的音频会话类别交给宿主。桌面平台空操作。
pub fn set_audio_category(category: AudioCategory) {
    let mix = category == AudioCategory::Ambient;
    if LAST_CATEGORY.swap(mix as i8, Ordering::Relaxed) == mix as i8 {
        return;
    }
    log!("[音频会话] 游戏请求类别 {:?}(与别的 App 混音:{})", category, mix);
    #[cfg(target_os = "ios")]
    {
        extern "C" {
            fn MoleWorld_Audio_SetCategory(mix_with_others: std::ffi::c_int);
        }
        unsafe { MoleWorld_Audio_SetCategory(mix as std::ffi::c_int) };
    }
    #[cfg(target_os = "android")]
    {
        // 与 MainActivity.COMMAND_AUDIO_CATEGORY 一致:参数 1 = 混音(放掉音频焦点),0 = 独占(申请 AUDIOFOCUS_GAIN)。
        const COMMAND_AUDIO_CATEGORY: u32 = 0x8002;
        if unsafe { SDL_AndroidSendMessage(COMMAND_AUDIO_CATEGORY, mix as std::ffi::c_int) } != 0 {
            log!("[音频会话] 安卓:设置音频焦点的消息没发出去");
        }
    }
}

#[cfg(target_os = "android")]
extern "C" {
    fn SDL_AndroidSendMessage(command: u32, param: std::ffi::c_int) -> std::ffi::c_int;
}
