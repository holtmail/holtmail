//! SASL `PLAIN` (RFC 4616) and `LOGIN` for `AUTHENTICATE`.

/// Where an `AUTHENTICATE` exchange stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sasl {
    /// `PLAIN`, waiting for the client's response.
    Plain,
    /// `LOGIN`, waiting for the user name.
    LoginUser,
    /// `LOGIN`, waiting for the password of this user.
    LoginPassword(String),
}

/// What the client said, or why it cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Ask for more with this challenge.
    Challenge(&'static [u8], Sasl),
    /// Verify these credentials.
    Credentials { login: String, secret: String },
    /// The response is malformed or names another authorization identity.
    Invalid,
}

/// The `LOGIN` challenge for the user name.
pub(crate) const USERNAME: &[u8] = b"Username:";
/// The `LOGIN` challenge for the password.
pub(crate) const PASSWORD: &[u8] = b"Password:";

/// Parses a `PLAIN` response: `authzid NUL authcid NUL passwd`. An authorization identity other
/// than empty or the login name is refused, so nobody signs in as somebody else.
pub(crate) fn plain(response: &[u8]) -> Step {
    let mut parts = response.split(|b| *b == 0);
    let (Some(authz), Some(authc), Some(password), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Step::Invalid;
    };
    let (Ok(authz), Ok(login), Ok(secret)) = (
        std::str::from_utf8(authz),
        std::str::from_utf8(authc),
        std::str::from_utf8(password),
    ) else {
        return Step::Invalid;
    };
    if login.is_empty() || (!authz.is_empty() && !authz.eq_ignore_ascii_case(login)) {
        return Step::Invalid;
    }
    Step::Credentials {
        login: login.into(),
        secret: secret.into(),
    }
}

impl Sasl {
    /// The next step after the client's `data`.
    pub(crate) fn step(self, data: &[u8]) -> Step {
        match self {
            Self::Plain => plain(data),
            Self::LoginUser => match std::str::from_utf8(data) {
                Ok(user) if !user.is_empty() => {
                    Step::Challenge(PASSWORD, Self::LoginPassword(user.into()))
                }
                _ => Step::Invalid,
            },
            Self::LoginPassword(login) => match std::str::from_utf8(data) {
                Ok(secret) => Step::Credentials {
                    login,
                    secret: secret.into(),
                },
                Err(_) => Step::Invalid,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_responses() {
        assert_eq!(
            plain(b"\0ada@example.com\0hm_x"),
            Step::Credentials {
                login: "ada@example.com".into(),
                secret: "hm_x".into()
            }
        );
        assert!(matches!(
            plain(b"ADA@example.com\0ada@example.com\0hm_x"),
            Step::Credentials { .. }
        ));
        assert_eq!(
            plain(b"bob@example.com\0ada@example.com\0hm_x"),
            Step::Invalid
        );
        assert_eq!(plain(b"ada@example.com\0hm_x"), Step::Invalid);
        assert_eq!(plain(b"\0\0hm_x"), Step::Invalid);
    }

    #[test]
    fn login_steps() {
        let Step::Challenge(challenge, next) = Sasl::LoginUser.step(b"ada@example.com") else {
            panic!("no password challenge");
        };
        assert_eq!(challenge, PASSWORD);
        assert_eq!(
            next.step(b"hm_x"),
            Step::Credentials {
                login: "ada@example.com".into(),
                secret: "hm_x".into()
            }
        );
    }
}
