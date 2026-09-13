package dev.bebok.mobile;

import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.security.SecureRandom;

import javax.crypto.Cipher;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * AES/GCM sealing used by {@link SecureStorePlugin} (WP-M5 / F10-20).
 *
 * Pure JVM code (no Android APIs) so it can be unit-tested with an ordinary
 * {@code javax.crypto} key; the plugin hands it the {@code AndroidKeyStore}
 * key at runtime and base64-encodes the record for SharedPreferences. Wire
 * format: {@code iv[12] || ciphertext || tag[16]}, one random 96-bit IV per
 * {@link #seal} call.
 */
public final class SecureStoreCipher {

    static final String TRANSFORMATION = "AES/GCM/NoPadding";
    static final int IV_BYTES = 12;
    static final int TAG_BITS = 128;

    private static final SecureRandom RANDOM = new SecureRandom();

    private SecureStoreCipher() {
    }

    /** Encrypt {@code plaintext} (UTF-8) under {@code key}. */
    public static byte[] seal(SecretKey key, String plaintext) throws GeneralSecurityException {
        byte[] iv = new byte[IV_BYTES];
        RANDOM.nextBytes(iv);
        Cipher cipher = Cipher.getInstance(TRANSFORMATION);
        cipher.init(Cipher.ENCRYPT_MODE, key, new GCMParameterSpec(TAG_BITS, iv));
        byte[] sealed = cipher.doFinal(plaintext.getBytes(StandardCharsets.UTF_8));
        byte[] record = new byte[iv.length + sealed.length];
        System.arraycopy(iv, 0, record, 0, iv.length);
        System.arraycopy(sealed, 0, record, iv.length, sealed.length);
        return record;
    }

    /**
     * Decrypt a record produced by {@link #seal}. Throws on a wrong key, a
     * truncated record or any tampering (GCM tag mismatch).
     */
    public static String open(SecretKey key, byte[] record) throws GeneralSecurityException {
        if (record == null || record.length <= IV_BYTES) {
            throw new GeneralSecurityException("secure store record too short");
        }
        Cipher cipher = Cipher.getInstance(TRANSFORMATION);
        cipher.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(TAG_BITS, record, 0, IV_BYTES));
        byte[] plain = cipher.doFinal(record, IV_BYTES, record.length - IV_BYTES);
        return new String(plain, StandardCharsets.UTF_8);
    }
}
