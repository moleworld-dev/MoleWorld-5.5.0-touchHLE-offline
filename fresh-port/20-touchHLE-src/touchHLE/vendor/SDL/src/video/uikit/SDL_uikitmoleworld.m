/*
 * [MoleWorld 2026-10-29 第十八轮] touchHLE 宿主在 iOS 上用到的系统服务(不属于 SDL 本身)。
 * Rust 侧声明与说明见 src/mole_host.rs。SDL 的 UIKit 后端按通配符编译本目录的 .m,只在 iOS/tvOS 上参与构建。
 */
#include "../../SDL_internal.h"

#if SDL_VIDEO_DRIVER_UIKIT

#import <AVFoundation/AVFoundation.h>
#import <UIKit/UIKit.h>
#import <UserNotifications/UserNotifications.h>
#include <stdatomic.h>

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

/* ───────────── 本地通知(R18-C-2) ─────────────
 * 原版切后台时预约「作物成熟 / 作物快枯萎 / 摩尔派遣回来 / 太久没来」四条提醒(UILocalNotification),回前台时全部取消。
 * 原版按 iOS 6 SDK 编译,不需要申请权限;宿主按现行系统要求在第一次预约前申请,玩家拒绝就静默跳过。
 * 提示音 UI_021.wav 在游戏包里,不在宿主 bundle 里,用系统默认提示音。 */
static int moleworld_notify_seq = 0;
/* 每次全部取消加一。申请权限的回调是异步的:切后台马上又回前台时,取消可能先于回调执行,回调里见代数变了就不再添加。 */
static atomic_int moleworld_notify_generation = 0;

void
MoleWorld_Notify_Schedule(double delay_secs, const char *body, const char *action)
{
    if (delay_secs <= 0) {
        return;
    }
    @autoreleasepool {
        NSString *text = body ? [NSString stringWithUTF8String:body] : nil;
        if (text.length == 0) {
            return;
        }
        NSString *identifier = [NSString stringWithFormat:@"moleworld-%d", moleworld_notify_seq++];
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        int generation = atomic_load(&moleworld_notify_generation);
        [center requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound | UNAuthorizationOptionBadge)
                              completionHandler:^(BOOL granted, NSError *error) {
            if (!granted || atomic_load(&moleworld_notify_generation) != generation) {
                return;
            }
            UNMutableNotificationContent *content = [[UNMutableNotificationContent alloc] init];
            content.body = text;
            content.sound = [UNNotificationSound defaultSound];
            UNTimeIntervalNotificationTrigger *trigger =
                [UNTimeIntervalNotificationTrigger triggerWithTimeInterval:delay_secs repeats:NO];
            UNNotificationRequest *request =
                [UNNotificationRequest requestWithIdentifier:identifier content:content trigger:trigger];
            [center addNotificationRequest:request withCompletionHandler:^(NSError *err) {
                if (err) {
                    NSLog(@"[MoleWorld] 本地通知预约失败 %@", err);
                }
            }];
        }];
    }
}

void
MoleWorld_Notify_CancelAll(void)
{
    atomic_fetch_add(&moleworld_notify_generation, 1);
    @autoreleasepool {
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        [center removeAllPendingNotificationRequests];
        [center removeAllDeliveredNotifications];
    }
}

void
MoleWorld_Notify_SetBadge(int number)
{
    @autoreleasepool {
        if (number < 0) {
            number = 0;
        }
        if (@available(iOS 16.0, *)) {
            [[UNUserNotificationCenter currentNotificationCenter] setBadgeCount:number withCompletionHandler:nil];
        } else {
            dispatch_async(dispatch_get_main_queue(), ^{
                [UIApplication sharedApplication].applicationIconBadgeNumber = number;
            });
        }
    }
}

#endif /* SDL_VIDEO_DRIVER_UIKIT */
