package dev.bebok.mobile;

import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.os.Build;
import android.util.Log;
import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.util.HashMap;
import java.util.Iterator;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

/**
 * Launches the embedded bebok-server engine on Android. The engine binds
 * 127.0.0.1 with a random port and prints `BEBOK_READY http://host:port` on
 * stdout; we capture that and hand the URL back to the webview so it can talk
 * to the local engine (same as the desktop sidecar, but in-process within the
 * app).
 *
 * F10-29 (supersedes F10-9's assets/bin/&lt;abi&gt;/ layout): the engine is
 * packaged as the "native library" {@code lib/<abi>/libbebok_server.so}
 * (jniLibs, legacy packaging) and executed straight from
 * {@link ApplicationInfo#nativeLibraryDir}. Android 10+ denies exec on files
 * an untrusted app wrote itself (W^X: copying the asset into {@code files/bin}
 * failed with {@code error=13, Permission denied} on Android 16), while the
 * installer-extracted native-library directory stays executable. The ABI is
 * picked by the installer, so nothing is copied or resolved at runtime.
 *
 * The bundled shell (mksh, {@code libmksh.so}) sits next to the engine and its
 * path is set via `BEBOK_SHELL`, so the engine's `bash` tool and (if enabled)
 * PTY work when it was built.
 *
 * F10-10 adds a reference-counted foreground service for the duration of a
 * turn ({@link #beginWork}/{@link #endWork}) and an idle auto-stop timer that
 * kills the engine after {@link IdleAutoStopPolicy#DEFAULT_IDLE_THRESHOLD_MILLIS}
 * of inactivity while the app is backgrounded.
 *
 * WP-M5 (F10-21, device testing): {@code start({ env: { BEBOK_PROVIDER_MOCK: "1" } })}
 * forwards WP-M1's deterministic mock-provider switch to the engine. The hook
 * is limited to {@link #DEBUG_ENV_ALLOWLIST} and is ignored entirely unless
 * the installed APK is debuggable ({@link ApplicationInfo#FLAG_DEBUGGABLE}),
 * so a release build can never be talked into the mock provider. In a
 * debuggable build the system property {@code debug.bebok.provider_mock}
 * ({@code adb shell setprop debug.bebok.provider_mock 1}) has the same effect
 * without touching the UI.
 */
@CapacitorPlugin(name = "EngineLauncher")
public class EngineLauncherPlugin extends Plugin {

    private static final String TAG = "BebokEngine";
    private static final String READY_PREFIX = "BEBOK_READY ";
    /** F10-29: engine + shell file names inside {@code ApplicationInfo.nativeLibraryDir}. */
    static final String SERVER_LIB = "libbebok_server.so";
    static final String SHELL_LIB = "libmksh.so";
    private static final long IDLE_CHECK_PERIOD_SECONDS = 60;
    /** Env vars JS may set on the engine - debuggable builds only. */
    static final Set<String> DEBUG_ENV_ALLOWLIST = Set.of("BEBOK_PROVIDER_MOCK");
    static final String MOCK_SYSPROP = "debug.bebok.provider_mock";
    /**
     * Debuggable builds only: `adb shell setprop debug.bebok.idle_ms 90000`
     * shortens the idle auto-stop threshold so the behaviour can be verified
     * on a device without waiting the full ten minutes. Read once in
     * {@link #load()}; ignored (and never read) in a non-debuggable build.
     */
    static final String IDLE_SYSPROP = "debug.bebok.idle_ms";

    private Process process;
    private volatile String baseUrl;

    private final WorkCounter workCounter = new WorkCounter();
    private volatile long lastActivityMillis = System.currentTimeMillis();
    private volatile boolean appInForeground = true;

    private ScheduledExecutorService idleExecutor;
    private ScheduledFuture<?> idleTask;
    private long idleThresholdMillis = IdleAutoStopPolicy.DEFAULT_IDLE_THRESHOLD_MILLIS;

    @Override
    public void load() {
        super.load();
        EngineForegroundService.setStopCallback(this::stopFromNotification);
        if (isDebuggable(getContext())) {
            String prop = readSystemProperty(IDLE_SYSPROP);
            if (!prop.isEmpty()) {
                try {
                    long ms = Long.parseLong(prop);
                    if (ms > 0) {
                        idleThresholdMillis = ms;
                        Log.w(TAG, "debug idle auto-stop threshold: " + ms + " ms");
                    }
                } catch (NumberFormatException ignored) {
                    Log.w(TAG, "ignoring " + IDLE_SYSPROP + "=" + prop + " (not a number)");
                }
            }
        }
        idleExecutor = Executors.newSingleThreadScheduledExecutor();
        idleTask = idleExecutor.scheduleWithFixedDelay(
                this::checkIdle, IDLE_CHECK_PERIOD_SECONDS, IDLE_CHECK_PERIOD_SECONDS, TimeUnit.SECONDS);
    }

