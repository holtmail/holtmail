use std::collections::HashMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use holtmail_core::ArchiveId;
use holtmail_mailview::Viewer;
use serde_json::{Map, Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Inner;
use crate::auth::{self, Authed};
use crate::error::{MethodError, Problem};
use crate::json::{Args, pointer};
use crate::{email, email_set, ids, mailbox, session, submit};

pub(crate) const CORE: &str = "urn:ietf:params:jmap:core";
pub(crate) const MAIL: &str = "urn:ietf:params:jmap:mail";
pub(crate) const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
pub(crate) const WEBSOCKET: &str = "urn:ietf:params:jmap:websocket";

/// One request's state: the device, the creation ids made so far and the answers so far.
pub(crate) struct Ctx<'a> {
    pub inner: &'a Inner,
    pub authed: &'a Authed,
    pub created: HashMap<String, String>,
}

impl Ctx<'_> {
    /// The account the call names, which the device must cover.
    pub fn account(&self, args: &Args<'_>) -> Result<ArchiveId, MethodError> {
        let id = args.required_string("accountId")?;
        ids::parse_account(id)
            .filter(|a| self.authed.has(*a))
            .ok_or_else(|| MethodError::new("accountNotFound"))
    }

    pub fn viewer(&self, archive: ArchiveId) -> Viewer {
        self.authed.viewer(archive)
    }

    /// An id from the client, with a `#creation` reference resolved.
    pub fn resolve<'s>(&'s self, id: &'s str) -> Option<&'s str> {
        match id.strip_prefix('#') {
            Some(creation) => self.created.get(creation).map(String::as_str),
            None => Some(id),
        }
    }
}

/// Holds a request slot of the user until dropped.
pub(crate) fn slot(inner: &Inner, authed: &Authed) -> Result<OwnedSemaphorePermit, Problem> {
    let semaphore = inner
        .requests
        .lock()
        .expect("request slots")
        .entry(authed.principal.user)
        .or_insert_with(|| {
            Arc::new(Semaphore::new(
                inner.settings.limits.max_concurrent_requests as usize,
            ))
        })
        .clone();
    semaphore.try_acquire_owned().map_err(|_| {
        Problem::limit(
            "maxConcurrentRequests",
            "too many requests at once from this user",
        )
    })
}

pub(crate) async fn handle(State(inner): State<Arc<Inner>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let authed =
        match auth::authenticate(&inner, &parts.headers, auth::peer(&parts.extensions)).await {
            Ok(authed) => authed,
            Err(response) => return *response,
        };
    let json = parts
        .headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return Problem::new("notJSON", "the request must be sent as application/json")
            .into_response();
    }
    let _slot = match slot(&inner, &authed) {
        Ok(slot) => slot,
        Err(problem) => return problem.into_response(),
    };
    let limit = inner.settings.limits.max_size_request;
    let bytes: Bytes = match axum::body::to_bytes(body, limit).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return Problem::limit("maxSizeRequest", "the request is too large").into_response();
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return Problem::new("notJSON", "the request is not JSON").into_response(),
    };
    match process(&inner, &authed, value).await {
        Ok(response) => Json(response).into_response(),
        Err(problem) => problem.into_response(),
    }
}

/// Runs one JMAP request object and returns the response object (RFC 8620 §3.3, §3.4).
pub(crate) async fn process(
    inner: &Inner,
    authed: &Authed,
    request: Value,
) -> Result<Value, Problem> {
    let object = request
        .as_object()
        .ok_or_else(|| Problem::new("notRequest", "the request is not an object"))?;
    let using: Vec<String> = object
        .get("using")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .ok_or_else(|| Problem::new("notRequest", "using is missing"))?;
    for capability in &using {
        if ![
            CORE,
            MAIL,
            SUBMISSION,
            WEBSOCKET,
            crate::INSTANCE_CAPABILITY,
        ]
        .contains(&capability.as_str())
            && !capability.starts_with("urn:ietf:params:jmap:")
        {
            return Err(Problem::new(
                "unknownCapability",
                format!("the server does not support {capability}"),
            ));
        }
    }
    let calls = object
        .get("methodCalls")
        .and_then(Value::as_array)
        .ok_or_else(|| Problem::new("notRequest", "methodCalls is missing"))?;
    if calls.len() > inner.settings.limits.max_calls_in_request {
        return Err(Problem::limit(
            "maxCallsInRequest",
            "too many method calls in one request",
        ));
    }
    let mut parsed = Vec::with_capacity(calls.len());
    for call in calls {
        let triple = call.as_array().filter(|t| t.len() == 3);
        let Some([name, args, id]) = triple.map(|t| [&t[0], &t[1], &t[2]]) else {
            return Err(Problem::new("notRequest", "a method call is not a triple"));
        };
        let (Some(name), Some(args), Some(id)) = (name.as_str(), args.as_object(), id.as_str())
        else {
            return Err(Problem::new("notRequest", "a method call is malformed"));
        };
        parsed.push((name.to_string(), args.clone(), id.to_string()));
    }
    let created: HashMap<String, String> = object
        .get("createdIds")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let echo_created = object.contains_key("createdIds");
    let mut ctx = Ctx {
        inner,
        authed,
        created,
    };
    let mut responses: Vec<Value> = Vec::new();
    for (name, args, id) in parsed {
        let args = match references(args, &responses) {
            Ok(args) => args,
            Err(error) => {
                responses.push(json!(["error", error.json(), id]));
                continue;
            }
        };
        match call(&mut ctx, &using, &name, args).await {
            Ok(answers) => {
                for (answer_name, answer) in answers {
                    responses.push(json!([answer_name, answer, id]));
                }
            }
            Err(error) => responses.push(json!(["error", error.json(), id])),
        }
    }
    let mut out = Map::new();
    out.insert("methodResponses".into(), Value::Array(responses));
    out.insert(
        "sessionState".into(),
        json!(session::state(inner, authed).await),
    );
    if echo_created {
        out.insert("createdIds".into(), json!(ctx.created));
    }
    Ok(Value::Object(out))
}

