package dev.bebok.mobile;

import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.Build;
import android.os.IBinder;

import androidx.core.app.NotificationCompat;
import androidx.core.app.ServiceCompat;

/**
 * Foreground service raised while the embedded engine is doing work
 * (F10-10): started by {@code EngineLauncherPlugin.beginWork()}, stopped when
 * the matching {@code endWork()} brings the reference count to zero. Type
 * {@code dataSync} (Android 14+ requires a declared FGS type; this one moves
 * data between the on-device engine and an LLM provider over HTTPS, which is
 * the closest fit) with a low-importance, non-dismissible notification and a
 * "Stop" action that kills the engine.
 *
 * Uses {@code androidx.core.app} compat wrappers throughout ({@link
 * NotificationCompat.Builder}, {@link ServiceCompat#startForeground}) rather
 * than the raw platform APIs - minSdk is 23, well below the API 26 (O)
 * notification channel APIs and the API 29/34 typed {@code startForeground}
 * overloads these wrap.
 *
 * Tolerates a denied POST_NOTIFICATIONS permission (API 33+): the runtime
 * request happens on the JS side later (WP-M5); if notifications are denied,
 * {@code startForeground()} still succeeds and the service still runs - the
 * user just does not see the notification. That is expected Android
 * behaviour and not an error here.
 */
public class EngineForegroundService extends Service {

    private static final String CHANNEL_ID = "bebok-engine-work";
    private static final int NOTIFICATION_ID = 4201;
    public static final String ACTION_STOP = "dev.bebok.mobile.action.STOP_ENGINE";
    public static final String EXTRA_REASON = "reason";

    /**
     * Set by {@link EngineLauncherPlugin} on load(); invoked when the user
     * taps "Stop" on the notification. A plain static callback is enough
     * because the service and the plugin live in the same process.
     */
    private static volatile Runnable stopCallback;

    public static void setStopCallback(Runnable callback) {
        stopCallback = callback;
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        if (intent != null && ACTION_STOP.equals(intent.getAction())) {
            Runnable cb = stopCallback;
            if (cb != null) {
                cb.run();
            }
            stopForeground(true);
            stopSelf();
            return START_NOT_STICKY;
        }

        String reason = intent != null ? intent.getStringExtra(EXTRA_REASON) : null;
        ensureChannel();
        android.app.Notification notification = buildNotification();
        ServiceCompat.startForeground(
                this,
                NOTIFICATION_ID,
                notification,
                Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
                        ? ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
                        : 0);
        return START_NOT_STICKY;
    }

    private android.app.Notification buildNotification() {
        Intent stopIntent = new Intent(this, EngineForegroundService.class);
        stopIntent.setAction(ACTION_STOP);
        int piFlags = PendingIntent.FLAG_UPDATE_CURRENT
                | (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M ? PendingIntent.FLAG_IMMUTABLE : 0);
        PendingIntent stopPending = PendingIntent.getService(this, 0, stopIntent, piFlags);

        return new NotificationCompat.Builder(this, CHANNEL_ID)
                .setContentTitle(getString(R.string.engine_work_notification_title))
                .setContentText(getString(R.string.engine_work_notification_text))
                .setSmallIcon(R.drawable.ic_engine_notification)
                .setOngoing(true)
                .setOnlyAlertOnce(true)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                .addAction(0, getString(R.string.engine_work_notification_stop), stopPending)
                .build();
    }

    /** No-op below API 26 - {@link NotificationCompat.Builder} degrades gracefully without a channel. */
    private void ensureChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm != null && nm.getNotificationChannel(CHANNEL_ID) == null) {
            NotificationChannel channel = new NotificationChannel(
                    CHANNEL_ID,
                    getString(R.string.engine_work_notification_channel),
                    NotificationManager.IMPORTANCE_LOW);
            channel.setShowBadge(false);
            nm.createNotificationChannel(channel);
        }
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null;
    }

    /** Convenience for the plugin: build the start intent with a reason. */
    static Intent startIntent(Context ctx, String reason) {
        Intent intent = new Intent(ctx, EngineForegroundService.class);
        intent.putExtra(EXTRA_REASON, reason);
        return intent;
    }
}
