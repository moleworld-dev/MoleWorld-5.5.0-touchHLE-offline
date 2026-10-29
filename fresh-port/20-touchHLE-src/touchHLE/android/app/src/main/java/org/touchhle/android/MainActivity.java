/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Parts of this file are derived from SDL 2's Android project template, which
 * has a different license. Please see vendor/SDL/LICENSE.txt for details.
 */
package org.touchhle.android;

import android.app.AlarmManager;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.os.Build;
import android.content.ComponentCallbacks2;
import android.content.pm.ActivityInfo;
import android.media.AudioManager;
import android.os.Bundle;
import android.os.SystemClock;
import android.util.Log;
import android.view.inputmethod.InputMethodManager;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileInputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;

import org.libsdl.app.SDLActivity;

/**
 * A wrapper class over SDLActivity
 */

public class MainActivity extends SDLActivity {
    @Override
    protected String[] getLibraries() {
        return new String[]{
            "SDL2",
            "touchHLE"
        };
    }

    /**
     * [2026-10-06 第九轮 R9-B7] 两个横屏方向都放开时(window.rs 的 set_sdl2_orientation 在安卓横屏下给
     * "LandscapeLeft LandscapeRight"),SDL 会选 SCREEN_ORIENTATION_SENSOR_LANDSCAPE,它无视系统的「方向锁定」。
     * 原版 iPad 会遵守方向锁,这里改用 USER_LANDSCAPE(两个横屏随传感器翻转,但遵守方向锁)。其余情形交给 SDL。
     */
    @Override
    public void setOrientationBis(int w, int h, boolean resizable, String hint) {
        if (hint != null && hint.contains("LandscapeLeft") && hint.contains("LandscapeRight")
                && !hint.contains("Portrait")) {
            Log.v("touchHLE", "setOrientation() 两个横屏都允许 → USER_LANDSCAPE(遵守系统方向锁) hint=" + hint);
            setRequestedOrientation(ActivityInfo.SCREEN_ORIENTATION_USER_LANDSCAPE);
            return;
        }
        super.setOrientationBis(w, h, resizable, hint);
    }

    /** 上次转发前台内存告警的时刻(elapsedRealtime 毫秒)。 */
    private long mLastTrimForwardMs = -1;

