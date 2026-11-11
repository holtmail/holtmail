use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use holtmail_mailview::MailViewError;
use holtmail_submission::SubmissionError;
use serde_json::{Map, Value, json};

/// A method-level error (RFC 8620 §3.6.2).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MethodError {
    pub kind: &'static str,
    pub description: Option<String>,
}

impl MethodError {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            description: None,
        }
    }

    pub fn with(kind: &'static str, description: impl Into<String>) -> Self {
        Self {
            kind,
            description: Some(description.into()),
        }
    }

    pub fn invalid(description: impl Into<String>) -> Self {
        Self::with("invalidArguments", description)
    }

    pub fn json(&self) -> Value {
        let mut out = Map::new();
        out.insert("type".into(), json!(self.kind));
        if let Some(description) = &self.description {
            out.insert("description".into(), json!(description));
        }
        Value::Object(out)
    }
}

impl From<MailViewError> for MethodError {
    fn from(error: MailViewError) -> Self {
        match error {
            MailViewError::NotFound("archive") => Self::new("accountNotFound"),
            MailViewError::NotFound(what) => Self::invalid(format!("no such {what}")),
            MailViewError::Building { .. } => Self::with(
                "serverUnavailable",
                "the mailbox is being prepared; try again later",
            ),
            MailViewError::CannotCalculateChanges => Self::new("cannotCalculateChanges"),
            MailViewError::TooLarge { limit } => Self::with(
                "requestTooLarge",
                format!("the message is larger than the {limit} bytes allowed"),
            ),
            MailViewError::TooMany { limit } => Self::with(
                "unsupportedFilter",
                format!("the search matches more than {limit} messages; narrow it"),
            ),
            MailViewError::Invalid(reason) => Self::invalid(reason),
            MailViewError::Unavailable(reason) => {
                tracing::warn!(%reason, "mail view unavailable");
                Self::new("serverUnavailable")
            }
        }
    }
}

/// A per-object error of a `/set` or `/import` (RFC 8620 §5.3).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SetError {
    pub kind: &'static str,
    pub description: Option<String>,
    pub properties: Option<Vec<String>>,
}

impl SetError {
    pub fn new(kind: &'static str) -> Self {
        Self {
            kind,
            description: None,
            properties: None,
        }
    }

    pub fn with(kind: &'static str, description: impl Into<String>) -> Self {
        Self {
            kind,
            description: Some(description.into()),
            properties: None,
        }
    }

    pub fn properties(description: impl Into<String>, properties: &[&str]) -> Self {
        Self {
            kind: "invalidProperties",
            description: Some(description.into()),
            properties: Some(properties.iter().map(|p| p.to_string()).collect()),
        }
    }

    pub fn json(&self) -> Value {
        let mut out = Map::new();
        out.insert("type".into(), json!(self.kind));
        if let Some(description) = &self.description {
            out.insert("description".into(), json!(description));
        }
        if let Some(properties) = &self.properties {
            out.insert("properties".into(), json!(properties));
        }
        Value::Object(out)
    }
}

impl From<MailViewError> for SetError {
    fn from(error: MailViewError) -> Self {
        match error {
            MailViewError::NotFound(_) => Self::new("notFound"),
            MailViewError::TooLarge { limit } => Self::with(
                "tooLarge",
                format!("the message is larger than the {limit} bytes allowed"),
            ),
            MailViewError::Invalid(reason) => Self::with("invalidProperties", reason),
            other => Self::with("serverFail", other.to_string()),
        }
    }
}

impl From<SubmissionError> for SetError {
    fn from(error: SubmissionError) -> Self {
        let text = error.to_string();
        match error {
            SubmissionError::Forbidden(_) => Self::with("forbiddenToSend", text),
            SubmissionError::FromMismatch => Self::with("forbiddenFrom", text),
            SubmissionError::Leak => Self::with("forbiddenToSend", text),
            SubmissionError::TooLarge { .. } => Self::with("tooLarge", text),
            SubmissionError::RateLimited { .. } => Self::with("forbiddenToSend", text),
            SubmissionError::Invalid(_) => Self::with("invalidEmail", text),
            SubmissionError::NotFound(_) => Self::new("notFound"),
            SubmissionError::TooLate(_) => Self::with("cannotUnsend", text),
            SubmissionError::Unavailable(_) => {
                tracing::warn!(error = %text, "submission unavailable");
                Self::with("serverFail", "submission is unavailable; try again later")
            }
        }
    }
}

/// A request-level error (RFC 8620 §3.6.1), as an RFC 7807 problem.
#[derive(Debug, Clone)]
pub(crate) struct Problem {
    pub status: StatusCode,
    pub kind: &'static str,
    pub detail: String,
    pub limit: Option<&'static str>,
}

impl Problem {
    pub fn new(kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            kind,
            detail: detail.into(),
            limit: None,
        }
    }

    pub fn limit(limit: &'static str, detail: impl Into<String>) -> Self {
        Self {
            limit: Some(limit),
            ..Self::new("limit", detail)
        }
    }

    pub fn json(&self) -> Value {
        let mut out = Map::new();
        let kind = if self.kind.starts_with("about:") {
            self.kind.to_string()
        } else {
            format!("urn:ietf:params:jmap:error:{}", self.kind)
        };
        out.insert("type".into(), json!(kind));
        out.insert("status".into(), json!(self.status.as_u16()));
        out.insert("detail".into(), json!(self.detail));
        if let Some(limit) = self.limit {
            out.insert("limit".into(), json!(limit));
        }
        Value::Object(out)
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut response = (self.status, axum::Json(self.json())).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        response
    }
}
