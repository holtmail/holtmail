use async_trait::async_trait;
use bytes::Bytes;

use super::{FakeCollections, FakeItem, State, content_type, split, touch};
use crate::{
    CollectionError, CollectionKind, CollectionWriter, ItemPatch, Partstat, Precondition,
    RemoteItem, WriteCaps, Written,
};

/// Reads back and disturbs what a server holds behind a [`CollectionWriter`], for
/// [`writer_conformance`].
#[async_trait]
pub trait WriteProbe: Send + Sync {
    /// The address of the account the writer writes as, which the suite invites.
    fn own_address(&self) -> String;

    /// The item's version tag and bytes as the server holds them now, or `None` when it is not
    /// there.
    async fn read(&self, collection: &str, id: &str) -> Option<(Option<String>, Bytes)>;

    /// The ids of every item in the collection.
    async fn ids(&self, collection: &str) -> Vec<String>;

    /// Changes the item on the server as another client would, so its tag and bytes change.
    async fn touch(&self, collection: &str, id: &str);

    /// The account's own answer in the item, for the series or one occurrence; `None` when it
    /// has none.
    async fn partstat(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
    ) -> Option<Partstat>;

    /// Whether `stored` (from [`WriteProbe::read`]) carries what `patch` wrote. Byte equality by
    /// default; writers that convert compare the fields they wrote.
    fn holds(&self, stored: &[u8], patch: &ItemPatch) -> bool {
        stored == patch.raw.as_ref()
    }

    /// The `UID` in `stored` (from [`WriteProbe::read`]). By default the first `UID` line of
    /// iCalendar or vCard text; writers that convert read it from their own format.
    fn uid(&self, stored: &[u8]) -> Option<String> {
        String::from_utf8_lossy(stored)
            .lines()
            .find_map(|l| l.strip_prefix("UID:").map(|u| u.trim().to_owned()))
    }

    /// A collection id of the writer's own shape that names no collection on the server.
    fn unknown_collection(&self) -> String {
        "/no/such/collection/".into()
    }
}

fn write(state: &mut State, collection: &str, name: &str, raw: Bytes) -> u64 {
    state.seq += 1;
    let seq = state.seq;
    if let Some(coll) = state.collections.get_mut(collection) {
        coll.items.insert(name.into(), FakeItem { etag: seq, raw });
        coll.log.push((seq, name.into()));
    }
    touch(state, collection);
    seq
}

impl FakeCollections {
    /// Makes every collection writable through [`CollectionWriter`]; until then the fake
    /// offers no writes.
    pub fn enable_writes(&self) {
        self.state.lock().expect("fake lock").writable = true;
    }

    /// Makes every later write fail with [`CollectionError::Refused`] and this reason, or
    /// accepts writes again with `None`.
    pub fn refuse_writes(&self, reason: Option<&str>) {
        self.state.lock().expect("fake lock").refuse = reason.map(str::to_owned);
    }

    /// The answers sent to organizers so far: item id, occurrence and answer, in order.
    pub fn notices(&self) -> Vec<(String, Option<String>, Partstat)> {
        self.state.lock().expect("fake lock").notices.clone()
    }

    fn writable(state: &State, collection: &str) -> Result<(), CollectionError> {
        Self::check(state)?;
        if !state.collections.contains_key(collection) {
            return Err(CollectionError::Gone);
        }
        if !state.writable {
            return Err(CollectionError::Refused(
                "the collection is read-only".into(),
            ));
        }
        if let Some(reason) = &state.refuse {
            return Err(CollectionError::Refused(reason.clone()));
        }
        Ok(())
    }

    fn written(state: &State, collection: &str, name: &str) -> Written {
        let coll = &state.collections[collection];
        let item = &coll.items[name];
        let id = format!("{collection}{name}");
        Written {
            id: id.clone(),
            etag: Some(Self::tag(item.etag)),
            item: Some(RemoteItem {
                id,
                etag: Some(Self::tag(item.etag)),
                content_type: coll
                    .media
                    .clone()
                    .unwrap_or_else(|| content_type(coll.kind).into()),
                raw: item.raw.clone(),
            }),
        }
    }
}

#[async_trait]
impl CollectionWriter for FakeCollections {
    fn capabilities(&self, collection: &str) -> WriteCaps {
        let state = self.state.lock().expect("fake lock");
        if !state.writable || !state.collections.contains_key(collection) {
            return WriteCaps::default();
        }
        WriteCaps {
            create: true,
            update: true,
            delete: true,
            respond: true,
            field_patch: false,
            tags: true,
            max_size: None,
            keeps_uid: true,
        }
    }