    /**
     * [2026-10-29 第十一轮 R11-H-5] SDL 只接了 onLowMemory,它约等于后台档位,前台几乎收不到。前台内存吃紧时系统先回调
     * onTrimMemory(TRIM_MEMORY_RUNNING_LOW / RUNNING_CRITICAL),这里转给 nativeLowMemory,复用现成的 SDL_APP_LOWMEMORY →
     * applicationDidReceiveMemoryWarning:(原版只做 [[CCDirector sharedDirector] purgeCachedData])链路。
     * 后台档位不转(挂起期间宿主本来就不处理);30 秒内只转一次,原版 iOS 告警也不会接连来。
     * Android 14 起这两个档位被标为弃用、系统不再保证投递,只是补全来源,不能当成防被杀的手段。
     */
    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        if (mBrokenLibraries) {
            return;
        }
        if (level != ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW
                && level != ComponentCallbacks2.TRIM_MEMORY_RUNNING_CRITICAL) {
            return;
        }
        long now = SystemClock.elapsedRealtime();
        if (mLastTrimForwardMs >= 0 && now - mLastTrimForwardMs < 30_000) {
            return;
        }
        mLastTrimForwardMs = now;
        Log.v("touchHLE", "onTrimMemory(" + level + ") 前台内存吃紧 → 转发内存告警");
        nativeLowMemory();
    }

    /** 与 window.rs 的 reset_text_input_connection 一致。 */
    private static final int COMMAND_RESTART_INPUT = COMMAND_USER + 1;
    /** 与 mole_host.rs 的 set_audio_category 一致:参数 1 = 与别的 App 混音,0 = 独占。 */
    private static final int COMMAND_AUDIO_CATEGORY = COMMAND_USER + 2;

    /**
     * [2026-10-29 第十八轮 R18-C-1] 告诉宿主有没有别的 App 在放音乐(原版 kAudioSessionProperty_OtherAudioIsPlaying)。
     * 原版 CocosDenshion 据此选 Ambient(混音、不放背景音乐,只出音效)或 SoloAmbient(独占、放背景音乐)。
     */
    private static native void nativeSetOtherAudioPlaying(boolean playing);

    /** 上次游戏选的是不是独占类别(-1 = 还没选)。 */
    private int mAudioSolo = -1;
    private final AudioManager.OnAudioFocusChangeListener mAudioFocusListener = focusChange -> { };

    private void reportOtherAudio() {
        if (mBrokenLibraries) {
            return;
        }
        AudioManager am = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
        boolean playing = am != null && am.isMusicActive();
        try {
            nativeSetOtherAudioPlaying(playing);
        } catch (UnsatisfiedLinkError e) {
            Log.w("touchHLE", "nativeSetOtherAudioPlaying 不可用", e);
            return;
        }
        // 回前台时别的音乐已经停了、游戏上次选的是独占:像 iOS 重新激活 SoloAmbient 会话一样重新拿回音频焦点。
        if (!playing && mAudioSolo == 1 && am != null) {
            am.requestAudioFocus(mAudioFocusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN);
        }
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        reportOtherAudio();
        // targetSdk 31 的应用在安卓 13+ 上,系统会在建第一个通知渠道时弹「是否允许通知」(应用在前台时)。
        ensureNotificationChannel(this);
    }

    // ───── [2026-10-29 第十八轮 R18-C-2] 本地提醒 ─────
    // 原版切后台时预约「作物成熟 / 作物快枯萎 / 摩尔派遣回来 / 太久没来」,回前台时全部取消。宿主(mole_host.rs)把每条
    // 写成 <getExternalFilesDir>/mole_notify/<触发毫秒>-<序号>.txt(第 1 行触发时刻毫秒时间戳,第 2 行正文,第 3 行按钮文字),
    // 再发 COMMAND_NOTIFY_SCHEDULE;这里读走、用 AlarmManager 排上并删除文件。不精确闹钟即可(安卓 12+ 精确闹钟要额外权限)。
    // 重启手机后已排的提醒会丢,与原版无关紧要的差别。
    static final String NOTIFY_CHANNEL = "moleworld_reminders";
    private static final int COMMAND_NOTIFY_SCHEDULE = COMMAND_USER + 3;
    private static final int COMMAND_NOTIFY_CANCEL = COMMAND_USER + 4;
    private static final String NOTIFY_PREFS = "moleworld_notify";
    private static final String NOTIFY_NEXT_ID = "next_id";

    static void ensureNotificationChannel(Context context) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null || nm.getNotificationChannel(NOTIFY_CHANNEL) != null) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(NOTIFY_CHANNEL, "庄园提醒",
                NotificationManager.IMPORTANCE_DEFAULT);
        channel.setDescription("作物成熟、作物快枯萎、摩尔派遣回来等提醒");
        nm.createNotificationChannel(channel);
    }

    private File notifyDir() {
        File base = getExternalFilesDir(null);
        return base == null ? null : new File(base, "mole_notify");
    }

    private PendingIntent reminderIntent(int id, String body, int flags) {
        Intent intent = new Intent(this, NotificationReceiver.class)
                .putExtra(NotificationReceiver.EXTRA_ID, id);
        if (body != null) {
            intent.putExtra(NotificationReceiver.EXTRA_BODY, body);
        }
        return PendingIntent.getBroadcast(this, id, intent, PendingIntent.FLAG_IMMUTABLE | flags);
    }

    private void schedulePendingReminders() {
        File dir = notifyDir();
        File[] files = dir == null ? null : dir.listFiles((d, name) -> name.endsWith(".txt"));
        AlarmManager am = (AlarmManager) getSystemService(Context.ALARM_SERVICE);
        if (files == null || am == null) {
            return;
        }
        SharedPreferences prefs = getSharedPreferences(NOTIFY_PREFS, MODE_PRIVATE);
        int nextId = prefs.getInt(NOTIFY_NEXT_ID, 0);
        for (File f : files) {
            try (BufferedReader r = new BufferedReader(
                    new InputStreamReader(new FileInputStream(f), StandardCharsets.UTF_8))) {
                long fireMs = Long.parseLong(r.readLine().trim());
                String body = r.readLine();
                if (body != null && !body.isEmpty()) {
                    int id = nextId++;
                    am.set(AlarmManager.RTC_WAKEUP, fireMs,
                            reminderIntent(id, body, PendingIntent.FLAG_UPDATE_CURRENT));
                    Log.v("touchHLE", "本地提醒已排:" + body + " @" + fireMs);
                }
            } catch (Exception e) {
                Log.w("touchHLE", "读本地提醒预约失败 " + f, e);
            }
            //noinspection ResultOfMethodCallIgnored
            f.delete();
        }
        prefs.edit().putInt(NOTIFY_NEXT_ID, nextId).apply();
    }

    private void cancelAllReminders() {
        AlarmManager am = (AlarmManager) getSystemService(Context.ALARM_SERVICE);
        SharedPreferences prefs = getSharedPreferences(NOTIFY_PREFS, MODE_PRIVATE);
        int nextId = prefs.getInt(NOTIFY_NEXT_ID, 0);
        for (int id = 0; id < nextId; id++) {
            PendingIntent pi = reminderIntent(id, null, PendingIntent.FLAG_NO_CREATE);
            if (pi != null) {
                if (am != null) {
                    am.cancel(pi);
                }
                pi.cancel();
            }
        }
        prefs.edit().putInt(NOTIFY_NEXT_ID, 0).apply();
        NotificationManager nm = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null) {
            nm.cancelAll();
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        reportOtherAudio();
    }

    /** UI 线程:独占 = 申请 AUDIOFOCUS_GAIN(别的 App 暂停,同 iOS SoloAmbient 打断别的音频);混音 = 放掉焦点(两边一起响,同 Ambient)。 */
    private void applyAudioCategory(boolean mix) {
        mAudioSolo = mix ? 0 : 1;
        AudioManager am = (AudioManager) getSystemService(Context.AUDIO_SERVICE);
        if (am == null) {
            return;
        }
        if (mix) {
            am.abandonAudioFocus(mAudioFocusListener);
        } else {
            am.requestAudioFocus(mAudioFocusListener, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN);
        }
        Log.v("touchHLE", "音频会话:游戏选" + (mix ? "混音(放掉音频焦点)" : "独占(申请音频焦点)"));
    }

    /**
     * [2026-10-29 第十一轮 R11-H-7] 游戏委托拒收输入(如换头像面板拒收纯空格)后,宿主经 SDL_AndroidSendMessage 发来。
     * SDLInputConnection 按自己的 Editable 做差量补退格,不知道游戏没收,之后删字会多删一个真字。
     * restartInput 让输入法重新 onCreateInputConnection,新连接从空文本开始(和刚弹出键盘时一样),键盘不收起。
     * 本方法由 SDLCommandHandler 在 UI 线程调用。
     */
    @Override
    protected boolean onUnhandledMessage(int command, Object param) {
        if (command == COMMAND_NOTIFY_SCHEDULE) {
            schedulePendingReminders();
            return true;
        }
        if (command == COMMAND_NOTIFY_CANCEL) {
            cancelAllReminders();
            return true;
        }
        if (command == COMMAND_AUDIO_CATEGORY) {
            applyAudioCategory(param instanceof Integer && (Integer) param != 0);
            return true;
        }
        if (command == COMMAND_RESTART_INPUT) {
            if (mTextEdit != null) {
                InputMethodManager imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
                if (imm != null) {
                    imm.restartInput(mTextEdit);
                }
            }
            return true;
        }
        return super.onUnhandledMessage(command, param);
    }
}
