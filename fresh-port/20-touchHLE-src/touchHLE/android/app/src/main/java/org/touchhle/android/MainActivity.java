/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Parts of this file are derived from SDL 2's Android project template, which
 * has a different license. Please see vendor/SDL/LICENSE.txt for details.
 */
package org.touchhle.android;

import android.content.Context;
import android.content.ComponentCallbacks2;
import android.content.pm.ActivityInfo;
import android.os.SystemClock;
import android.util.Log;
import android.view.inputmethod.InputMethodManager;

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

    /**
     * [2026-10-29 第十一轮 R11-H-7] 游戏委托拒收输入(如换头像面板拒收纯空格)后,宿主经 SDL_AndroidSendMessage 发来。
     * SDLInputConnection 按自己的 Editable 做差量补退格,不知道游戏没收,之后删字会多删一个真字。
     * restartInput 让输入法重新 onCreateInputConnection,新连接从空文本开始(和刚弹出键盘时一样),键盘不收起。
     * 本方法由 SDLCommandHandler 在 UI 线程调用。
     */
    @Override
    protected boolean onUnhandledMessage(int command, Object param) {
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