    async fn put(
        &self,
        collection: &str,
        id: Option<&str>,
        change: &ItemPatch,
        precondition: Precondition,
        idempotency: &str,
    ) -> Result<Written, CollectionError> {
        let mut state = self.state.lock().expect("fake lock");
        Self::writable(&state, collection)?;
        let name = match id {
            None => {
                let key = (collection.to_owned(), idempotency.to_owned());
                if let Some(name) = state.created.get(&key).cloned()
                    && state.collections[collection].items.contains_key(&name)
                {
                    return Ok(Self::written(&state, collection, &name));
                }
                let extension = match state.collections[collection].kind {
                    CollectionKind::Contacts => "vcf",
                    CollectionKind::Calendar => "ics",
                };
                let name = format!("{idempotency}.{extension}");
                if let Some(taken) = state.collections[collection].items.get(&name) {
                    return Err(CollectionError::PreconditionFailed {
                        current: Some(Self::tag(taken.etag)),
                    });
                }
                state.created.insert(key, name.clone());
                name
            }
            Some(id) => {
                let gone = CollectionError::PreconditionFailed { current: None };
                let name = split(collection, id).ok_or(gone)?;
                let Some(item) = state.collections[collection].items.get(&name) else {
                    return Err(CollectionError::PreconditionFailed { current: None });
                };
                let holds = match &precondition {
                    Precondition::None => true,
                    Precondition::Absent => false,
                    Precondition::IfMatch(tag) => *tag == Self::tag(item.etag),
                    Precondition::IfUnchanged(raw) => *raw == item.raw,
                };
                if !holds {
                    return Err(CollectionError::PreconditionFailed {
                        current: Some(Self::tag(item.etag)),
                    });
                }
                name
            }
        };
        write(&mut state, collection, &name, change.raw.clone());
        let written = Self::written(&state, collection, &name);
        drop(state);
        self.notify.notify_waiters();
        Ok(written)
    }

    async fn delete(
        &self,
        collection: &str,
        id: &str,
        if_match: Option<&str>,
    ) -> Result<(), CollectionError> {
        let mut state = self.state.lock().expect("fake lock");
        Self::writable(&state, collection)?;
        let Some(name) = split(collection, id) else {
            return Ok(());
        };
        let Some(item) = state.collections[collection].items.get(&name) else {
            return Ok(());
        };
        if let Some(tag) = if_match
            && tag != Self::tag(item.etag)
        {
            return Err(CollectionError::PreconditionFailed {
                current: Some(Self::tag(item.etag)),
            });
        }
        state.seq += 1;
        let seq = state.seq;
        if let Some(coll) = state.collections.get_mut(collection) {
            coll.items.remove(&name);
            coll.log.push((seq, name));
        }
        touch(&mut state, collection);
        drop(state);
        self.notify.notify_waiters();
        Ok(())
    }

    async fn respond(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
        answer: Partstat,
        notify_organizer: bool,
    ) -> Result<Written, CollectionError> {
        let mut state = self.state.lock().expect("fake lock");
        Self::writable(&state, collection)?;
        let gone = CollectionError::PreconditionFailed { current: None };
        let name = split(collection, id).ok_or(gone)?;
        let Some(raw) = state.collections[collection]
            .items
            .get(&name)
            .map(|i| i.raw.clone())
        else {
            return Err(CollectionError::PreconditionFailed { current: None });
        };
        let occurrence = recurrence_id.map(str::to_owned);
        state
            .answers
            .insert((id.to_owned(), occurrence.clone()), answer);
        if notify_organizer {
            state.notices.push((id.to_owned(), occurrence, answer));
        }
        write(&mut state, collection, &name, raw);
        let written = Self::written(&state, collection, &name);
        drop(state);
        self.notify.notify_waiters();
        Ok(written)
    }
}

#[async_trait]
impl WriteProbe for FakeCollections {
    fn own_address(&self) -> String {
        "ada@example.com".into()
    }

    async fn read(&self, collection: &str, id: &str) -> Option<(Option<String>, Bytes)> {
        let state = self.state.lock().expect("fake lock");
        let name = split(collection, id)?;
        let item = state.collections.get(collection)?.items.get(&name)?;
        Some((Some(Self::tag(item.etag)), item.raw.clone()))
    }

    async fn ids(&self, collection: &str) -> Vec<String> {
        let state = self.state.lock().expect("fake lock");
        state
            .collections
            .get(collection)
            .map(|c| {
                c.items
                    .keys()
                    .map(|name| format!("{collection}{name}"))
                    .collect()
            })
            .unwrap_or_default()
    }

