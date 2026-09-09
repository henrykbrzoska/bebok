package dev.bebok.mobile;

import android.content.Context;
import android.content.res.AssetManager;
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

/**
 * Launches the embedded bebok-server engine (bundled in assets/bin/) on
 * Android. The engine binds 127.0.0.1 with a random port and prints
 * `BEBOK_READY http://host:port` on stdout; we capture that and hand the URL
 * back to the webview so it can talk to the local engine (same as the desktop
 * sidecar, but in-process within the app).
 *
 * The bundled shell (mksh) is extracted next to the engine and its path is set
 * via `BEBOK_SHELL`, so the engine's `bash` tool and (if enabled) PTY work.
 */
@CapacitorPlugin(name = "EngineLauncher")
public class EngineLauncherPlugin extends Plugin {

    private static final String READY_PREFIX = "BEBOK_READY ";

    private Process process;
    private volatile String baseUrl;

    @PluginMethod
    public void start(PluginCall call) {
        try {
            if (baseUrl == null) {
                baseUrl = launch();
            }
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
        baseUrl = null;
        call.resolve();
    }

    private String launch() throws Exception {
        Context ctx = getContext();
        File binDir = new File(ctx.getFilesDir(), "bin");
        if (!binDir.exists() && !binDir.mkdirs()) {
            throw new Exception("failed to create bin dir");
        }

        File server = extract(ctx, "bin/bebok-server", new File(binDir, "bebok-server"));
        File shell = extract(ctx, "bin/mksh", new File(binDir, "mksh"));
        if (!server.setExecutable(true) || !shell.setExecutable(true)) {
            throw new Exception("failed to chmod +x binaries");
        }

        ProcessBuilder pb = new ProcessBuilder(server.getAbsolutePath(), "--port", "0");
        pb.directory(binDir);
        pb.environment().put("BEBOK_SHELL", shell.getAbsolutePath());
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

    private void killProcess() {
        if (process != null) {
            process.destroy();
            process = null;
        }
    }

    @Override
    protected void handleOnDestroy() {
        killProcess();
        super.handleOnDestroy();
    }
}
