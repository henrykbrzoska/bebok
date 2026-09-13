package dev.bebok.mobile;

import android.content.Context;
import android.content.Intent;
import android.content.res.AssetManager;
import android.os.Build;
import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.util.HashSet;
import java.util.Set;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;

/**
 * Launches the embedded bebok-server engine (bundled per-ABI in
 * assets/bin/&lt;abi&gt;/, F10-9) on Android. The engine binds 127.0.0.1 with a
 * random port and prints `BEBOK_READY http://host:port` on stdout; we capture
 * that and hand the URL back to the webview so it can talk to the local
 * engine (same as the desktop sidecar, but in-process within the app).
 *
 * The bundled shell (mksh) is extracted next to the engine and its path is set
 * via `BEBOK_SHELL`, so the engine's `bash` tool and (if enabled) PTY work.
 *
 * F10-10 adds a reference-counted foreground service for the duration of a
 * turn ({@link #beginWork}/{@link #endWork}) and an idle auto-stop timer that
 * kills the engine after {@link IdleAutoStopPolicy#DEFAULT_IDLE_THRESHOLD_MILLIS}
 * of inactivity while the app is backgrounded.
 */
@CapacitorPlugin(name = "EngineLauncher")
public class EngineLauncherPlugin extends Plugin {

    private static final String READY_PREFIX = "BEBOK_READY ";
    private static final long IDLE_CHECK_PERIOD_SECONDS = 60;

    private Process process;
    private volatile String baseUrl;

    private final WorkCounter workCounter = new WorkCounter();
    private volatile long lastActivityMillis = System.currentTimeMillis();
    private volatile boolean appInForeground = true;

    private ScheduledExecutorService idleExecutor;
    private ScheduledFuture<?> idleTask;

    @Override
    public void load() {
        super.load();
        EngineForegroundService.setStopCallback(this::stopFromNotification);
        idleExecutor = Executors.newSingleThreadScheduledExecutor();
        idleTask = idleExecutor.scheduleWithFixedDelay(
                this::checkIdle, IDLE_CHECK_PERIOD_SECONDS, IDLE_CHECK_PERIOD_SECONDS, TimeUnit.SECONDS);
    }

    @PluginMethod
    public void start(PluginCall call) {
        try {
            if (baseUrl == null) {
                baseUrl = launch();
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
        boolean stop = IdleAutoStopPolicy.shouldStop(
                lastActivityMillis,
                System.currentTimeMillis(),
                appInForeground,
                workCounter.isActive(),
                IdleAutoStopPolicy.DEFAULT_IDLE_THRESHOLD_MILLIS);
        if (stop) {
            killProcess();
        }
    }

    /** Invoked by {@link EngineForegroundService} when "Stop" is tapped. */
    private void stopFromNotification() {
        killProcess();
        recordActivity();
    }

    private String launch() throws Exception {
        Context ctx = getContext();
        File binDir = new File(ctx.getFilesDir(), "bin");
        if (!binDir.exists() && !binDir.mkdirs()) {
            throw new Exception("failed to create bin dir");
        }

        String abi = resolveAbi(ctx);
        String assetPrefix = "bin/" + abi + "/";

        File server = extract(ctx, assetPrefix + "bebok-server", new File(binDir, "bebok-server"));
        File shell = extractOptional(ctx, assetPrefix + "mksh", new File(binDir, "mksh"));
        if (!server.setExecutable(true)) {
            throw new Exception("failed to chmod +x bebok-server");
        }
        if (shell != null && !shell.setExecutable(true)) {
            throw new Exception("failed to chmod +x mksh");
        }

        ProcessBuilder pb = new ProcessBuilder(server.getAbsolutePath(), "--port", "0");
        pb.directory(binDir);
        if (shell != null) {
            pb.environment().put("BEBOK_SHELL", shell.getAbsolutePath());
        }
        pb.environment().put("HOME", ctx.getFilesDir().getAbsolutePath());
        pb.environment().put("TERM", "xterm-256color");
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
        return urlHolder[0];
    }

    /**
     * Picks the bundled ABI directory matching this device (F10-9). Fails
     * with a clear message when the APK does not bundle any ABI the device
     * supports (e.g. an x86 32-bit device, or an x86_64-only debug build
     * installed on an arm64 device).
     */
    private String resolveAbi(Context ctx) throws Exception {
        Set<String> bundled = listBundledAbis(ctx);
        String abi = AbiAssetResolver.resolve(Build.SUPPORTED_ABIS, bundled);
        if (abi == null) {
            throw new Exception(
                    "no bundled engine ABI matches this device (device supports "
                            + String.join(", ", Build.SUPPORTED_ABIS)
                            + "; APK bundles "
                            + String.join(", ", bundled)
                            + ")");
        }
        return abi;
    }

    private Set<String> listBundledAbis(Context ctx) {
        Set<String> abis = new HashSet<>();
        try {
            String[] entries = ctx.getAssets().list("bin");
            if (entries != null) {
                for (String entry : entries) {
                    // Only directories (an ABI name) count; a flat legacy
                    // `bin/bebok-server` layout is not supported post F10-9.
                    String[] children = ctx.getAssets().list("bin/" + entry);
                    if (children != null && children.length > 0) {
                        abis.add(entry);
                    }
                }
            }
        } catch (Exception ignored) {
        }
        return abis;
    }

    /** Copy an asset to the files dir once (idempotent). */
    private File extract(Context ctx, String assetPath, File dest) throws Exception {
        if (dest.exists() && dest.length() > 0) {
            return dest;
        }
        AssetManager am = ctx.getAssets();
        try (InputStream in = am.open(assetPath);
             FileOutputStream out = new FileOutputStream(dest)) {
            byte[] buf = new byte[8192];
            int n;
            while ((n = in.read(buf)) > 0) {
                out.write(buf, 0, n);
            }
        }
        return dest;
    }

    /** Like {@link #extract}, but returns null instead of throwing when the asset is absent (mksh is best-effort). */
    private File extractOptional(Context ctx, String assetPath, File dest) {
        try {
            return extract(ctx, assetPath, dest);
        } catch (Exception e) {
            return null;
        }
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
