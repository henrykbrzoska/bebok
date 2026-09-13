package dev.bebok.mobile;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;

import java.util.LinkedHashSet;
import java.util.Set;

import org.junit.Test;

/** F10-9: ABI selection helper - plain JVM test, no Android framework needed. */
public class AbiAssetResolverTest {

    @Test
    public void picksFirstSupportedAbiThatIsBundled() {
        String[] supported = {"arm64-v8a", "armeabi-v7a", "armeabi"};
        Set<String> bundled = new LinkedHashSet<>();
        bundled.add("arm64-v8a");
        bundled.add("x86_64");

        assertEquals("arm64-v8a", AbiAssetResolver.resolve(supported, bundled));
    }

    @Test
    public void fallsThroughToALaterSupportedAbi() {
        // Emulator: device prefers x86_64 but also reports arm64 emulation
        // via libhoudini/ndk-translation; only x86_64 is bundled.
        String[] supported = {"arm64-v8a", "x86_64"};
        Set<String> bundled = new LinkedHashSet<>();
        bundled.add("x86_64");

        assertEquals("x86_64", AbiAssetResolver.resolve(supported, bundled));
    }

    @Test
    public void returnsNullWhenNoBundledAbiMatches() {
        String[] supported = {"armeabi-v7a", "armeabi"};
        Set<String> bundled = new LinkedHashSet<>();
        bundled.add("arm64-v8a");
        bundled.add("x86_64");

        assertNull(AbiAssetResolver.resolve(supported, bundled));
    }

    @Test
    public void returnsNullWhenNothingIsBundled() {
        String[] supported = {"arm64-v8a"};
        assertNull(AbiAssetResolver.resolve(supported, new LinkedHashSet<>()));
    }

    @Test
    public void returnsNullOnNullInputs() {
        assertNull(AbiAssetResolver.resolve(null, new LinkedHashSet<>()));
        assertNull(AbiAssetResolver.resolve(new String[] {"arm64-v8a"}, null));
    }
}
