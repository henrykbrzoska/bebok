package dev.bebok.mobile;

import java.util.Set;

/**
 * Picks which bundled per-ABI engine directory (assets/bin/&lt;abi&gt;/) to
 * extract and run, given the device's supported ABIs (most-preferred first,
 * as reported by {@code Build.SUPPORTED_ABIS}) and the set of ABI directories
 * actually bundled in this APK (F10-9: {@code BEBOK_ANDROID_ABIS} controls
 * which ones {@code bundle-android.sh} produces).
 *
 * Pure function - no Android framework dependency - so it can be exercised by
 * a plain JVM unit test (src/test) without Robolectric/instrumentation.
 */
public final class AbiAssetResolver {

    private AbiAssetResolver() {
    }

    /**
     * @param supportedAbis  device ABIs, most-preferred first (e.g. from
     *                       {@code Build.SUPPORTED_ABIS})
     * @param bundledAbiDirs ABI directory names present under assets/bin/
     * @return the first supported ABI that is also bundled, or {@code null}
     *         if none match (caller should fail with a clear message).
     */
    public static String resolve(String[] supportedAbis, Set<String> bundledAbiDirs) {
        if (supportedAbis == null || bundledAbiDirs == null || bundledAbiDirs.isEmpty()) {
            return null;
        }
        for (String abi : supportedAbis) {
            if (abi != null && bundledAbiDirs.contains(abi)) {
                return abi;
            }
        }
        return null;
    }
}
