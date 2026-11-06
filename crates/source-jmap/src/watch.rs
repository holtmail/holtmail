use std::time::Duration;

use async_trait::async_trait;
use holtmail_source::{Changed, SourceError, SourceWatch};
use serde_json::Value;

pub(crate) const PING: Duration = Duration::from_secs(60);

/// The event source address for `types`, from the session's template (RFC 8620 §7.3); a plain
/// address gets the parameters as a query.
pub(crate) fn event_url(template: &str, types: &[&str], ping: Duration) -> String {
    let types = types.join(",");
    let ping = ping.as_secs().to_string();
    if template.contains("{types}") {
        crate::expand(
            template,
            &[("types", &types), ("closeafter", "no"), ("ping", &ping)],
        )
    } else {
        let joiner = if template.contains('?') { '&' } else { '?' };
        format!("{template}{joiner}types={types}&closeafter=no&ping={ping}")
    }
}

/// An open event stream, reporting `StateChange`s of one account's watched types.
pub(crate) struct JmapWatch {
    response: reqwest::Response,
    parser: holtmail_sse::Parser,
    account: String,
    types: &'static [&'static str],
    silence: Duration,
}

impl JmapWatch {
    pub(crate) fn new(
        response: reqwest::Response,
        account: String,
        types: &'static [&'static str],
        ping: Duration,
    ) -> Self {
        Self {
            response,
            parser: holtmail_sse::Parser::default(),
            account,
            types,
            silence: ping * 5 / 2,
        }
    }

    fn relevant(&self, data: &str) -> bool {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return false;
        };
        value.get("@type").and_then(Value::as_str) == Some("StateChange")
            && value
                .pointer(&format!(
                    "/changed/{}",
                    self.account.replace('~', "~0").replace('/', "~1")
                ))
                .and_then(Value::as_object)
                .is_some_and(|changed| self.types.iter().any(|t| changed.contains_key(*t)))
    }
}

#[async_trait]
impl SourceWatch for JmapWatch {
    async fn next(&mut self) -> Result<Changed, SourceError> {
        loop {
            let chunk = tokio::time::timeout(self.silence, self.response.chunk())
                .await
                .map_err(|_| SourceError::Connection("the event source fell silent".into()))?
                .map_err(|_| SourceError::Connection("the event source broke off".into()))?
                .ok_or_else(|| SourceError::Connection("the event source closed".into()))?;
            let events = self
                .parser
                .push(&chunk)
                .map_err(|e| SourceError::Protocol(e.to_string()))?;
            if events
                .iter()
                .any(|e| (e.event == "state" || e.event == "message") && self.relevant(&e.data))
            {
                return Ok(Changed::all());
            }
        }
    }

    async fn close(self: Box<Self>) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_the_template_or_appends_a_query() {
        assert_eq!(
            event_url(
                "https://jmap.example.com/events?types={types}&closeafter={closeafter}&ping={ping}",
                &["Email", "Mailbox"],
                PING
            ),
            "https://jmap.example.com/events?types=Email%2CMailbox&closeafter=no&ping=60"
        );
        assert_eq!(
            event_url(
                "http://127.0.0.1:1/jmap/events/",
                &["Email"],
                Duration::from_secs(5)
            ),
            "http://127.0.0.1:1/jmap/events/?types=Email&closeafter=no&ping=5"
        );
    }
}
