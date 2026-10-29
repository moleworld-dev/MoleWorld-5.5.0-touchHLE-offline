/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
package org.touchhle.android;

import android.app.Notification;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.os.Build;

/**
 * [2026-10-29 第十八轮 R18-C-2] 游戏预约的本地提醒(作物成熟 / 作物快枯萎 / 摩尔派遣回来 / 太久没来)到点后由
 * AlarmManager 唤起,在这里发系统通知;点通知回到游戏。排程与取消见 MainActivity。
 */
public class NotificationReceiver extends BroadcastReceiver {
    static final String EXTRA_BODY = "body";
    static final String EXTRA_ID = "id";

    @Override
    public void onReceive(Context context, Intent intent) {
        String body = intent.getStringExtra(EXTRA_BODY);
        if (body == null || body.isEmpty()) {
            return;
        }
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null) {
            return;
        }
        MainActivity.ensureNotificationChannel(context);
        Intent launch = new Intent(context, MainActivity.class)
                .setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent open = PendingIntent.getActivity(context, 0, launch,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification.Builder builder;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            builder = new Notification.Builder(context, MainActivity.NOTIFY_CHANNEL);
        } else {
            builder = new Notification.Builder(context).setDefaults(Notification.DEFAULT_SOUND);
        }
        builder.setSmallIcon(context.getApplicationInfo().icon)
                .setContentTitle(context.getApplicationInfo().loadLabel(context.getPackageManager()))
                .setContentText(body)
                .setAutoCancel(true)
                .setContentIntent(open);
        nm.notify(intent.getIntExtra(EXTRA_ID, 0), builder.build());
    }
}
