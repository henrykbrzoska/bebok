package dev.bebok.mobile;

/**
 * Decides whether the embedded engine should be auto-stopped for idleness
 * (F10-10): no work active, the app not in the foreground, and the idle
 * threshold elapsed since the last recorded activity
 * ({@code start()}/{@code beginWork()}/{@code endWork()}).
 *
 * Pure function over caller-supplied timestamps (no {@code System}/clock
 * dependency baked in) so it is exercised by a plain JVM unit test with a
 * fake clock, without Robolectric.
 */
public final class IdleAutoStopPolicy {

    /** Default idle threshold: 10 minutes (design in WP-M3 brief F10-10). */
    public static final long DEFAULT_IDLE_THRESHOLD_MILLIS = 10L * 60L * 1000L;

    private IdleAutoStopPolicy() {
    }

    /**
     * @param lastActivityMillis wall-clock time of the last start/begin/end call
     * @param nowMillis          current wall-clock time
     * @param appInForeground    whether the app currently has a resumed activity
     * @param workActive         whether {@link WorkCounter#isActive()} is true
     * @param idleThresholdMillis how long the engine may sit idle before stopping
     */
    public static boolean shouldStop(
            long lastActivityMillis,
            long nowMillis,
            boolean appInForeground,
            boolean workActive,
            long idleThresholdMillis) {
        if (appInForeground || workActive) {
            return false;
        }
        return (nowMillis - lastActivityMillis) >= idleThresholdMillis;
    }
}
