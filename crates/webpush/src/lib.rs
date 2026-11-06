//! Receiving Web Push messages: a subscriber's keys and the decryption of a message encrypted
//! for them as RFC 8291 says (`aes128gcm` content coding, RFC 8188), plus encryption for tests
//! and senders. No I/O.

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{ECDH_P256, PrivateKey, UnparsedPublicKey, agree};
use aws_lc_rs::encoding::{AsBigEndian, EcPrivateKeyBin};
use aws_lc_rs::hmac;

const SALT: usize = 16;
const PUBLIC: usize = 65;
const AUTH: usize = 16;
const TAG: usize = 16;
const HEADER: usize = SALT + 4 + 1 + PUBLIC;
const RECORD_SIZE: u32 = 4096;

/// Why a message could not be read or written.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum WebPushError {
    /// The message is not an `aes128gcm` body for these keys, or it was altered.
    #[error("the push message cannot be decrypted")]
    Undecryptable,
    /// A key or secret has the wrong length or is not a P-256 point.
    #[error("a push key is invalid")]
    InvalidKey,
    /// The system's random source failed.
    #[error("no random bytes")]
    Random,
}

/// A subscriber's P-256 key pair and auth secret. `Debug` shows neither secret.
#[derive(Clone)]
pub struct Keys {
    private: [u8; 32],
    public: [u8; PUBLIC],
    auth: [u8; AUTH],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keys(..)")
    }
}

impl Keys {
    /// New random keys.
    pub fn generate() -> Result<Self, WebPushError> {
        let private = PrivateKey::generate(&ECDH_P256).map_err(|_| WebPushError::Random)?;
        let mut auth = [0u8; AUTH];
        aws_lc_rs::rand::fill(&mut auth).map_err(|_| WebPushError::Random)?;
        Self::from_private(&private, auth)
    }

    /// Keys from a private key (a 32-byte big-endian scalar) and an auth secret.
    pub fn from_parts(private: &[u8], auth: &[u8]) -> Result<Self, WebPushError> {
        let private = PrivateKey::from_private_key(&ECDH_P256, private)
            .map_err(|_| WebPushError::InvalidKey)?;
        Self::from_private(
            &private,
            auth.try_into().map_err(|_| WebPushError::InvalidKey)?,
        )
    }

    fn from_private(private: &PrivateKey, auth: [u8; AUTH]) -> Result<Self, WebPushError> {
        let raw: EcPrivateKeyBin<'_> = private
            .as_be_bytes()
            .map_err(|_| WebPushError::InvalidKey)?;
        let public = private
            .compute_public_key()
            .map_err(|_| WebPushError::InvalidKey)?;
        Ok(Self {
            private: raw
                .as_ref()
                .try_into()
                .map_err(|_| WebPushError::InvalidKey)?,
            public: public
                .as_ref()
                .try_into()
                .map_err(|_| WebPushError::InvalidKey)?,
            auth,
        })
    }

    /// The public key, uncompressed (65 bytes), to hand to the sender.
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// The auth secret, to hand to the sender.
    pub fn auth_secret(&self) -> &[u8] {
        &self.auth
    }

    /// The plaintext of `body`, an `aes128gcm` message in one record encrypted for these keys.
    /// `Undecryptable` for anything else, including a message that was changed on the way.
    pub fn decrypt(&self, body: &[u8]) -> Result<Vec<u8>, WebPushError> {
        if body.len() < HEADER + TAG + 1 {
            return Err(WebPushError::Undecryptable);
        }
        let salt = &body[..SALT];
        let record_size = u32::from_be_bytes(body[SALT..SALT + 4].try_into().unwrap_or_default());
        if usize::from(body[SALT + 4]) != PUBLIC {
            return Err(WebPushError::Undecryptable);
        }
        let sender = &body[SALT + 5..HEADER];
        let sealed = &body[HEADER..];
        if record_size < (TAG + 2) as u32 || sealed.len() > record_size as usize {
            return Err(WebPushError::Undecryptable);
        }
        let private = PrivateKey::from_private_key(&ECDH_P256, &self.private)
            .map_err(|_| WebPushError::InvalidKey)?;
        let ecdh = agree(
            &private,
            UnparsedPublicKey::new(&ECDH_P256, sender),
            WebPushError::Undecryptable,
            |secret| Ok(secret.to_vec()),
        )?;
        let (cek, nonce) = derive(&ecdh, &self.auth, &self.public, sender, salt);
        let key = LessSafeKey::new(
            UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| WebPushError::Undecryptable)?,
        );
        let mut buffer = sealed.to_vec();
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::empty(),
                &mut buffer,
            )
            .map_err(|_| WebPushError::Undecryptable)?;
        let end = plain
            .iter()
            .rposition(|&b| b != 0)
            .ok_or(WebPushError::Undecryptable)?;
        if plain[end] != 2 {
            return Err(WebPushError::Undecryptable);
        }
        Ok(plain[..end].to_vec())
    }
}

/// A random 128-bit id in lowercase hex, for naming a subscriber's push address so it cannot
/// be guessed.
pub fn random_id() -> Result<String, WebPushError> {
    let mut bytes = [0u8; 16];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| WebPushError::Random)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `plaintext` encrypted for the subscriber's public key and auth secret, as one `aes128gcm`
