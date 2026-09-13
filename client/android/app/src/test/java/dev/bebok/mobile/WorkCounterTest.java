package dev.bebok.mobile;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;

import org.junit.Test;

/** F10-10: beginWork()/endWork() reference counting. */
public class WorkCounterTest {

    @Test
    public void startsInactive() {
        WorkCounter counter = new WorkCounter();
        assertFalse(counter.isActive());
        assertEquals(0, counter.get());
    }

    @Test
    public void beginMakesItActive() {
        WorkCounter counter = new WorkCounter();
        assertEquals(1, counter.begin());
        assertTrue(counter.isActive());
    }

    @Test
    public void onlyGoesInactiveWhenEveryBeginIsMatchedByAnEnd() {
        WorkCounter counter = new WorkCounter();
        counter.begin();
        counter.begin();
        assertEquals(2, counter.get());

        assertEquals(1, counter.end());
        assertTrue("still active - one begin() unmatched", counter.isActive());

        assertEquals(0, counter.end());
        assertFalse(counter.isActive());
    }

    @Test
    public void endIsFlooredAtZero() {
        WorkCounter counter = new WorkCounter();
        assertEquals(0, counter.end());
        assertEquals(0, counter.end());
        assertFalse(counter.isActive());
    }
}
