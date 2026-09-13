package dev.bebok.mobile;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import com.getcapacitor.JSObject;
import com.getcapacitor.Plugin;
import com.getcapacitor.PluginCall;
import com.getcapacitor.PluginMethod;
import com.getcapacitor.annotation.CapacitorPlugin;

import java.security.KeyStore;

import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;

/**
 * SecureStore (WP-M5 / F10-20): small key/value store for device secrets
 * (pairing tokens, later the push key) sealed with one AES-256/GCM key that
 * lives in {@code AndroidKeyStore} and never leaves it.
 *
 * Values are stored as {@code base64(iv || ciphertext || tag)} in the private
 * {@link SharedPreferences} file {@value #PREFS}; the JS side
 * ({@code core/secure-store.ts}) implements {@code TargetSecrets} on top of
 * {@link #get}/{@link #set}/{@link #remove}. A record that no longer opens
 * (key wiped by a factory reset / uninstall, tampering) reads as absent.
 */
@CapacitorPlugin(name = "SecureStore")
public class SecureStorePlugin extends Plugin {

    static final String PREFS = "bebok_secure";
    static final String KEY_ALIAS = "bebok.secure-store.v1";
    private static final String ANDROID_KEYSTORE = "AndroidKeyStore";

    private SecretKey key;

    @PluginMethod
    public void get(PluginCall call) {
        String name = call.getString("key");
        if (name == null || name.isEmpty()) {
            call.reject("key is required");
            return;
        }
        try {
            String record = prefs().getString(name, null);
            JSObject ret = new JSObject();
            if (record == null) {
                ret.put("value", JSObject.NULL);
            } else {
                try {
                    ret.put("value", SecureStoreCipher.open(key(), Base64.decode(record, Base64.NO_WRAP)));
                } catch (Exception unreadable) {
                    // Wrong/rotated key or corrupt record: treat as absent
                    // rather than surfacing ciphertext or crashing the caller.
                    ret.put("value", JSObject.NULL);
                }
            }
            call.resolve(ret);
        } catch (Exception e) {
            call.reject("secure store read failed: " + e.getMessage(), e);
        }
    }

    @PluginMethod
    public void set(PluginCall call) {
        String name = call.getString("key");
        String value = call.getString("value");
        if (name == null || name.isEmpty()) {
            call.reject("key is required");
            return;
        }
        if (value == null) {
            call.reject("value is required");
            return;
        }
        try {
            byte[] record = SecureStoreCipher.seal(key(), value);
            prefs().edit().putString(name, Base64.encodeToString(record, Base64.NO_WRAP)).apply();
            call.resolve();
        } catch (Exception e) {
            call.reject("secure store write failed: " + e.getMessage(), e);
        }
    }

    @PluginMethod
    public void remove(PluginCall call) {
        String name = call.getString("key");
        if (name == null || name.isEmpty()) {
            call.reject("key is required");
            return;
        }
        prefs().edit().remove(name).apply();
        call.resolve();
    }

    private SharedPreferences prefs() {
        return getContext().getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /** The Keystore key, generated on first use (AES-256, GCM, no user auth). */
    private synchronized SecretKey key() throws Exception {
        if (key != null) {
            return key;
        }
        KeyStore store = KeyStore.getInstance(ANDROID_KEYSTORE);
        store.load(null);
        KeyStore.Entry entry = store.getEntry(KEY_ALIAS, null);
        if (entry instanceof KeyStore.SecretKeyEntry) {
            key = ((KeyStore.SecretKeyEntry) entry).getSecretKey();
            return key;
        }
        KeyGenerator generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, ANDROID_KEYSTORE);
        generator.init(new KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                // We supply our own random IV per record (SecureStoreCipher).
                .setRandomizedEncryptionRequired(false)
                .build());
        key = generator.generateKey();
        return key;
    }
}
