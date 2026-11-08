use async_trait::async_trait;
use bytes::Bytes;
use holtmail_collection::{
    CollectionError, CollectionKind, CollectionWriter, ItemPatch, Partstat, Precondition,
    RemoteItem, WriteCaps, Written, media,
};
use reqwest::StatusCode;
use reqwest::header::ETAG;
use url::Url;

use crate::http::{Answer, MAX_XML, status_error};
use crate::xml::{self, CALDAV, DAV, Element};
use crate::{DavSource, Found, etag, ical, ok_props, path, prop};

const RESPOND_ATTEMPTS: usize = 3;

fn media_of(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Contacts => media::VCARD,
        CollectionKind::Calendar => media::ICALENDAR,
    }
}

/// The file name a create with `key` gets: the key's letters, digits, `-` and `.` kept, every
/// other byte written as `_` and two hex digits, so different keys never share a name.
pub(crate) fn file_name(key: &str, kind: CollectionKind) -> String {
    let mut out = String::with_capacity(key.len() + 4);
    for b in key.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'.' {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("_{b:02x}"));
        }
    }
    out.push_str(match kind {
        CollectionKind::Contacts => ".vcf",
        CollectionKind::Calendar => ".ics",
    });
    out
}

fn child(collection: &Url, name: &str) -> Option<Url> {
    let mut base = collection.clone();
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    base.join(&path::encode(name)).ok()
}

