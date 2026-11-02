use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use holtmail_store::SessionHash;

use crate::AuthError;

const TOKEN_BYTES: usize = 32;

/// 32 random bytes as unpadded base64url (43 characters).
pub fn random_token() -> Result<String, AuthError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|_| AuthError::Random)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The BLAKE3 hash a session token is stored as; `None` for a string that is not a token.
pub fn token_hash(token: &str) -> Option<SessionHash> {
    let mut bytes = [0u8; TOKEN_BYTES];
    match URL_SAFE_NO_PAD.decode_slice(token.as_bytes(), &mut bytes) {
        Ok(TOKEN_BYTES) if token.len() == 43 => Some(*blake3::hash(&bytes).as_bytes()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hash_stably() {
        let first = random_token().unwrap();
        let second = random_token().unwrap();
        assert_eq!(first.len(), 43);
        assert_ne!(first, second);
        assert_eq!(token_hash(&first), token_hash(&first));
        assert_ne!(token_hash(&first), token_hash(&second));
    }

    #[test]
    fn rejects_what_is_not_a_token() {
        assert_eq!(token_hash(""), None);
        assert_eq!(token_hash("short"), None);
        assert_eq!(token_hash(&"A".repeat(44)), None);
        assert_eq!(token_hash(&"!".repeat(43)), None);
    }
}
