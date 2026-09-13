package dev.bebok.mobile;

import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** F10-10: idle auto-stop decision, exercised with a fake clock (plain longs). */
public class IdleAutoStopPolicyTest {

    private static final long THRESHOLD = IdleAutoStopPolicy.DEFAULT_IDLE_THRESHOLD_MILLIS;

    @Test
    public void doesNotStopWhileAppIsInForeground() {
        long lastActivity = 0L;
        long now = lastActivity + THRESHOLD + 60_000;
        assertFalse(IdleAutoStopPolicy.shouldStop(lastActivity, now, true, false, THRESHOLD));
    }

    @Test
    public void doesNotStopWhileWorkIsActive() {
        long lastActivity = 0L;
        long now = lastActivity + THRESHOLD + 60_000;
        assertFalse(IdleAutoStopPolicy.shouldStop(lastActivity, now, false, true, THRESHOLD));
    }

    @Test
    public void doesNotStopBeforeTheThresholdElapses() {
        long lastActivity = 0L;
        long now = lastActivity + THRESHOLD - 1;
        assertFalse(IdleAutoStopPolicy.shouldStop(lastActivity, now, false, false, THRESHOLD));
    }

    @Test
    public void stopsOnceIdleThresholdElapsesInBackground() {
        long lastActivity = 0L;
        long now = lastActivity + THRESHOLD;
        assertTrue(IdleAutoStopPolicy.shouldStop(lastActivity, now, false, false, THRESHOLD));
    }

    @Test
    public void stopsWellPastTheThresholdToo() {
        long lastActivity = 1_000_000L;
        long now = lastActivity + THRESHOLD * 3;
        assertTrue(IdleAutoStopPolicy.shouldStop(lastActivity, now, false, false, THRESHOLD));
    }
}