    async fn touch(&self, collection: &str, id: &str) {
        let mut state = self.state.lock().expect("fake lock");
        let Some(name) = split(collection, id) else {
            return;
        };
        let Some(raw) = state
            .collections
            .get(collection)
            .and_then(|c| c.items.get(&name))
            .map(|i| [i.raw.as_ref(), b"X-TOUCHED:1\r\n"].concat())
        else {
            return;
        };
        write(&mut state, collection, &name, Bytes::from(raw));
        drop(state);
        self.notify.notify_waiters();
    }

    async fn partstat(
        &self,
        collection: &str,
        id: &str,
        recurrence_id: Option<&str>,
    ) -> Option<Partstat> {
        let _ = collection;
        self.state
            .lock()
            .expect("fake lock")
            .answers
            .get(&(id.to_owned(), recurrence_id.map(str::to_owned)))
            .copied()
    }
}

fn event(uid: &str, summary: &str, attendee: Option<&str>) -> Bytes {
    let invite = attendee
        .map(|a| {
            format!(
                "ORGANIZER:mailto:organizer@example.org\r\n\
                 ATTENDEE;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{a}\r\n\
                 RRULE:FREQ=WEEKLY;COUNT=4\r\n"
            )
        })
        .unwrap_or_default();
    Bytes::from(format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Holtmail//conformance//EN\r\n\
         BEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20261101T090000Z\r\n\
         DTSTART:20261201T100000Z\r\nDTEND:20261201T110000Z\r\nSUMMARY:{summary}\r\n\
         {invite}END:VEVENT\r\nEND:VCALENDAR\r\n"
    ))
}

fn card(uid: &str, name: &str) -> Bytes {
    Bytes::from(format!(
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\n\
         EMAIL:{}@example.com\r\nEND:VCARD\r\n",
        name.to_ascii_lowercase().replace(' ', ".")
    ))
}

fn patch(kind: CollectionKind, uid: &str, text: &str) -> ItemPatch {
    let (content_type, raw, fields) = match kind {
        CollectionKind::Calendar => (
            crate::media::ICALENDAR,
            event(uid, text, None),
            vec![crate::Field::Summary],
        ),
        CollectionKind::Contacts => (
            crate::media::VCARD,
            card(uid, text),
            vec![crate::Field::Name, crate::Field::Emails],
        ),
    };
    ItemPatch {
        content_type: content_type.into(),
        raw,
        uid: Some(uid.into()),
        fields: Some(fields),
    }
}

