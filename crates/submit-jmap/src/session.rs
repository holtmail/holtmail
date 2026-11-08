use holtmail_submit::SubmitError;
use serde_json::Value;
use url::Url;

pub(crate) const CORE: &str = "urn:ietf:params:jmap:core";
pub(crate) const MAIL: &str = "urn:ietf:params:jmap:mail";
pub(crate) const SUBMISSION: &str = "urn:ietf:params:jmap:submission";

pub(crate) struct Session {
    pub(crate) api_url: String,
    pub(crate) upload_url: String,
    pub(crate) account: String,
    pub(crate) max_size: Option<u64>,
}

fn malformed(what: &str) -> SubmitError {
    SubmitError::Unavailable(what.to_owned())
}

pub(crate) fn pointer_key(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

impl Session {
    pub(crate) fn parse(base: &Url, session: &Value) -> Result<Self, SubmitError> {
        let capabilities = session
            .get("capabilities")
            .and_then(Value::as_object)
            .ok_or_else(|| malformed("the address does not answer with a JMAP session"))?;
        if !capabilities.contains_key(MAIL) || !capabilities.contains_key(SUBMISSION) {
            return Err(SubmitError::Invalid(
                "the JMAP server does not offer mail submission".into(),
            ));
        }
        let account = [SUBMISSION, MAIL]
            .iter()
            .find_map(|c| {
                session
                    .pointer(&format!("/primaryAccounts/{}", pointer_key(c)))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| {
                SubmitError::Invalid("the JMAP session has no mail account for this user".into())
            })?
            .to_owned();
        let account_capabilities = session
            .pointer(&format!(
                "/accounts/{}/accountCapabilities",
                pointer_key(&account)
            ))
            .and_then(Value::as_object);
        let submission = match account_capabilities {
            Some(caps) => caps.get(SUBMISSION).ok_or_else(|| {
                SubmitError::Invalid("this JMAP account may not submit mail".into())
            })?,
            None => &Value::Null,
        };
        let upload_limit = capabilities
            .get(CORE)
            .and_then(|c| c.get("maxSizeUpload"))
            .and_then(Value::as_u64)
            .filter(|n| *n > 0);
        let smtp_limit = submission
            .pointer("/submissionExtensions/SIZE/0")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0);
        let max_size = match (upload_limit, smtp_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let resolve = |key: &str| -> Result<String, SubmitError> {
            let raw = session
                .get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| malformed(&format!("the JMAP session has no {key}")))?;
            if raw.contains('{') {
                return Ok(if raw.contains("://") {
                    raw.to_owned()
                } else {
                    format!("{}{}", base.origin().ascii_serialization(), raw)
                });
            }
            base.join(raw)
                .map(|u| u.to_string())
                .map_err(|_| malformed(&format!("the JMAP session's {key} is not an address")))
        };
        let upload_url = resolve("uploadUrl")?.replace("{accountId}", &account);
        Ok(Self {
            api_url: resolve("apiUrl")?,
            upload_url,
            account,
            max_size,
        })
    }
}
