use crate::OAuthError;

/// What the provider sent the browser back with.
#[derive(Clone, PartialEq, Eq)]
pub struct Redirect {
    /// The one-time code to exchange.
    pub code: String,
    /// The `state` of the sign-in it answers.
    pub state: String,
}

impl std::fmt::Debug for Redirect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Redirect").finish_non_exhaustive()
    }
}

/// Reads `code` and `state` from the address the browser landed on, as pasted: a full address,
/// or only its query (`?code=…&state=…` or `code=…&state=…`). Surrounding spaces, quotes and
/// angle brackets are ignored. An answer with `error` is [`OAuthError::Denied`] with the
/// provider's description; anything else without both values is [`OAuthError::Redirect`].
pub fn read_redirect(pasted: &str) -> Result<Redirect, OAuthError> {
    let text = pasted
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>')
        .trim();
    if text.is_empty() {
        return Err(OAuthError::Redirect("nothing was pasted".into()));
    }
    let query = match url::Url::parse(text) {
        Ok(url) => {
            let mut query = url.query().unwrap_or_default().to_owned();
            if let Some(fragment) = url.fragment().filter(|f| f.contains('=')) {
                query.push('&');
                query.push_str(fragment);
            }
            query
        }
        Err(_) => match text.split_once('?') {
            Some((_, query)) => query.to_owned(),
            None if text.contains('=') => text.to_owned(),
            None => return Err(OAuthError::Redirect("this is not an address".into())),
        },
    };
    let mut code = None;
    let mut state = None;
    let mut error = None;
    let mut description = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let value = value.trim().to_owned();
        match key.as_ref() {
            "code" if !value.is_empty() => code = Some(value),
            "state" if !value.is_empty() => state = Some(value),
            "error" if !value.is_empty() => error = Some(value),
            "error_description" if !value.is_empty() => description = Some(value),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Err(OAuthError::Denied(if error == "access_denied" {
            "access was refused at the sign-in page".into()
        } else {
            description.unwrap_or(error)
        }));
    }
    match (code, state) {
        (Some(code), Some(state)) => Ok(Redirect { code, state }),
        (None, _) => Err(OAuthError::Redirect("the address has no code".into())),
        (_, None) => Err(OAuthError::Redirect("the address has no state".into())),
    }
}
