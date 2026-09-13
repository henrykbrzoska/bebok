package dev.bebok.mobile;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotEquals;
import static org.junit.Assert.fail;

import java.nio.charset.StandardCharsets;
import java.security.GeneralSecurityException;
import java.util.Arrays;

import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;

import org.junit.Test;

/** WP-M5 / F10-20: the SecureStore sealing round-trips and rejects tampering. */
public class SecureStoreCipherTest {

    private static SecretKey aesKey() throws Exception {
        KeyGenerator generator = KeyGenerator.getInstance("AES");
        generator.init(256);
        return generator.generateKey();
    }

    @Test
    public void roundTripsUnicodeText() throws Exception {
        SecretKey key = aesKey();
        String secret = "device-token-éè中文 " + "x".repeat(300);
        byte[] record = SecureStoreCipher.seal(key, secret);
        assertEquals(secret, SecureStoreCipher.open(key, record));
    }

    @Test
    public void recordIsNotPlaintextAndDiffersPerCall() throws Exception {
        SecretKey key = aesKey();
        String secret = "super-secret-token";
        byte[] a = SecureStoreCipher.seal(key, secret);
        byte[] b = SecureStoreCipher.seal(key, secret);
        assertFalse(Arrays.equals(a, b));
        String rendered = new String(a, StandardCharsets.ISO_8859_1);
        assertFalse(rendered.contains(secret));
        assertEquals(SecureStoreCipher.IV_BYTES + secret.length() + SecureStoreCipher.TAG_BITS / 8, a.length);
    }

    @Test
    public void wrongKeyFails() throws Exception {
        byte[] record = SecureStoreCipher.seal(aesKey(), "token");
        try {
            SecureStoreCipher.open(aesKey(), record);
            fail("expected a GCM failure");
        } catch (GeneralSecurityException expected) {
            assertNotEquals("", expected.getClass().getName());
        }
    }

    @Test
    public void tamperedRecordFails() throws Exception {
        SecretKey key = aesKey();
        byte[] record = SecureStoreCipher.seal(key, "token");
        record[record.length - 1] ^= 0x01;
        try {
            SecureStoreCipher.open(key, record);
            fail("expected a GCM tag mismatch");
        } catch (GeneralSecurityException expected) {
            // ok
        }
    }

    @Test
    public void truncatedRecordFails() throws Exception {
        SecretKey key = aesKey();
        try {
            SecureStoreCipher.open(key, new byte[SecureStoreCipher.IV_BYTES]);
            fail("expected a length check failure");
        } catch (GeneralSecurityException expected) {
            // ok
        }
    }
}