/// Checks that `writer` keeps the [`CollectionWriter`] contract in `collection`, an empty
/// collection of `kind` on a server it may write to, using `probe` to see what the server holds.
/// Answering invitations is checked only when the writer says it can. Panics on the first
/// violation.
pub async fn writer_conformance(
    writer: &dyn CollectionWriter,
    probe: &dyn WriteProbe,
    collection: &str,
    kind: CollectionKind,
) {
    let caps = writer.capabilities(collection);
    assert!(
        caps.create && caps.update && caps.delete,
        "the suite needs a writable collection"
    );
    assert_eq!(
        writer.capabilities(&probe.unknown_collection()),
        WriteCaps::default(),
        "an unknown collection offers nothing"
    );

    let first = patch(kind, "conformance-1@example.com", "Ada Lovelace");
    let created = writer
        .put(
            collection,
            None,
            &first,
            Precondition::Absent,
            "conformance-1",
        )
        .await
        .expect("a create");
    let (tag, stored) = probe.read(collection, &created.id).await.expect("created");
    assert!(
        probe.holds(&stored, &first),
        "the server holds what was written"
    );
    if caps.keeps_uid {
        assert_eq!(probe.uid(&stored), first.uid, "a create keeps the UID");
    }
    if caps.tags {
        assert!(
            created.etag.is_some(),
            "a server with tags answers with one"
        );
        assert_eq!(created.etag, tag, "the answer carries the current tag");
    }
    let again = writer
        .put(
            collection,
            None,
            &first,
            Precondition::Absent,
            "conformance-1",
        )
        .await
        .expect("a repeated create");
    assert_eq!(again.id, created.id, "creates are idempotent by key");
    assert_eq!(
        probe.ids(collection).await,
        vec![created.id.clone()],
        "a repeated create makes no second item"
    );

    let second = patch(kind, "conformance-1@example.com", "Ada King");
    let condition = |etag: &Option<String>, raw: &Bytes| match (caps.tags, etag) {
        (true, Some(tag)) => Precondition::IfMatch(tag.clone()),
        _ => Precondition::IfUnchanged(raw.clone()),
    };
    let before = condition(&tag, &stored);
    let updated = writer
        .put(
            collection,
            Some(&created.id),
            &second,
            before.clone(),
            "conformance-2",
        )
        .await
        .expect("an update under a holding precondition");
    assert_eq!(updated.id, created.id);
    let (tag2, stored2) = probe.read(collection, &created.id).await.expect("updated");
    assert!(probe.holds(&stored2, &second));
    if caps.tags {
        assert_ne!(tag2, tag, "an update changes the tag");
    }

    let stale = writer
        .put(
            collection,
            Some(&created.id),
            &first,
            before,
            "conformance-3",
        )
        .await;
    match stale {
        Err(CollectionError::PreconditionFailed { current }) => {
            if caps.tags {
                assert_eq!(current, tag2, "a failed precondition names the current tag");
            }
        }
        other => panic!("a stale precondition must fail, got {other:?}"),
    }
    let (_, unchanged) = probe
        .read(collection, &created.id)
        .await
        .expect("still there");
    assert!(
        probe.holds(&unchanged, &second),
        "a failed write changed nothing"
    );

    let now = condition(&tag2, &stored2);
    probe.touch(collection, &created.id).await;
    assert!(
        matches!(
            writer
                .put(collection, Some(&created.id), &first, now, "conformance-4")
                .await,
            Err(CollectionError::PreconditionFailed { .. })
        ),
        "a change made upstream meanwhile fails the precondition"
    );
    let forced = writer
        .put(
            collection,
            Some(&created.id),
            &first,
            Precondition::None,
            "conformance-5",
        )
        .await
        .expect("a write without a precondition");
    let (tag3, stored3) = probe.read(collection, &created.id).await.expect("forced");
    assert!(probe.holds(&stored3, &first), "the last writer wins");
    if caps.tags {
        assert_eq!(forced.etag, tag3);
        assert!(matches!(
            writer
                .delete(collection, &created.id, Some("\"no-such-tag\""))
                .await,
            Err(CollectionError::PreconditionFailed { .. })
        ));
        assert!(probe.read(collection, &created.id).await.is_some());
    }
    writer
        .delete(
            collection,
            &created.id,
            tag3.as_deref().filter(|_| caps.tags),
        )
        .await
        .expect("a delete");
    assert!(probe.read(collection, &created.id).await.is_none());
    writer
        .delete(collection, &created.id, None)
        .await
        .expect("deleting a missing item succeeds");
    assert!(
        matches!(
            writer
                .put(
                    collection,
                    Some(&created.id),
                    &second,
                    Precondition::IfUnchanged(stored3.clone()),
                    "conformance-6"
                )
                .await,
            Err(CollectionError::PreconditionFailed { current: None })
        ),
        "an update of a missing item fails without a tag"
    );

    assert!(matches!(
        writer
            .put(
                "/no/such/collection/",
                None,
                &first,
                Precondition::Absent,
                "conformance-7"
            )
            .await,
        Err(CollectionError::Gone)
    ));

    if caps.respond && kind == CollectionKind::Calendar {
        let own = probe.own_address();
        let invitation = ItemPatch {
            content_type: crate::media::ICALENDAR.into(),
            raw: event("conformance-invite@example.org", "Planning", Some(&own)),
            uid: Some("conformance-invite@example.org".into()),
            fields: None,
        };
        let invited = writer
            .put(
                collection,
                None,
                &invitation,
                Precondition::Absent,
                "conformance-invite",
            )
            .await
            .expect("an invitation");
        writer
            .respond(collection, &invited.id, None, Partstat::Accepted, false)
            .await
            .expect("an answer");
        assert_eq!(
            probe.partstat(collection, &invited.id, None).await,
            Some(Partstat::Accepted)
        );
        writer
            .respond(
                collection,
                &invited.id,
                Some("20261208T100000Z"),
                Partstat::Declined,
                true,
            )
            .await
            .expect("an answer for one occurrence");
        assert_eq!(
            probe
                .partstat(collection, &invited.id, Some("20261208T100000Z"))
                .await,
            Some(Partstat::Declined)
        );
        assert_eq!(
            probe.partstat(collection, &invited.id, None).await,
            Some(Partstat::Accepted),
            "answering one occurrence keeps the series' answer"
        );
        writer
            .delete(collection, &invited.id, None)
            .await
            .expect("deleting the invitation");
        assert!(matches!(
            writer
                .respond(collection, &invited.id, None, Partstat::Accepted, false)
                .await,
            Err(CollectionError::PreconditionFailed { current: None })
        ));
    }
}