    @PluginMethod
    public void start(PluginCall call) {
        try {
            // Serialised: the JS side may call start() from two places at once
            // (EventsStore's reconnect loop + an explicit reconnect) and must
            // never end up with two engine processes.
            synchronized (this) {
                if (baseUrl != null && (process == null || !process.isAlive())) {
                    // Killed from outside (OS memory pressure, `kill`): the
                    // remembered URL points at a dead port - relaunch.
                    Log.w(TAG, "engine process is gone; relaunching");
                    killProcess();
                }
                if (baseUrl == null) {
                    baseUrl = launch(debugEnv(call.getObject("env")));
                }
            }
            recordActivity();
            JSObject ret = new JSObject();
            ret.put("baseUrl", baseUrl);
            call.resolve(ret);
        } catch (Exception e) {
            call.reject("engine launch failed: " + e.getMessage(), e);
        }
    }

    @PluginMethod
    public void stop(PluginCall call) {
        killProcess();
        recordActivity();
        call.resolve();
    }

    /**
     * Reference-counted "a turn is in progress" signal. Starts the foreground
     * service (idempotent - Android coalesces repeated startForegroundService
     * calls into one running service instance).
     */
    @PluginMethod
    public void beginWork(PluginCall call) {
        String reason = call.getString("reason", "");
        workCounter.begin();
        recordActivity();
        Context ctx = getContext();
        Intent intent = EngineForegroundService.startIntent(ctx, reason);
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            ctx.startForegroundService(intent);
        } else {
            ctx.startService(intent);
        }
        call.resolve();
    }

    /** Matching end for {@link #beginWork}; stops the service at count zero. */
    @PluginMethod
    public void endWork(PluginCall call) {
        int remaining = workCounter.end();
        recordActivity();
        if (remaining == 0) {
            getContext().stopService(new Intent(getContext(), EngineForegroundService.class));
        }
        call.resolve();
    }

    @Override
    protected void handleOnResume() {
        super.handleOnResume();
        appInForeground = true;
    }

    @Override
    protected void handleOnPause() {
        super.handleOnPause();
        appInForeground = false;
        recordActivity();
    }

    private void recordActivity() {
        lastActivityMillis = System.currentTimeMillis();
    }

    /** Runs on the idle-check executor thread every {@link #IDLE_CHECK_PERIOD_SECONDS}. */
    private void checkIdle() {
        if (baseUrl == null) {
            return;
        }
        long now = System.currentTimeMillis();
        boolean workActive = workCounter.isActive();
        boolean stop = IdleAutoStopPolicy.shouldStop(
                lastActivityMillis, now, appInForeground, workActive, idleThresholdMillis);
        Log.d(TAG, "idle check: idle " + (now - lastActivityMillis) / 1000 + " s, foreground="
                + appInForeground + ", work=" + workActive + ", stop=" + stop);
        if (stop) {
            Log.i(TAG, "idle auto-stop: engine idle for " + (now - lastActivityMillis) / 1000
                    + " s in the background - stopping it");
            killProcess();
        }
    }

    /** Invoked by {@link EngineForegroundService} when "Stop" is tapped. */
    private void stopFromNotification() {
        killProcess();
        recordActivity();
    }

    /** True for a debuggable APK (assembleDebug, or a release build with `debuggable true`). */
    static boolean isDebuggable(Context ctx) {
        return (ctx.getApplicationInfo().flags & ApplicationInfo.FLAG_DEBUGGABLE) != 0;
    }

    /**
     * Filter the JS-supplied env through {@link #DEBUG_ENV_ALLOWLIST}; empty
     * (and logged) in a non-debuggable build. Also honours the
     * {@link #MOCK_SYSPROP} system property in debuggable builds.
     */
    private Map<String, String> debugEnv(JSObject requested) {
        Map<String, String> env = new HashMap<>();
        boolean debuggable = isDebuggable(getContext());
        if (requested != null) {
            Iterator<String> keys = requested.keys();
            while (keys.hasNext()) {
                String key = keys.next();
                if (!DEBUG_ENV_ALLOWLIST.contains(key)) {
                    Log.w(TAG, "ignoring engine env " + key + " (not allow-listed)");
                    continue;
                }
                if (!debuggable) {
                    Log.w(TAG, "ignoring engine env " + key + " (release build)");
                    continue;
                }
                String value = requested.getString(key);
                if (value != null && !value.isEmpty()) {
                    env.put(key, value);
                }
            }
        }
        if (debuggable && !env.containsKey("BEBOK_PROVIDER_MOCK")) {
            String prop = readSystemProperty(MOCK_SYSPROP);
            if ("1".equals(prop) || "true".equalsIgnoreCase(prop)) {
                env.put("BEBOK_PROVIDER_MOCK", "1");
            }
        }
        if (!env.isEmpty()) {
            Log.w(TAG, "debug engine env: " + env.keySet());
        }
        return env;
    }

    /** `getprop <name>` (empty string when unset or unavailable). */
    private static String readSystemProperty(String name) {
        try {
            Process p = new ProcessBuilder("getprop", name).redirectErrorStream(true).start();
            try (BufferedReader r = new BufferedReader(new InputStreamReader(p.getInputStream()))) {
                String line = r.readLine();
                return line == null ? "" : line.trim();
            }
        } catch (Exception e) {
            return "";
        }
    }

    private String launch(Map<String, String> extraEnv) throws Exception {
        Context ctx = getContext();
        File workDir = ctx.getFilesDir();

        File server = resolveNativeBinary(ctx, SERVER_LIB);
        if (server == null) {
            throw new Exception(
                    "embedded engine not installed: " + SERVER_LIB + " missing from "
                            + nativeLibraryDir(ctx)
                            + " (device ABIs " + String.join(", ", Build.SUPPORTED_ABIS)
                            + "; was the APK built with scripts/bundle-android.sh?)");
        }
        // mksh is best-effort (bundle-android.sh may skip it on a Windows host).
        File shell = resolveNativeBinary(ctx, SHELL_LIB);
        Log.i(TAG, "launching " + server.getAbsolutePath()
                + (shell != null ? " with shell " + shell.getAbsolutePath() : " (no bundled shell)"));

        ProcessBuilder pb = new ProcessBuilder(server.getAbsolutePath(), "--port", "0");
        pb.directory(workDir);
        if (shell != null) {
            pb.environment().put("BEBOK_SHELL", shell.getAbsolutePath());
        }
        pb.environment().put("HOME", workDir.getAbsolutePath());
        pb.environment().put("TERM", "xterm-256color");
        pb.environment().putAll(extraEnv);
        pb.redirectErrorStream(false);

        process = pb.start();

        // Drain stderr so the child never blocks on a full pipe.
        Thread stderrDrainer = new Thread(() -> {
            try (InputStream err = process.getErrorStream()) {
                byte[] buf = new byte[4096];
                while (err.read(buf) >= 0) {
                    // discard (engine logs)
                }
            } catch (Exception ignored) {
            }
        });
        stderrDrainer.setDaemon(true);
        stderrDrainer.start();

        // Capture BEBOK_READY from stdout.
        final String[] urlHolder = new String[1];
        Thread reader = new Thread(() -> {
            try (BufferedReader r = new BufferedReader(new InputStreamReader(process.getInputStream()))) {
                String line;
                while ((line = r.readLine()) != null) {
                    if (line.startsWith(READY_PREFIX)) {
                        synchronized (urlHolder) {
                            urlHolder[0] = line.substring(READY_PREFIX.length()).trim();
                            urlHolder.notifyAll();
                        }
                        break;
                    }
                }
            } catch (Exception ignored) {
            }
        });
        reader.setDaemon(true);
        reader.start();

        synchronized (urlHolder) {
            long deadline = System.currentTimeMillis() + 20000;
            while (urlHolder[0] == null && System.currentTimeMillis() < deadline) {
                try {
                    urlHolder.wait(200);
                } catch (InterruptedException e) {
                    break;
                }
            }
        }

        if (urlHolder[0] == null) {
            killProcess();
            throw new Exception("engine did not announce BEBOK_READY");
        }
        // Without the `?token=` query: logcat must never carry the launch token.
        Log.i(TAG, "BEBOK_READY " + urlHolder[0].replaceAll("\\?.*$", ""));
        return urlHolder[0];
    }

    /** {@code ApplicationInfo.nativeLibraryDir}: where the installer extracted lib/&lt;abi&gt;/lib*.so (F10-29). */
    static String nativeLibraryDir(Context ctx) {
        return ctx.getApplicationInfo().nativeLibraryDir;
    }

    /**
     * F10-29: the engine (and mksh) are packaged as {@code lib/<abi>/lib*.so}
     * and extracted by the installer into {@link ApplicationInfo#nativeLibraryDir}
     * ({@code useLegacyPackaging} in build.gradle). That directory is the only
     * place an untrusted app may exec a file from on Android 10+ - copying the
     * binary into {@code files/} (the pre-F10-29 approach) fails with
     * {@code error=13, Permission denied} (W^X). The ABI was already chosen by
     * the installer, so there is nothing to resolve here beyond existence.
     *
     * @return the binary, or {@code null} when it is not installed.
     */
    static File resolveNativeBinary(Context ctx, String libName) {
        File lib = new File(nativeLibraryDir(ctx), libName);
        if (!lib.isFile() || lib.length() == 0) {
            return null;
        }
        if (!lib.canExecute() && !lib.setExecutable(true)) {
            // The installer marks extracted libs 0755; log, but still try to
            // exec - canExecute() is only advisory here.
            Log.w(TAG, libName + " is not marked executable in " + lib.getParent());
        }
        return lib;
    }

    private void killProcess() {
        if (process != null) {
            process.destroy();
            process = null;
        }
        baseUrl = null;
    }

    @Override
    protected void handleOnDestroy() {
        killProcess();
        if (idleTask != null) {
            idleTask.cancel(true);
        }
        if (idleExecutor != null) {
            idleExecutor.shutdownNow();
        }
        EngineForegroundService.setStopCallback(null);
        super.handleOnDestroy();
    }
}
