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

/* 取得通知权限后执行 then(granted)(在系统的后台队列上)。
 * 玩家已经答复过:直接按现有结果执行(切后台那一刻预约提醒就走这条)。
 * 还没答复:系统只在应用处于前台活跃状态时才会弹授权框,启动早期场景还没进前台或切后台那一刻申请,请求会被挂起、
 * 授权框不出来、回调也不来,所以放到主线程,应用正活跃就立刻申请,否则等 UIApplicationDidBecomeActiveNotification。 */
static void
MoleWorld_Notify_WithAuthorization(void (^then)(BOOL granted))
{
    UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
    [center getNotificationSettingsWithCompletionHandler:^(UNNotificationSettings *settings) {
        if (settings.authorizationStatus != UNAuthorizationStatusNotDetermined) {
            if (then) {
                then(settings.authorizationStatus == UNAuthorizationStatusAuthorized ||
                     settings.authorizationStatus == UNAuthorizationStatusProvisional);
            }
            return;
        }
        dispatch_async(dispatch_get_main_queue(), ^{
            void (^request)(void) = ^{
                [center requestAuthorizationWithOptions:(UNAuthorizationOptionAlert | UNAuthorizationOptionSound | UNAuthorizationOptionBadge)
                                      completionHandler:^(BOOL granted, NSError *error) {
                    NSLog(@"[MoleWorld] 本地通知权限:%@", granted ? @"允许" : @"拒绝");
                    if (then) {
                        then(granted);
                    }
                }];
            };
            if ([UIApplication sharedApplication].applicationState == UIApplicationStateActive) {
                request();
                return;
            }
            __block id observer = [[NSNotificationCenter defaultCenter]
                addObserverForName:UIApplicationDidBecomeActiveNotification
                            object:nil
                             queue:[NSOperationQueue mainQueue]
                        usingBlock:^(NSNotification *note) {
                [[NSNotificationCenter defaultCenter] removeObserver:observer];
                observer = nil;
                request();
            }];
        });
    }];
}

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
        int generation = atomic_load(&moleworld_notify_generation);
        /* 切后台时应用已不活跃:若玩家还没答复过授权,这条要等回到前台答复后才加;已答复过的系统会立刻回调。 */
        MoleWorld_Notify_WithAuthorization(^(BOOL granted) {
            if (!granted || atomic_load(&moleworld_notify_generation) != generation) {
                return;
            }
            UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
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
                } else {
                    NSLog(@"[MoleWorld] 本地通知已预约 %@:%.0f 秒后", identifier, delay_secs);
                }
            }];
        });
    }
}

void
MoleWorld_Notify_CancelAll(void)
{
    static int authorization_requested = 0;
    atomic_fetch_add(&moleworld_notify_generation, 1);
    @autoreleasepool {
        UNUserNotificationCenter *center = [UNUserNotificationCenter currentNotificationCenter];
        /* 游戏进村和回前台时都会先全部取消,这时应用在前台。预约发生在切后台那一刻,系统那时未必弹授权框,
         * 第一次的提醒会静默丢掉;所以在第一次取消时就先申请(系统只问一次,之后直接返回结果)。 */
        if (!authorization_requested) {
            authorization_requested = 1;
            MoleWorld_Notify_WithAuthorization(nil);
        }
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
