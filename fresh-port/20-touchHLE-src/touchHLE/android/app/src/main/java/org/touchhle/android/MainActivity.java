/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 *
 * Parts of this file are derived from SDL 2's Android project template, which
 * has a different license. Please see vendor/SDL/LICENSE.txt for details.
 */
package org.touchhle.android;

import android.content.pm.ActivityInfo;
import android.util.Log;

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
}