/// Resolves `#property` result references against earlier answers (RFC 8620 §3.7).
fn references(
    mut args: Map<String, Value>,
    responses: &[Value],
) -> Result<Map<String, Value>, MethodError> {
    let keys: Vec<String> = args
        .keys()
        .filter(|k| k.starts_with('#'))
        .cloned()
        .collect();
    for key in keys {
        let plain = key[1..].to_string();
        if args.contains_key(&plain) {
            return Err(MethodError::invalid(format!(
                "{plain} is given both directly and as a reference"
            )));
        }
        let reference = args.remove(&key).unwrap_or_default();
        let bad = || MethodError::new("invalidResultReference");
        let result_of = reference
            .get("resultOf")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let name = reference
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let path = reference
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(bad)?;
        let answer = responses
            .iter()
            .find(|r| r[2] == result_of)
            .filter(|r| r[0] == name)
            .ok_or_else(bad)?;
        let value = pointer(&answer[1], path).ok_or_else(bad)?;
        args.insert(plain, value);
    }
    Ok(args)
}

fn needs(using: &[String], capability: &str) -> Result<(), MethodError> {
    if using.iter().any(|u| u == capability) {
        Ok(())
    } else {
        Err(MethodError::new("unknownMethod"))
    }
}

async fn call(
    ctx: &mut Ctx<'_>,
    using: &[String],
    name: &str,
    args: Map<String, Value>,
) -> Result<Vec<(String, Value)>, MethodError> {
    let one = |value: Value| Ok(vec![(name.to_string(), value)]);
    let (kind, _) = name.split_once('/').unwrap_or((name, ""));
    match kind {
        "Core" => needs(using, CORE)?,
        "Mailbox" | "Email" | "Thread" | "SearchSnippet" => needs(using, MAIL)?,
        "Identity" | "EmailSubmission" => needs(using, SUBMISSION)?,
        _ => return Err(MethodError::new("unknownMethod")),
    }
    let args_ref = Args(&args);
    match name {
        "Core/echo" => one(Value::Object(args)),
        "Mailbox/get" => one(mailbox::get(ctx, &args_ref).await?),
        "Mailbox/changes" => one(mailbox::changes(ctx, &args_ref).await?),
        "Mailbox/query" => one(mailbox::query(ctx, &args_ref).await?),
        "Mailbox/queryChanges" => {
            ctx.account(&args_ref)?;
            Err(MethodError::new("cannotCalculateChanges"))
        }
        "Mailbox/set" => one(mailbox::set(ctx, &args_ref).await?),
        "Thread/get" => one(email::thread_get(ctx, &args_ref).await?),
        "Thread/changes" => one(email::thread_changes(ctx, &args_ref).await?),
        "Email/get" => one(email::get(ctx, &args_ref).await?),
        "Email/changes" => one(email::changes(ctx, &args_ref).await?),
        "Email/query" => one(email::query(ctx, &args_ref).await?),
        "Email/queryChanges" => {
            ctx.account(&args_ref)?;
            Err(MethodError::new("cannotCalculateChanges"))
        }
        "SearchSnippet/get" => one(email::snippets(ctx, &args_ref).await?),
        "Email/set" => one(email_set::set(ctx, &args_ref).await?),
        "Email/import" => one(email_set::import(ctx, &args_ref).await?),
        "Identity/get" => one(submit::identity_get(ctx, &args_ref).await?),
        "Identity/changes" => one(submit::identity_changes(ctx, &args_ref).await?),
        "Identity/set" => one(submit::identity_set(ctx, &args_ref).await?),
        "EmailSubmission/get" => one(submit::get(ctx, &args_ref).await?),
        "EmailSubmission/changes" => one(submit::changes(ctx, &args_ref).await?),
        "EmailSubmission/query" => one(submit::query(ctx, &args_ref).await?),
        "EmailSubmission/queryChanges" => {
            ctx.account(&args_ref)?;
            Err(MethodError::new("cannotCalculateChanges"))
        }
        "EmailSubmission/set" => submit::set(ctx, &args_ref).await,
        _ => Err(MethodError::new("unknownMethod")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_resolve_and_refuse() {
        let responses = vec![json!(["Email/query", {"ids": ["a", "b"]}, "0"])];
        let args = json!({"#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}});
        let out = references(args.as_object().unwrap().clone(), &responses).unwrap();
        assert_eq!(out["ids"], json!(["a", "b"]));
        let wrong = json!({"#ids": {"resultOf": "0", "name": "Email/get", "path": "/ids"}});
        assert_eq!(
            references(wrong.as_object().unwrap().clone(), &responses)
                .unwrap_err()
                .kind,
            "invalidResultReference"
        );
        let both =
            json!({"ids": [], "#ids": {"resultOf": "0", "name": "Email/query", "path": "/ids"}});
        assert_eq!(
            references(both.as_object().unwrap().clone(), &responses)
                .unwrap_err()
                .kind,
            "invalidArguments"
        );
    }
}