fn header_etag(answer: &Answer) -> Option<String> {
    answer
        .headers
        .get(ETAG)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn refusal(answer: &Answer) -> String {
    let host = answer.url.host_str().unwrap_or("the server");
    let condition = xml::parse(&answer.body)
        .filter(|root| root.is(DAV, "error"))
        .and_then(|root| root.children.first().map(|c| c.name.clone()));
    match condition {
        Some(name) => format!(
            "{host} refused the change ({}, {name})",
            answer.status.as_u16()
        ),
        None => format!("{host} refused the change ({})", answer.status.as_u16()),
    }
}

fn write_error(answer: &Answer) -> CollectionError {
    match answer.status.as_u16() {
        403 | 405 | 409 | 413 | 415 | 422 | 507 => CollectionError::Refused(refusal(answer)),
        _ => status_error(answer.status, &answer.url),
    }
}

fn charset(content_type: &str) -> String {
    format!("{content_type}; charset=utf-8")
}

impl DavSource {
    fn item_url(found: &Found, collection: &str, id: &str) -> Option<Url> {
        let name = id.strip_prefix(collection)?;
        if name.is_empty() {
            return None;
        }
        found.url.join(&path::encode(id)).ok()
    }

    async fn etag_of(&self, url: &Url) -> Option<String> {
        let (root, _) = self.propfind(url, "0", "<d:getetag/>").await.ok()?;
        let response = root.child(DAV, "response")?;
        etag(&ok_props(response))
    }

    /// The item at `url` with its tag, or `None` when it is not there.
    async fn current(&self, url: &Url) -> Result<Option<(Option<String>, Bytes)>, CollectionError> {
        let Some(answer) = self.http.get(url).await? else {
            return Ok(None);
        };
        let tag = match header_etag(&answer) {
            Some(tag) => Some(tag),
            None => self.etag_of(url).await,
        };
        Ok(Some((tag, answer.body)))
    }

    async fn current_tag(&self, url: &Url) -> Result<Option<String>, CollectionError> {
        Ok(self.current(url).await?.and_then(|(tag, _)| tag))
    }

    async fn send_put(
        &self,
        url: &Url,
        content_type: &str,
        raw: &Bytes,
        headers: &[(&str, &str)],
    ) -> Result<Answer, CollectionError> {
        self.http
            .request(
                "PUT",
                url,
                None,
                Some((&charset(content_type), raw)),
                headers,
                MAX_XML,
            )
            .await
    }

    async fn written(
        &self,
        url: &Url,
        answer: &Answer,
        kind: CollectionKind,
        raw: &Bytes,
    ) -> Written {
        let id = path::id(url);
        match header_etag(answer) {
            Some(tag) if tag.starts_with("W/") => Written {
                id,
                etag: Some(tag),
                item: None,
            },
            Some(tag) => Written {
                id: id.clone(),
                etag: Some(tag.clone()),
                item: Some(RemoteItem {
                    id,
                    etag: Some(tag),
                    content_type: media_of(kind).into(),
                    raw: raw.clone(),
                }),
            },
            None => Written {
                id,
                etag: self.etag_of(url).await,
                item: None,
            },
        }
    }

    async fn own_addresses(&self, collection: &Url) -> Vec<String> {
        let cached = self.user_addresses.lock().expect("addresses lock").clone();
        let listed = match cached {
            Some(listed) => listed,
            None => match self.principal_addresses(collection).await {
                Some(listed) => {
                    *self.user_addresses.lock().expect("addresses lock") = Some(listed.clone());
                    listed
                }
                None => Vec::new(),
            },
        };
        self.addresses.iter().cloned().chain(listed).collect()
    }

    async fn principal_addresses(&self, collection: &Url) -> Option<Vec<String>> {
        let (root, base) = self
            .propfind(collection, "0", "<d:current-user-principal/>")
            .await
            .ok()?;
        let props = ok_props(root.child(DAV, "response")?);
        let principal = prop(&props, DAV, "current-user-principal")
            .and_then(|p| p.child(DAV, "href"))
            .and_then(Element::trimmed)
            .and_then(|h| base.join(h).ok())?;
        if !self.http.may_sign_in(&principal) {
            return None;
        }
        let (root, _) = self
            .propfind(&principal, "0", "<cal:calendar-user-address-set/>")
            .await
            .ok()?;
        let props = ok_props(root.child(DAV, "response")?);
        Some(
            prop(&props, CALDAV, "calendar-user-address-set")
                .map(|set| {
                    set.children(DAV, "href")
                        .filter_map(Element::trimmed)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        )
    }

    async fn create(
        &self,
        found: &Found,
        change: &ItemPatch,
        idempotency: &str,
    ) -> Result<Written, CollectionError> {
        let url = child(&found.url, &file_name(idempotency, found.kind))
            .ok_or_else(|| CollectionError::Protocol("the item address is invalid".into()))?;
        let answer = self
            .send_put(
                &url,
                &change.content_type,
                &change.raw,
                &[("If-None-Match", "*")],
            )
            .await?;
        if answer.status.is_success() {
            return Ok(self.written(&url, &answer, found.kind, &change.raw).await);
        }
        match answer.status.as_u16() {
            412 => {
                let Some((tag, raw)) = self.current(&url).await? else {
                    return Err(CollectionError::PreconditionFailed { current: None });
                };
                let ours = match &change.uid {
                    Some(uid) => {
                        ical::uid(&String::from_utf8_lossy(&raw)).as_deref() == Some(uid.as_str())
                    }
                    None => raw == change.raw,
                };
                if !ours {
                    return Err(CollectionError::PreconditionFailed { current: tag });
                }
                let id = path::id(&url);
                Ok(Written {
                    id: id.clone(),
                    etag: tag.clone(),
                    item: Some(RemoteItem {
                        id,
                        etag: tag,
                        content_type: media_of(found.kind).into(),
                        raw,
                    }),
                })
            }
            404 | 410 => Err(CollectionError::Gone),
            _ => Err(write_error(&answer)),
        }
    }

    async fn update(
        &self,
        found: &Found,
        url: &Url,
        change: &ItemPatch,
        precondition: Precondition,
    ) -> Result<Written, CollectionError> {
        let (name, value) = match precondition {
            Precondition::None => ("If-Match", "*".to_owned()),
            Precondition::Absent => ("If-None-Match", "*".to_owned()),
            Precondition::IfMatch(tag) => ("If-Match", tag),
            Precondition::IfUnchanged(expected) => {
                let Some((tag, raw)) = self.current(url).await? else {
                    return Err(CollectionError::PreconditionFailed { current: None });
                };
                if raw != expected {
                    return Err(CollectionError::PreconditionFailed { current: tag });
                }
                ("If-Match", tag.unwrap_or_else(|| "*".to_owned()))
            }
        };
        let answer = self
            .send_put(url, &change.content_type, &change.raw, &[(name, &value)])
            .await?;
        if answer.status.is_success() {
            return Ok(self.written(url, &answer, found.kind, &change.raw).await);
        }
        match answer.status.as_u16() {
            412 => Err(CollectionError::PreconditionFailed {
                current: self.current_tag(url).await?,
            }),
            404 | 410 => Err(CollectionError::PreconditionFailed { current: None }),
            _ => Err(write_error(&answer)),
        }
    }
}

/// Writes with `PUT` and `DELETE` (RFC 4791 §5.3.2, RFC 6352 §6.3.2), conditional on ETags.
/// Capabilities come from the last collection listing: a collection not listed yet offers
/// nothing until [`holtmail_collection::CollectionSource::collections`] or a write has run.
#[async_trait]
impl CollectionWriter for DavSource {
    /// Create, update and delete in every listed collection, with tags and the collection's
    /// `max-resource-size`. Answering invitations only in calendars whose server advertises
    /// `calendar-auto-schedule` (RFC 6638), where a `PUT` of the attendee copy makes the server
    /// tell the organizer.
    fn capabilities(&self, collection: &str) -> WriteCaps {
        let Some(found) = self
            .found
            .lock()
            .expect("found lock")
            .get(collection)
            .cloned()
        else {
            return WriteCaps::default();
        };
        WriteCaps {
            create: true,
            update: true,
            delete: true,
            respond: found.kind == CollectionKind::Calendar && found.auto_schedule,
            field_patch: false,
            tags: true,
            max_size: found.max_size,
            keeps_uid: true,
        }
    }

    /// A create is a `PUT` with `If-None-Match: *` to a name made from `idempotency`; a `412`
    /// there whose item has the change's `UID` (its bytes, without one) is the earlier create
    /// and answered as success. An update sends `If-Match` with the tag, `*` for
    /// [`Precondition::None`] (so a missing item is not made again), or the tag read just before
    /// for [`Precondition::IfUnchanged`]. A `412` reads the current tag for the error.
    async fn put(
        &self,
        collection: &str,
        id: Option<&str>,
        change: &ItemPatch,
        precondition: Precondition,
        idempotency: &str,
    ) -> Result<Written, CollectionError> {
        let found = self.found(collection).await?;
        let media = media_of(found.kind);
        let base = change.content_type.split(';').next().unwrap_or_default();
        if !base.trim().eq_ignore_ascii_case(media) {
            return Err(CollectionError::Refused(format!(
                "this collection takes only {media}"
            )));
        }
        if found
            .max_size
            .is_some_and(|max| change.raw.len() as u64 > max)
        {
            return Err(CollectionError::Refused(
                "the item is larger than the server takes".into(),
            ));
        }
        let result = match id {
            None => self.create(&found, change, idempotency).await,
            Some(id) => {
                let url = Self::item_url(&found, collection, id)
                    .ok_or(CollectionError::PreconditionFailed { current: None })?;
                self.update(&found, &url, change, precondition).await
            }
        };
        tracing::debug!(ok = result.is_ok(), "DAV item written");
        result
    }

    /// `DELETE`, with `If-Match` when given. `404` and `410` count as deleted; a `412` for an
    /// item that is gone meanwhile does too.
    async fn delete(
        &self,
        collection: &str,
        id: &str,
        if_match: Option<&str>,
    ) -> Result<(), CollectionError> {
        let found = self.found(collection).await?;
        let Some(url) = Self::item_url(&found, collection, id) else {
            return Ok(());
        };
        let headers: Vec<(&str, &str)> = if_match.map(|t| ("If-Match", t)).into_iter().collect();
        let answer = self
            .http
            .request("DELETE", &url, None, None, &headers, MAX_XML)
            .await?;
        match answer.status.as_u16() {
            s if (200..300).contains(&s) => Ok(()),
            404 | 410 => Ok(()),
            412 => match self.current(&url).await? {
                None => Ok(()),
                Some((current, _)) => Err(CollectionError::PreconditionFailed { current }),
            },
            _ => Err(write_error(&answer)),
        }
    }

    /// Reads the event, sets `PARTSTAT` on the attendee lines that are the account's own
    /// (addresses from [`DavSource::with_addresses`] and the principal's
    /// `calendar-user-address-set`), and `PUT`s it back with `If-Match`, reading again up to
    /// three times when another client changed it meanwhile. Without `notify_organizer` the
    /// `PUT` carries `Schedule-Reply: F`. With it, a calendar whose server does not schedule,
    /// or an event that leaves scheduling to apps (`SCHEDULE-AGENT=CLIENT`), is `Refused`.
    async fn respond(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
        answer: Partstat,
        notify_organizer: bool,
    ) -> Result<Written, CollectionError> {
        let found = self.found(collection).await?;
        if found.kind != CollectionKind::Calendar {
            return Err(CollectionError::Refused("not a calendar".into()));
        }
        let url = Self::item_url(&found, collection, id)
            .ok_or(CollectionError::PreconditionFailed { current: None })?;
        if notify_organizer && !found.auto_schedule {
            return Err(CollectionError::Refused(
                "this calendar's server does not send answers to organizers".into(),
            ));
        }
        let addresses = self.own_addresses(&found.url).await;
        let mut last = None;
        for _ in 0..RESPOND_ATTEMPTS {
            let Some((tag, raw)) = self.current(&url).await? else {
                return Err(CollectionError::PreconditionFailed { current: None });
            };
            let text = std::str::from_utf8(&raw)
                .map_err(|_| CollectionError::Refused("the event is not UTF-8 text".into()))?;
            let changed = ical::answer(text, &addresses, recurrence_id, answer, notify_organizer)
                .map_err(|e| CollectionError::Refused(e.reason().into()))?;
            let changed = Bytes::from(changed);
            let condition = tag.clone().unwrap_or_else(|| "*".into());
            let mut headers = vec![("If-Match", condition.as_str())];
            if !notify_organizer {
                headers.push(("Schedule-Reply", "F"));
            }
            let put = self
                .send_put(&url, media::ICALENDAR, &changed, &headers)
                .await?;
            if put.status.is_success() {
                tracing::debug!("DAV invitation answered");
                return Ok(self.written(&url, &put, found.kind, &changed).await);
            }
            match put.status {
                StatusCode::PRECONDITION_FAILED => last = tag,
                StatusCode::NOT_FOUND | StatusCode::GONE => {
                    return Err(CollectionError::PreconditionFailed { current: None });
                }
                _ => return Err(write_error(&put)),
            }
        }
        Err(CollectionError::PreconditionFailed {
            current: self.current_tag(&url).await?.or(last),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_keys_are_safe_and_distinct() {
        assert_eq!(
            file_name("0192-abc.d", CollectionKind::Calendar),
            "0192-abc.d.ics"
        );
        assert_eq!(
            file_name("a/b c", CollectionKind::Contacts),
            "a_2fb_20c.vcf"
        );
        assert_ne!(
            file_name("a_2f", CollectionKind::Contacts),
            file_name("a/", CollectionKind::Contacts)
        );
    }
}
