/*
 * [MoleWorld 2026-10-29 第十八轮] touchHLE 宿主在 iOS 上用到的系统服务(不属于 SDL 本身)。
 * Rust 侧声明与说明见 src/mole_host.rs。SDL 的 UIKit 后端按通配符编译本目录的 .m,只在 iOS/tvOS 上参与构建。
 */
#include "../../SDL_internal.h"

#if SDL_VIDEO_DRIVER_UIKIT

#import <AVFoundation/AVFoundation.h>

/* ───────────── 音频会话(R18-C-1) ─────────────
 * 原版 CocosDenshion 先问「有没有别的 App 在放音频」,再按结果把会话设成 Ambient(混音、不放背景音乐)或
 * SoloAmbient(独占、放背景音乐)。宿主进程在游戏第一次查询前先以 Ambient 激活会话:SoloAmbient 一激活就会把
 * 别的 App 的音乐停掉,之后再问就永远是「没有」。 */
static int moleworld_audio_prepared = 0;

static void
MoleWorld_Audio_Prepare(void)
{
    if (moleworld_audio_prepared) {
        return;
    }
    moleworld_audio_prepared = 1;
    AVAudioSession *session = [AVAudioSession sharedInstance];
    NSError *err = nil;
    if (![session setCategory:AVAudioSessionCategoryAmbient error:&err]) {
        NSLog(@"[MoleWorld] 音频会话:初始设为 Ambient 失败 %@", err);
    }
    err = nil;
    if (![session setActive:YES error:&err]) {
        NSLog(@"[MoleWorld] 音频会话:激活失败 %@", err);
    }
}

int
MoleWorld_Audio_OtherIsPlaying(void)
{
    @autoreleasepool {
        MoleWorld_Audio_Prepare();
        return [AVAudioSession sharedInstance].otherAudioPlaying ? 1 : 0;
    }
}

void
MoleWorld_Audio_SetCategory(int mix_with_others)
{
    @autoreleasepool {
        MoleWorld_Audio_Prepare();
        AVAudioSession *session = [AVAudioSession sharedInstance];
        NSString *category = mix_with_others ? AVAudioSessionCategoryAmbient : AVAudioSessionCategorySoloAmbient;
        NSError *err = nil;
        if (![session setCategory:category error:&err]) {
            NSLog(@"[MoleWorld] 音频会话:设类别 %@ 失败 %@", category, err);
            return;
        }
        err = nil;
        if (![session setActive:YES error:&err]) {
            NSLog(@"[MoleWorld] 音频会话:激活 %@ 失败 %@", category, err);
        }
    }
}

#endif /* SDL_VIDEO_DRIVER_UIKIT */
