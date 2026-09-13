package dev.bebok.mobile;

/**
 * Thread-safe reference count of in-flight "work" (an active turn) driving
 * the foreground service (F10-10). {@code beginWork()}/{@code endWork()} on
 * {@link EngineLauncherPlugin} are reference-counted because a turn can
 * involve overlapping async calls (e.g. a fleet of sub-agents); the service
 * only stops once every caller that began work has ended it.
 *
 * Pure/plain Java (no Android dependency) so it is covered by a JVM unit
 * test without Robolectric.
 */
public final class WorkCounter {

    private int count = 0;

    /** Increment the counter and return the new value. */
    public synchronized int begin() {
        count++;
        return count;
    }

    /**
     * Decrement the counter (floored at 0 - an unmatched {@code end()} is a
     * caller bug, not a reason to go negative) and return the new value.
     */
    public synchronized int end() {
        if (count > 0) {
            count--;
        }
        return count;
    }

    public synchronized boolean isActive() {
        return count > 0;
    }

    public synchronized int get() {
        return count;
    }
}
