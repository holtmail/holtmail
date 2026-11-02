use argon2::{Algorithm, Argon2, Params, PasswordHasher, PasswordVerifier, Version};

use crate::AuthError;

/// Fewest characters a password may have.
pub const MIN_PASSWORD_CHARS: usize = 10;

/// Most bytes a password may have, so hashing stays bounded.
pub const MAX_PASSWORD_BYTES: usize = 1024;

/// How expensive a new Argon2id hash is. Stored hashes keep the cost they were made with, so
/// changing it affects only hashes made afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HashCost {
    /// Memory in KiB.
    pub memory_kib: u32,
    /// Passes over the memory.
    pub iterations: u32,
    /// Degree of parallelism.
    pub lanes: u32,
}

impl Default for HashCost {
    /// 19 MiB, 2 passes, 1 lane: the OWASP recommendation for Argon2id.
    fn default() -> Self {
        Self {
            memory_kib: 19 * 1024,
            iterations: 2,
            lanes: 1,
        }
    }
}

/// Hashes and checks passwords with Argon2id, as PHC strings.
#[derive(Debug, Clone)]
pub struct Passwords {
    params: Params,
}

impl Passwords {
    /// Hashing at `cost`. Fails with `Invalid` when the cost is out of Argon2's range.
    pub fn new(cost: HashCost) -> Result<Self, AuthError> {
        let params = Params::new(cost.memory_kib, cost.iterations, cost.lanes, None)
            .map_err(|e| AuthError::Invalid(format!("password hash cost: {e}")))?;
        Ok(Self { params })
    }

    fn argon(&self) -> Argon2<'static> {
        Argon2::new(Algorithm::Argon2id, Version::V0x13, self.params.clone())
    }

    /// A PHC string for `password` with a fresh random salt.
    pub fn hash(&self, password: &str) -> Result<String, AuthError> {
        self.argon()
            .hash_password(password.as_bytes())
            .map(|hash| hash.to_string())
            .map_err(|_| AuthError::Hashing)
    }

    /// Whether `password` matches the PHC string `hash`, compared in constant time. A malformed
    /// hash matches nothing.
    pub fn verify(&self, password: &str, hash: &str) -> bool {
        self.argon()
            .verify_password(password.as_bytes(), hash)
            .is_ok()
    }
}

/// Checks a new password: at least [`MIN_PASSWORD_CHARS`] characters and at most
/// [`MAX_PASSWORD_BYTES`] bytes.
pub fn validate_password(password: &str) -> Result<(), AuthError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(AuthError::Invalid(format!(
            "the password needs at least {MIN_PASSWORD_CHARS} characters"
        )));
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(AuthError::Invalid("the password is too long".into()));
    }
    Ok(())
}

/// Checks and trims an email: one `@` with text on both sides, no spaces, at most 254 bytes.
pub fn normalize_email(email: &str) -> Result<String, AuthError> {
    let email = email.trim();
    let valid = email.len() <= 254
        && !email.chars().any(char::is_whitespace)
        && email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && !domain.is_empty() && !domain.contains('@')
        });
    if valid {
        Ok(email.to_owned())
    } else {
        Err(AuthError::Invalid("not a valid email address".into()))
    }
}

/// Checks and trims a display name: not empty, at most 100 characters.
pub fn normalize_name(name: &str) -> Result<String, AuthError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(AuthError::Invalid(
            "the name must have between 1 and 100 characters".into(),
        ));
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cheap() -> Passwords {
        Passwords::new(HashCost {
            memory_kib: 64,
            iterations: 1,
            lanes: 1,
        })
        .unwrap()
    }

    #[test]
    fn hashes_verify_and_differ_by_salt() {
        let passwords = cheap();
        let first = passwords.hash("correct horse battery").unwrap();
        let second = passwords.hash("correct horse battery").unwrap();
        assert!(first.starts_with("$argon2id$v=19$m=64,t=1,p=1$"), "{first}");
        assert_ne!(first, second);
        assert!(passwords.verify("correct horse battery", &first));
        assert!(!passwords.verify("correct horse batterY", &first));
        assert!(!passwords.verify("anything", "not a hash"));
    }

    #[test]
    fn verifies_hashes_made_at_another_cost() {
        let old = cheap().hash("correct horse battery").unwrap();
        let current = Passwords::new(HashCost {
            memory_kib: 128,
            iterations: 2,
            lanes: 1,
        })
        .unwrap();
        assert!(current.verify("correct horse battery", &old));
    }

    #[test]
    fn default_cost_follows_owasp() {
        let hash = Passwords::new(HashCost::default())
            .unwrap()
            .hash("correct horse battery")
            .unwrap();
        assert!(
            hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{hash}"
        );
    }

    #[test]
    fn validates_input() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("ten chars!").is_ok());
        assert!(validate_password(&"x".repeat(MAX_PASSWORD_BYTES + 1)).is_err());
        assert_eq!(
            normalize_email("  Ada@Example.com ").unwrap(),
            "Ada@Example.com"
        );
        for bad in [
            "",
            "ada",
            "@example.com",
            "ada@",
            "a b@example.com",
            "a@b@c",
        ] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
        assert_eq!(normalize_name(" Ada ").unwrap(), "Ada");
        assert!(normalize_name("  ").is_err());
    }
}