/// record with a fresh sender key and salt.
pub fn encrypt(
    public_key: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, WebPushError> {
    let sender = PrivateKey::generate(&ECDH_P256).map_err(|_| WebPushError::Random)?;
    let mut salt = [0u8; SALT];
    aws_lc_rs::rand::fill(&mut salt).map_err(|_| WebPushError::Random)?;
    encrypt_with(&sender, &salt, public_key, auth_secret, plaintext)
}

fn encrypt_with(
    sender: &PrivateKey,
    salt: &[u8; SALT],
    public_key: &[u8],
    auth_secret: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, WebPushError> {
    if public_key.len() != PUBLIC
        || auth_secret.len() != AUTH
        || plaintext.len() + 1 + TAG > RECORD_SIZE as usize
    {
        return Err(WebPushError::InvalidKey);
    }
    let sender_public = sender
        .compute_public_key()
        .map_err(|_| WebPushError::InvalidKey)?;
    let ecdh = agree(
        sender,
        UnparsedPublicKey::new(&ECDH_P256, public_key),
        WebPushError::InvalidKey,
        |secret| Ok(secret.to_vec()),
    )?;
    let (cek, nonce) = derive(&ecdh, auth_secret, public_key, sender_public.as_ref(), salt);
    let key = LessSafeKey::new(
        UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| WebPushError::InvalidKey)?,
    );
    let mut record = plaintext.to_vec();
    record.push(2);
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce),
        Aad::empty(),
        &mut record,
    )
    .map_err(|_| WebPushError::InvalidKey)?;
    let mut out = Vec::with_capacity(HEADER + record.len());
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(PUBLIC as u8);
    out.extend_from_slice(sender_public.as_ref());
    out.extend_from_slice(&record);
    Ok(out)
}

fn hmac_sha256(key: &[u8], data: &[&[u8]]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let mut context = hmac::Context::with_key(&key);
    for part in data {
        context.update(part);
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(context.sign().as_ref());
    out
}

fn derive(
    ecdh: &[u8],
    auth: &[u8],
    receiver: &[u8],
    sender: &[u8],
    salt: &[u8],
) -> ([u8; 16], [u8; 12]) {
    let prk_key = hmac_sha256(auth, &[ecdh]);
    let ikm = hmac_sha256(&prk_key, &[b"WebPush: info\0", receiver, sender, &[1]]);
    let prk = hmac_sha256(salt, &[&ikm]);
    let cek = hmac_sha256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]]);
    let nonce = hmac_sha256(&prk, &[b"Content-Encoding: nonce\0", &[1]]);
    let mut out_cek = [0u8; 16];
    out_cek.copy_from_slice(&cek[..16]);
    let mut out_nonce = [0u8; 12];
    out_nonce.copy_from_slice(&nonce[..12]);
    (out_cek, out_nonce)
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    use super::*;

    fn b64(text: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD
            .decode(text.split_whitespace().collect::<String>())
            .unwrap()
    }

    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const UA_PUBLIC: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AUTH_SECRET: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SALT_B64: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24";
    const HEADER_B64: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8";
    const CIPHERTEXT: &str =
        "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrd irN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs bI_0LpXMuGvnzQ";

    fn rfc_message() -> Vec<u8> {
        let mut body = b64(HEADER_B64);
        body.extend(b64(CIPHERTEXT));
        body
    }

    #[test]
    fn decrypts_the_rfc_8291_example() {
        let keys = Keys::from_parts(&b64(UA_PRIVATE), &b64(AUTH_SECRET)).unwrap();
        assert_eq!(keys.public_key(), b64(UA_PUBLIC).as_slice());
        assert_eq!(keys.decrypt(&rfc_message()).unwrap(), b64(PLAINTEXT));
    }

    #[test]
    fn encrypts_the_rfc_8291_example() {
        let sender = PrivateKey::from_private_key(&ECDH_P256, &b64(AS_PRIVATE)).unwrap();
        let salt: [u8; 16] = b64(SALT_B64).try_into().unwrap();
        let body = encrypt_with(
            &sender,
            &salt,
            &b64(UA_PUBLIC),
            &b64(AUTH_SECRET),
            &b64(PLAINTEXT),
        )
        .unwrap();
        assert_eq!(body, rfc_message());
    }

    #[test]
    fn round_trips_with_fresh_keys_and_refuses_others() {
        let keys = Keys::generate().unwrap();
        let body = encrypt(keys.public_key(), keys.auth_secret(), b"<push-message/>").unwrap();
        assert_eq!(keys.decrypt(&body).unwrap(), b"<push-message/>");

        let stranger = Keys::generate().unwrap();
        assert_eq!(stranger.decrypt(&body), Err(WebPushError::Undecryptable));
        let mut altered = body.clone();
        *altered.last_mut().unwrap() ^= 1;
        assert_eq!(keys.decrypt(&altered), Err(WebPushError::Undecryptable));
        assert_eq!(
            keys.decrypt(b"plain text"),
            Err(WebPushError::Undecryptable)
        );
        assert_eq!(format!("{keys:?}"), "Keys(..)");
    }

    #[test]
    fn random_ids_are_hex_and_differ() {
        let a = random_id().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, random_id().unwrap());
    }
}
