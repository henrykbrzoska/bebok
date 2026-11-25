use sha2::{Digest, Sha256};

pub fn fingerprint(salt: &[u8], domain: &str, secret: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(domain.as_bytes());
    h.update(secret.as_bytes());
    h.finalize().to_vec()
}

pub fn make_salt() -> Vec<u8> {
    let a = uuid::Uuid::new_v4().as_bytes().to_vec();
    let b = uuid::Uuid::new_v4().as_bytes().to_vec();
    [a, b].concat()
}

pub fn last4(secret: &str) -> String {
    let s: String = secret.chars().filter(|c| !c.is_whitespace()).collect();
    if s.len() <= 4 {
        s
    } else {
        s[s.len() - 4..].to_string()
    }
}

pub fn hash_algo() -> &'static str {
    "sha256"
}

pub fn resolve_secret(_provider: &str, api_key: Option<String>) -> Option<String> {
    // One-liner hook: if DB has sealed secret, decrypt; otherwise use api_key/env.
    // For now, prefer api_key, fallback to env via caller; DB lookup is optional.
    // This keeps provider.rs to one line change.
    if let Some(k) = api_key.filter(|s| !s.trim().is_empty()) {
        return Some(k);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprint_deterministic() {
        let salt = vec![1u8; 32];
        let a = fingerprint(&salt, "openai", "sk-123");
        let b = fingerprint(&salt, "openai", "sk-123");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
    }
    #[test]
    fn last4_works() {
        assert_eq!(last4("sk-123456"), "3456");
        assert_eq!(last4("ab"), "ab");
    }
    #[test]
    fn salt_len() {
        assert_eq!(make_salt().len(), 32);
    }
}
