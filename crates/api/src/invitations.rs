use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use bytes::Bytes;
use holtmail_collection::{Partstat, WriteCaps};
use holtmail_compose::{ImipKind, Invitation, Mailbox};
use holtmail_core::{ArchiveId, ItemId, Timestamp};
use holtmail_outbox::Queue;
use holtmail_pim::{ItemKind, Method};
use holtmail_pim_edit::{
    CopyUpdate, Host, Imip, ImipError, OrganizerChange, OrganizerRoute, ReplyRoute, Respond,
};
use holtmail_route::{Context, RouteError, Routed};
use holtmail_store::{
    Account, AuditKind, Capability, ChangeTarget, Collection, CollectionKind, Effective, Message,
    OutboxEntry, OutboxState, Transport, User, WriteMode,
};
use holtmail_submit::Envelope;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::dto::ErrorDto;
use crate::mailboxes::{Scheduling, SchedulingProbe};
use crate::outbox::{OutboxDto, may_send, may_send_somehow, outbox_view};
use crate::pim_edit::{PimChangeDto, RecordAs};
use crate::relay::{self, Routing};
use crate::send::{outbox_error, sending};
use crate::write::{Scopes, audit, may_write};
use crate::{ApiError, ApiResult, ApiState, CurrentUser};

type AppState = State<Arc<ApiState>>;

/// An answer to an invitation.
#[derive(Debug, Deserialize, ToSchema)]
pub struct RsvpRequest {
    /// `accepted`, `tentative` or `declined`.
    pub answer: String,
    /// A note for the organizer, sent as the reply's `COMMENT`.
    #[serde(default)]
    pub comment: Option<String>,
    /// One occurrence of a repeating event, by its `RECURRENCE-ID` as iCalendar writes it
    /// (`20261208T100000Z`, `20261208T110000` in the event's zone, `20261208`); absent answers
    /// the whole event.
    #[serde(default)]
    pub recurrence_id: Option<String>,
}

/// How an answer was given.
#[derive(Debug, Serialize, ToSchema)]
pub struct RsvpDto {
    /// `server` (the account's server tells the organizer), `imip` (an iMIP reply through the
    /// outbox) or `quiet` (the organizer asked for no answers; only the copy follows).
    pub route: String,
    /// How the account's copy follows: `respond`, `put`, `held` (recorded, sent once the
    /// collection writes through) or `none` (no account holds a copy; the archived reply shows
    /// the answer once it leaves).
    pub copy: String,
    /// The answer recorded on the account's copy.
    pub change: Option<PimChangeDto>,
    /// The iMIP reply; it waits for the undo window, so cancelling it takes the answer back
    /// before the organizer sees it.
    pub outbox: Option<OutboxDto>,
}

fn unprocessable(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

fn forbidden(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::FORBIDDEN, message.into())
}

fn imip_error(error: ImipError) -> ApiError {
    match error {
        ImipError::NotAttendee => unprocessable("none of your addresses is invited to this event"),
        ImipError::NoInstance => unprocessable("the event has no such occurrence"),
        other => unprocessable(other.to_string()),
    }
}

fn answer_of(raw: &str) -> ApiResult<(Partstat, holtmail_compose::Answer)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "accepted" => Ok((Partstat::Accepted, holtmail_compose::Answer::Accepted)),
        "tentative" => Ok((Partstat::Tentative, holtmail_compose::Answer::Tentative)),
        "declined" => Ok((Partstat::Declined, holtmail_compose::Answer::Declined)),
        _ => Err(ApiError::bad_request(
            "answer is accepted, tentative or declined",
        )),
    }
}

fn full(method: Option<&str>) -> bool {
    matches!(Method::of(method), Method::Request | Method::Publish)
}

/// What an answer is given to: the account's copy when one holds it, the event from mail, the
/// invitation's bytes and the mail it came in.
struct Target {
    archive: ArchiveId,
    copy: Option<(Collection, Effective)>,
    invitation: Bytes,
    message: Option<Message>,
}

enum Start {
    Item(ItemId),
    Message(Box<Message>),
}

async fn mail_invitation(
    state: &ApiState,
    edits: &holtmail_pim_sync::Edits,
    item: ItemId,
) -> ApiResult<Option<(Bytes, Option<Message>)>> {
    let versions = state.store.item_versions(item).await?;
    let Some(version) = versions
        .iter()
        .rev()
        .find(|v| full(v.mail.as_ref().and_then(|m| m.method.as_deref())))
        .or(versions.last())
    else {
        return Ok(None);
    };
    let Some(mut raw) = edits
        .version_text(version, ItemKind::Event)
        .await
        .map_err(|_| ApiError::not_found("invitation bytes"))?
    else {
        return Ok(None);
    };
    if !holtmail_pim_edit::holds_series(&raw) {
        let mut updates = vec![raw.clone()];
        for earlier in versions.iter().rev().filter(|v| v.number < version.number) {
            if !full(earlier.mail.as_ref().and_then(|m| m.method.as_deref())) {
                continue;
            }
            let Ok(Some(earlier)) = edits.version_text(earlier, ItemKind::Event).await else {
                continue;
            };
            if holtmail_pim_edit::holds_series(&earlier) {
                let later: Vec<&[u8]> = updates.iter().rev().map(|b| b.as_ref()).collect();
                raw = Bytes::from(holtmail_pim_edit::with_instances(&earlier, &later));
                break;
            }
            updates.push(earlier);
        }
    }
    let message = match state
        .store
        .version_messages(item)
        .await?
        .into_iter()
        .rev()
        .find(|s| s.number == version.number)
    {
        Some(source) => state.store.get_message(source.message_id).await?,
        None => None,
    };
    Ok(Some((raw, message)))
}

async fn resolve(state: &ApiState, user: &User, start: Start) -> ApiResult<Target> {
    let edits = crate::pim_edit::edits(state);
    let (summary, message) = match start {
        Start::Item(id) => {
            let summary = state
                .store
                .get_item(id)
                .await?
                .ok_or_else(|| ApiError::not_found("item"))?;
            crate::pim_edit::collection_of(state, user, summary.item.collection_id)
                .await
                .map_err(|_| ApiError::not_found("item"))?;
            (summary, None)
        }
        Start::Message(message) => {
            let mut found = None;
            for source in state.store.message_versions(message.id).await? {
                if let Some(summary) = state.store.get_item(source.item_id).await?
                    && summary.item.kind == holtmail_store::ItemKind::Event
                {
                    found = Some(summary);
                    break;
                }
            }
            let summary = found.ok_or_else(|| unprocessable("the message holds no invitation"))?;
            (summary, Some(*message))
        }
    };
    if summary.item.kind != holtmail_store::ItemKind::Event {
        return Err(unprocessable("only events can be answered"));
    }
    let archive = summary.item.archive_id;
    let mut members = vec![summary.clone()];
    if let Some(group) = holtmail_calendar::group_of(&summary) {
        for member in state.store.group_members(group).await? {
            if member.summary.item.id != summary.item.id
                && member.summary.item.archive_id == archive
            {
                members.push(member.summary);
            }
        }
    }
    let mut copy = None;
    let mut mail_item = None;
    for member in members {
        if member.item.removed_at.is_some() {
            continue;
        }
        let Some(collection) = state
            .store
            .get_collection(member.item.collection_id)
            .await?
        else {
            continue;
        };
        if collection.removed_at.is_some() || collection.kind != CollectionKind::Calendar {
            continue;
        }
        if collection.account_id.is_none()
            && collection.remote_id == holtmail_calendar::MAIL_COLLECTION
        {
            mail_item.get_or_insert(member.item.id);
        } else if collection.account_id.is_some()
            && collection.write_mode != WriteMode::ReadOnly
            && copy.is_none()
            && let Some(effective) = state
                .store
                .effective(&ChangeTarget::Item {
                    item: member.item.id,
                })
                .await?
            && effective.served
        {
            copy = Some((collection, effective));
        }
    }
    let from_mail = match mail_item {
        Some(item) => mail_invitation(state, &edits, item).await?,
        None => None,
    };
    let invitation = match &copy {
        Some((_, effective)) => edits
            .served(effective)
            .await
            .map_err(|_| ApiError::not_found("item bytes"))?
            .map(|s| s.raw),
        None => from_mail.as_ref().map(|(raw, _)| raw.clone()),
    }
    .ok_or_else(|| unprocessable("the invitation cannot be read"))?;
    let message = message.or(from_mail.and_then(|(_, m)| m));
    Ok(Target {
        archive,
        copy,
        invitation,
        message,
    })
}

/// The account that answers: the copy's, else the one the invitation mail was synced from.
async fn answering_account(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    target: &Target,
) -> ApiResult<Account> {
    let account = match (&target.copy, &target.message) {
        (Some((collection, _)), _) => collection.account_id,
        (None, Some(message)) => {
            let mut found = None;
            for location in state.store.locations(message.id).await? {
                if let Some(folder) = state.store.get_folder(location.folder_id).await? {
                    found = Some(folder.account_id);
                    if location.removed_at.is_none() {
                        break;
                    }
                }
            }
            found
        }
        (None, None) => None,
    };
    let account = account
        .ok_or_else(|| forbidden("the invitation came through no account of the archive"))?;
    may_write(state, user, scopes, account, Capability::PimRespond).await
}

/// The host and the capabilities open now; `None` when the collection's server cannot be
/// asked, so nobody can tell whether it would schedule.
async fn host_of(
    state: &ApiState,
    collection: &Collection,
    capability: Capability,
) -> Option<(Host, WriteCaps)> {
    let Scheduling { host, caps } = match state.mailboxes.scheduling(collection).await {
        SchedulingProbe::NoServer => return Some((Host::Mail, WriteCaps::default())),
        SchedulingProbe::Unknown => return None,
        SchedulingProbe::Known(known) => known,
    };
    let open = state.write.enabled
        && collection.write_mode == WriteMode::WriteThrough
        && match collection.account_id {
            Some(account) => state
                .store
                .account_capabilities(account)
                .await
                .is_ok_and(|c| c.has(capability)),
            None => false,
        };
    Some((host, if open { caps } else { WriteCaps::default() }))
}

/// The host an answer is routed for. An unknown server gets an iMIP reply and a held copy that
/// never asks the server to tell the organizer, which is right whether or not it schedules.
async fn answer_host(state: &ApiState, target: &Target) -> (Host, WriteCaps) {
    match &target.copy {
        Some((collection, _)) => host_of(state, collection, Capability::PimRespond)
            .await
            .unwrap_or((Host::Mail, WriteCaps::default())),
        None => (Host::Mail, WriteCaps::default()),
    }
}

/// What the user answers as: their active identities and the aliases linked to them.
struct Mine {
    identities: Vec<holtmail_store::Identity>,
    addresses: Vec<String>,
}

async fn mine(state: &ApiState, user: &User, archive: ArchiveId) -> ApiResult<Mine> {
    let identities: Vec<_> = state
        .store
        .user_identities(user.id, archive)
        .await?
        .into_iter()
        .filter(|i| i.fields.active)
        .collect();
    let addresses = identities
        .iter()
        .flat_map(|i| [Some(&i.fields.address), i.fields.alias.as_ref()])
        .flatten()
        .filter_map(|a| holtmail_pim::normalize_email(a))
        .collect();
    Ok(Mine {
        identities,
        addresses,
    })
}

impl Mine {
    fn identity_of(&self, address: &str) -> Option<&holtmail_store::Identity> {
        self.identities
            .iter()
            .find(|i| holtmail_pim::normalize_email(&i.fields.address).as_deref() == Some(address))
    }
}

/// The attendee line a copy's answer changes: the account's own address when it is invited,
/// else the first invited address of the user. In a shared archive that keeps one person from
/// answering as another.
fn copy_attendee(invited: &[String], account: &Account, mine: &Mine) -> Option<String> {
    account
        .address
        .as_deref()
        .and_then(holtmail_pim::normalize_email)
        .filter(|a| invited.contains(a))
        .or_else(|| invited.iter().find(|a| mine.addresses.contains(a)).cloned())
}

/// The refusal for an invitation addressed to an alias the user has no identity sending as:
/// answering would put the alias in `ATTENDEE` and the real mailbox in `From`.
fn alias_refused() -> ApiError {
    ApiError(
        StatusCode::CONFLICT,
        "this invitation was sent to an alias; answering from here would show the organizer your real mailbox".into(),
    )
}

fn attendees(raw: &[u8]) -> Vec<String> {
    match holtmail_pim::normalize(ItemKind::Event, raw) {
        holtmail_pim::Normalized::Calendar(calendar) => {
            let mut out: Vec<String> = Vec::new();
            for event in &calendar.components {
                for a in &event.attendees {
                    if let Some(email) = &a.email
                        && !out.contains(email)
                    {
                        out.push(email.clone());
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

/// The routing context of an answer: a reply to the invitation mail when there is one, so the
/// identity is the address it was sent to and an alias strategy applies; otherwise a reply to
/// the event's attendees as if they had been the mail's recipients.
async fn answer_routing(
    state: &ApiState,
    target: &Target,
    account: &Account,
) -> ApiResult<Routing> {
    if let Some(message) = &target.message {
        let raw = state.blobs.get(&message.blob_id).await.ok().flatten();
        let parsed = raw.as_deref().and_then(|r| holtmail_mime::parse(r).ok());
        return relay::routing_of(state, message, parsed.as_ref(), false, None).await;
    }
    let mut routing = relay::new_message(target.archive);
    routing.context = Context::Reply(holtmail_route::Original {
        recipients: attendees(&target.invitation),
        account: Some(account.id.to_string()),
        alias: None,
    });
    Ok(routing)
}

/// A message queued for sending, waiting for its audit entry before it is released.
pub(crate) struct Held {
    entry: OutboxEntry,
    account: Option<holtmail_core::AccountId>,
}

/// Queues `draft` from `routed` through `transport`, held until [`release`].
pub(crate) async fn hold(
    state: &ApiState,
    user: &User,
    archive: ArchiveId,
    route: holtmail_route::Route,
    transport: &Transport,
    draft: &holtmail_compose::Draft,
    key: Option<String>,
) -> ApiResult<Held> {
    let sending = sending(state)?;
    let built =
        holtmail_compose::build(draft).map_err(|error| ApiError::bad_request(error.to_string()))?;
    let entry = sending
        .outbox
        .queue(Queue {
            archive,
            user: Some(user.id),
            device: None,
            route,
            envelope: Envelope {
                mail_from: draft.from.address.clone(),
                recipients: built.recipients,
            },
            bytes: Bytes::from(built.bytes),
            send_at: None,
            hold: true,
            flow_run: None,
            idempotency_key: key,
        })
        .await
        .map_err(outbox_error)?;
    Ok(Held {
        entry,
        account: transport.account_id,
    })
}

/// Cancels messages [`hold`] queued when what they belong to could not be recorded.
pub(crate) async fn drop_held(state: &ApiState, held: Vec<Held>) {
    let Ok(sending) = sending(state) else {
        return;
    };
    for h in held {
        if h.entry.state == OutboxState::Draft {
            let _ = sending.outbox.cancel(h.entry.id).await;
        }
    }
}

/// The ids of held messages, to record with the change they belong to.
pub(crate) fn held_ids(held: &[Held]) -> Vec<holtmail_core::OutboxId> {
    held.iter().map(|h| h.entry.id).collect()
}

/// Audits and releases held messages into the undo window once what they belong to is
/// recorded; returns those released as the outbox lists them. The change exists by now, so a
/// failure here never takes it back: it is logged, every other message is still released, and a
/// message of a recorded change left held is released at the next start.
pub(crate) async fn release(
    state: &ApiState,
    user: &User,
    held: Vec<Held>,
    action: &str,
) -> Vec<OutboxEntry> {
    let Ok(sending) = sending(state) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(held.len());
    for h in held {
        if h.entry.state != OutboxState::Draft {
            out.push(h.entry);
            continue;
        }
        if let Err(error) = audit(
            state,
            user,
            AuditKind::OutboxChanged,
            Some(h.entry.new.archive_id),
            h.account,
            vec![h.entry.id.to_string(), action.to_owned()],
        )
        .await
        {
            tracing::warn!(outbox = %h.entry.id, error = %error.1, "release not audited");
        }
        match sending.outbox.release(h.entry.id, None).await {
            Ok(entry) => out.push(entry),
            Err(error) => tracing::warn!(outbox = %h.entry.id, %error, "held message not released"),
        }
    }
    out
}

async fn transport_for(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    archive: ArchiveId,
    routed: &Routed,
) -> ApiResult<Transport> {
    let transport = state
        .store
        .archive_transports(archive)
        .await?
        .into_iter()
        .find(|t| t.id.to_string() == routed.route.transport)
        .ok_or_else(|| ApiError::bad_request("the archive may not use the transport"))?;
    may_send(state, user, scopes, archive, &transport).await?;
    Ok(transport)
}

/// When a change whose reply is held in `held` may go upstream: after the reply's undo window,
/// so undoing the answer can still discard it.
fn not_before(held: &[Held]) -> Option<Timestamp> {
    held.iter()
        .map(|h| h.entry.new.send_at.as_millis())
        .max()
        .map(|at| Timestamp::from_millis(at + UNDO_MARGIN_MS))
}

/// How long after the end of the undo window an answered copy waits to go upstream.
const UNDO_MARGIN_MS: i64 = 5_000;

fn invitation_of(imip: &Imip, kind: ImipKind, comment: Option<&str>) -> Invitation {
    Invitation {
        kind,
        calendar: imip.calendar.clone(),
        summary: imip.summary.clone(),
        when: imip.when.clone(),
        location: imip.location.clone(),
        comment: comment
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned),
    }
}

fn idempotency(headers: &HeaderMap, prefix: &str) -> Option<String> {
    headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .map(|k| format!("{prefix}:{k}"))
}

async fn answer(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    headers: &HeaderMap,
    start: Start,
    request: &RsvpRequest,
) -> ApiResult<RsvpDto> {
    let (partstat, compose_answer) = answer_of(&request.answer)?;
    let target = resolve(state, user, start).await?;
    let account = answering_account(state, user, scopes, &target).await?;
    let own = crate::pim_edit::edits(state)
        .own_addresses(target.archive)
        .await?;
    let own: Vec<String> = own
        .iter()
        .filter_map(|a| holtmail_pim::normalize_email(a))
        .collect();
    if holtmail_pim_edit::organizer(&target.invitation).is_some_and(|o| own.contains(&o)) {
        return Err(unprocessable("you organize this event"));
    }
    let agent = holtmail_pim_edit::reply_agent(&target.invitation);
    let (host, caps) = answer_host(state, &target).await;
    let route = holtmail_pim_edit::reply_route(host, &caps, agent);
    let recurrence_id = request
        .recurrence_id
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let now = Timestamp::now().as_millis();
    let invited: Vec<String> = attendees(&target.invitation)
        .into_iter()
        .filter(|a| own.contains(a))
        .collect();

    let mine = mine(state, user, target.archive).await?;
    let mut held = Vec::new();
    let mut attendee = copy_attendee(&invited, &account, &mine);
    if let ReplyRoute::Imip { .. } = route {
        may_send_somehow(state, user, scopes, target.archive).await?;
        let mut routing = answer_routing(state, &target, &account).await?;
        let mut routed = relay::route_creating(state, user, &mut routing, None)
            .await?
            .map_err(|error: RouteError| ApiError::bad_request(error.to_string()))?;
        if !holtmail_pim::normalize_email(holtmail_route::attendee_address(&routed))
            .is_some_and(|a| invited.contains(&a))
        {
            let candidate = invited
                .iter()
                .find(|a| mine.addresses.contains(a))
                .ok_or_else(|| imip_error(ImipError::NotAttendee))?;
            let identity = mine.identity_of(candidate).ok_or_else(alias_refused)?;
            routed = relay::route(
                state,
                user,
                &relay::new_message(target.archive),
                Some(&identity.id.to_string()),
            )
            .await?
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        }
        let transport = transport_for(state, user, scopes, target.archive, &routed).await?;
        let used = holtmail_route::attendee_address(&routed).to_owned();
        let imip = holtmail_pim_edit::reply(
            &target.invitation,
            &used,
            partstat,
            recurrence_id,
            request.comment.as_deref(),
            now,
        )
        .map_err(imip_error)?;
        attendee = Some(used);
        let original = match &target.message {
            Some(message) => state
                .blobs
                .get(&message.blob_id)
                .await
                .ok()
                .flatten()
                .and_then(|raw| holtmail_mime::parse(&raw).ok())
                .map(|parsed| holtmail_compose::Original::from_parsed(&parsed)),
            None => None,
        };
        let to = routed.to.clone().unwrap_or_else(|| imip.organizer.clone());
        let draft = holtmail_compose::imip(
            Mailbox {
                name: routed.route.name.clone(),
                address: routed.route.from.clone(),
            },
            vec![Mailbox::new(to)],
            &invitation_of(
                &imip,
                ImipKind::Reply(compose_answer),
                request.comment.as_deref(),
            ),
            original.as_ref(),
        );
        held.push(
            hold(
                state,
                user,
                target.archive,
                routed.route.clone(),
                &transport,
                &draft,
                idempotency(headers, "rsvp"),
            )
            .await?,
        );
    }

    let (copy_word, change) = match (&target.copy, route) {
        (Some((collection, effective)), route) => {
            let attendee = match attendee.clone() {
                Some(a) => a,
                None => {
                    drop_held(state, held).await;
                    return Err(imip_error(ImipError::NotAttendee));
                }
            };
            let copy = match route {
                ReplyRoute::Server => None,
                ReplyRoute::Imip { copy } | ReplyRoute::Quiet { copy } => Some(copy),
            };
            let bytes = match holtmail_pim_edit::answered(
                &target.invitation,
                &attendee,
                partstat,
                recurrence_id,
            ) {
                Ok(bytes) => bytes,
                Err(error) => {
                    drop_held(state, held).await;
                    return Err(imip_error(error));
                }
            };
            let respond = Respond {
                answer: partstat.as_str().to_owned(),
                recurrence_id: recurrence_id.map(str::to_owned),
                notify: route == ReplyRoute::Server,
            };
            let recorded = crate::pim_edit::record_answer(
                state,
                user,
                &account,
                collection,
                effective,
                bytes,
                RecordAs {
                    respond: Some(respond),
                    audit: AuditKind::PimRespond,
                    not_before: not_before(&held),
                    messages: held_ids(&held),
                },
            )
            .await;
            let change = match recorded {
                Ok(change) => change,
                Err(error) => {
                    drop_held(state, held).await;
                    return Err(error);
                }
            };
            let word = match copy {
                None | Some(CopyUpdate::Respond) => "respond",
                Some(CopyUpdate::Put) => "put",
                Some(CopyUpdate::Held) => "held",
            };
            (word, Some(change))
        }
        (None, ReplyRoute::Quiet { .. }) => {
            return Err(unprocessable(
                "the organizer asked for no answers, and no account holds a copy to answer",
            ));
        }
        (None, _) => {
            if let Err(error) = audit(
                state,
                user,
                AuditKind::PimRespond,
                Some(target.archive),
                Some(account.id),
                Vec::new(),
            )
            .await
            {
                drop_held(state, held).await;
                return Err(error);
            }
            ("none", None)
        }
    };
    let released = release(state, user, held, "queued").await;
    let outbox = match released.first() {
        Some(entry) => Some(outbox_view(state, entry).await?),
        None => None,
    };
    Ok(RsvpDto {
        route: match route {
            ReplyRoute::Server => "server",
            ReplyRoute::Imip { .. } => "imip",
            ReplyRoute::Quiet { .. } => "quiet",
        }
        .into(),
        copy: copy_word.into(),
        change: change.as_ref().map(PimChangeDto::from),
        outbox,
    })
}

#[utoipa::path(
    post,
    path = "/api/items/{id}/rsvp",
    params(("id" = String, Path, description = "Event item id: an account's copy or the event from mail")),
    request_body = RsvpRequest,
    responses((status = 200, body = RsvpDto), (status = 400, description = "An unknown answer, no route for the reply, or a transport the archive may not use", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock: the instance switch, the account's pim.respond, the grant (editor, sender for an iMIP reply), the scope (write:pim, send:mail) or the transport's allowed senders", body = ErrorDto), (status = 404, description = "No such item", body = ErrorDto), (status = 409, description = "The copy changed since it was read", body = ErrorDto), (status = 422, description = "Not an invitation to the user: no attendee address of theirs, their own event, no such occurrence, or an organizer who asked for no answers", body = ErrorDto), (status = 503, description = "An iMIP reply is needed but the server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn rsvp_item(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RsvpRequest>,
) -> ApiResult<Json<RsvpDto>> {
    let item: ItemId = raw.parse().map_err(|_| ApiError::not_found("item"))?;
    let done = answer(
        &state,
        &user,
        &scopes,
        &headers,
        Start::Item(item),
        &request,
    )
    .await?;
    state.mailboxes.pim_changed(
        state
            .store
            .get_item(item)
            .await?
            .map(|s| s.item.archive_id)
            .ok_or_else(|| ApiError::not_found("item"))?,
    );
    Ok(Json(done))
}

#[utoipa::path(
    post,
    path = "/api/messages/{id}/rsvp",
    params(("id" = String, Path, description = "Id of a message carrying an invitation")),
    request_body = RsvpRequest,
    responses((status = 200, body = RsvpDto), (status = 400, description = "An unknown answer, no route for the reply, or a transport the archive may not use", body = ErrorDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 403, description = "A closed lock, as for an item", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto), (status = 409, description = "The copy changed since it was read", body = ErrorDto), (status = 422, description = "The message holds no invitation to the user", body = ErrorDto), (status = 503, description = "An iMIP reply is needed but the server does not send mail", body = ErrorDto)),
)]
pub(crate) async fn rsvp_message(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    Json(request): Json<RsvpRequest>,
) -> ApiResult<Json<RsvpDto>> {
    let message = crate::drafts::readable_message(&state, &user, &raw).await?;
    let archive = message.archive_id;
    let done = answer(
        &state,
        &user,
        &scopes,
        &headers,
        Start::Message(Box::new(message)),
        &request,
    )
    .await?;
    state.mailboxes.pim_changed(archive);
    Ok(Json(done))
}

/// What an organizer's change sends: the copy to record in place of the client's bytes and the
/// messages, held in the outbox until the change is recorded.
pub(crate) struct Organizing {
    /// The bytes to record (`SEQUENCE` raised, answers reset where RFC 5546 asks); `None` for
    /// a delete or when the change is not an organizer's.
    pub event: Option<Vec<u8>>,
    /// The held messages; release them once the change is recorded, drop them when it fails.
    pub held: Vec<Held>,
}

/// The organizer path of an event change in `collection` (`previous` what was served,
/// `next` what is recorded; `None` for a create or a delete). Nothing happens unless the
/// event names an `ORGANIZER` that is one of the user's identities and has attendees. Where
/// the server schedules, also once the held change reaches it (`Deferred`), only
/// `SCHEDULE-AGENT=CLIENT` attendees are mailed; otherwise every attendee gets a `REQUEST` or
/// `CANCEL` through the outbox from that identity. With `notify` false nothing is mailed, but
/// the copy is still adjusted. A server that cannot be asked is `503` when anyone would be
/// mailed, since mailing could double what it sends later. Sending locks are checked before
/// anything is queued.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn organize(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    headers: &HeaderMap,
    collection: &Collection,
    previous: Option<&[u8]>,
    next: Option<&[u8]>,
    notify: bool,
) -> ApiResult<Organizing> {
    let none = Organizing {
        event: next.map(<[u8]>::to_vec),
        held: Vec::new(),
    };
    let Some(organizer) = next
        .and_then(holtmail_pim_edit::organizer)
        .or_else(|| previous.and_then(holtmail_pim_edit::organizer))
    else {
        return Ok(none);
    };
    let archive = collection.archive_id;
    let identity = state
        .store
        .user_identities(user.id, archive)
        .await?
        .into_iter()
        .find(|i| holtmail_pim::normalize_email(&i.fields.address).as_deref() == Some(&organizer));
    let own_address = identity.is_some()
        || crate::pim_edit::edits(state)
            .own_addresses(archive)
            .await?
            .iter()
            .any(|a| holtmail_pim::normalize_email(a).as_deref() == Some(&organizer));
    if !own_address {
        return Ok(none);
    }
    let change = match (previous, next) {
        (None, _) => OrganizerChange::Create,
        (Some(_), None) => OrganizerChange::Delete,
        (Some(_), Some(_)) => OrganizerChange::Update,
    };
    let route = host_of(state, collection, Capability::PimEdit)
        .await
        .map(|(host, caps)| holtmail_pim_edit::organizer_route(host, &caps, change));
    let now = Timestamp::now().as_millis();
    let organized = holtmail_pim_edit::organize(
        previous,
        next,
        matches!(
            route,
            Some(OrganizerRoute::Server | OrganizerRoute::Deferred)
        ),
        now,
    )
    .map_err(imip_error)?;
    let event = organized.event.or(none.event);
    if !notify || organized.messages.is_empty() {
        return Ok(Organizing {
            event,
            held: Vec::new(),
        });
    }
    if route.is_none() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "the calendar's server cannot be reached, so Holtmail cannot tell who sends the invitations; try again, or save without invitations".into(),
        ));
    }
    let identity = identity.ok_or_else(|| {
        forbidden("none of your identities sends as the organizer; save without invitations")
    })?;
    may_send_somehow(state, user, scopes, archive).await?;
    let routing = relay::new_message(archive);
    let identity_id = identity.id.to_string();
    let routed = relay::route(state, user, &routing, Some(&identity_id))
        .await?
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let transport = transport_for(state, user, scopes, archive, &routed).await?;
    let mut held = Vec::new();
    for (index, imip) in organized.messages.iter().enumerate() {
        let kind = match (imip.method, change) {
            (Method::Cancel, _) => ImipKind::Cancel,
            (_, OrganizerChange::Create) => ImipKind::Invitation,
            _ => ImipKind::Update,
        };
        let draft = holtmail_compose::imip(
            Mailbox {
                name: routed.route.name.clone(),
                address: routed.route.from.clone(),
            },
            imip.recipients.iter().cloned().map(Mailbox::new).collect(),
            &invitation_of(imip, kind, None),
            None,
        );
        let key = idempotency(headers, &format!("organize-{index}"));
        match hold(
            state,
            user,
            archive,
            routed.route.clone(),
            &transport,
            &draft,
            key,
        )
        .await
        {
            Ok(h) => held.push(h),
            Err(error) => {
                drop_held(state, held).await;
                return Err(error);
            }
        }
    }
    Ok(Organizing { event, held })
}

/// The address and name a new event with invitees is organized as: the user's default
/// identity in the archive, else their first active one, else the account's address.
pub(crate) async fn organizer_identity(
    state: &ApiState,
    user: &User,
    account: &Account,
) -> ApiResult<Option<(String, Option<String>)>> {
    let identities = state
        .store
        .user_identities(user.id, account.archive_id)
        .await?;
    let default = state
        .store
        .default_identity(user.id, account.archive_id)
        .await?;
    let chosen = identities
        .iter()
        .find(|i| Some(i.id) == default && i.fields.active)
        .or_else(|| {
            identities
                .iter()
                .find(|i| i.fields.active && i.fields.account_id == Some(account.id))
        })
        .or_else(|| identities.iter().find(|i| i.fields.active));
    Ok(match chosen {
        Some(identity) => Some((
            identity.fields.address.clone(),
            identity.fields.name.clone(),
        )),
        None => account.address.clone().map(|a| (a, None)),
    })
}

/// Whether changes of `collection` can be an organizer's: a calendar.
pub(crate) fn is_event(collection: &Collection) -> bool {
    collection.kind == CollectionKind::Calendar
}

/// One occurrence of a repeating event that can be answered on its own.
#[derive(Debug, Serialize, ToSchema)]
pub struct RsvpOccurrenceDto {
    /// Pass back as `RsvpRequest.recurrence_id`.
    pub recurrence_id: String,
    /// The instance as the calendar names it (`CalendarOccurrence.recurrence_id`).
    pub key: String,
    /// Unix milliseconds.
    pub starts_at: i64,
    /// Unix milliseconds.
    pub ends_at: i64,
    /// The start as a wall-clock time in its own zone.
    pub start_local: String,
    pub time_zone: Option<String>,
    pub all_day: bool,
}

/// What answering an invitation would do now. Reading it records and sends nothing.
#[derive(Debug, Serialize, ToSchema)]
pub struct RsvpOptionsDto {
    /// Whether `POST .../rsvp` would be accepted now (it may still fail when something changes
    /// in between).
    pub allowed: bool,
    /// Why not, when `allowed` is false: `organizer` (one of the archive's own addresses
    /// organizes it), `not_invited` (none of them is an attendee, or none the user answers
    /// as), `locked` (a lock for `pim.respond` is closed), `no_answers` (the organizer asked
    /// for no answers and no account holds a copy), `open_edit` (an edit of the copy has not
    /// reached the account yet) or `cannot_send` (an iMIP reply is needed and the user may not
    /// send it, no identity routes it, or it would show the real mailbox behind an alias).
    pub reason: Option<String>,
    /// `server`, `imip` or `quiet`, as `RsvpDto.route` would say; absent for `organizer`,
    /// `not_invited` and `locked`.
    pub route: Option<String>,
    /// `respond`, `put`, `held` or `none`, as `RsvpDto.copy` would say.
    pub copy: String,
    /// The account's copy an answer is recorded on; absent when only "From mail" holds it.
    pub item_id: Option<String>,
    /// The calendar of that copy.
    pub collection_name: Option<String>,
    /// The account of that copy; for `server`, the one whose server tells the organizer.
    pub account_name: Option<String>,
    /// For `imip`: the address the reply is sent from, when the route needs no reverse alias
    /// made first.
    pub from: Option<String>,
    /// The attendee address the organizer sees: the alias for an invitation that came through
    /// one.
    pub attendee: Option<String>,
    /// For an `imip` reply through an alias service: its provider id, such as `simplelogin`.
    pub provider: Option<String>,
    /// The user's current answer on what would be answered, lowercase `PARTSTAT`.
    pub my_status: Option<String>,
    /// Whether the event repeats, so one occurrence can be answered by its `RECURRENCE-ID`.
    pub recurring: bool,
    /// The occurrences that can be answered one by one, by start: from a month ago to a year
    /// ahead, at most 60, without cancelled ones. Empty for an event that does not repeat.
    pub occurrences: Vec<RsvpOccurrenceDto>,
    /// The Local edit mark of the copy, when an open change is served on it.
    pub local_edit: Option<crate::pim_edit::LocalEditDto>,
}

fn route_word(route: ReplyRoute) -> &'static str {
    match route {
        ReplyRoute::Server => "server",
        ReplyRoute::Imip { .. } => "imip",
        ReplyRoute::Quiet { .. } => "quiet",
    }
}

/// How long before and after now a repeating event's occurrences are offered.
const OCCURRENCES_BEFORE_MS: i64 = 31 * 86_400_000;
const OCCURRENCES_AFTER_MS: i64 = 366 * 86_400_000;
const MAX_OCCURRENCES: usize = 60;

struct Read {
    recurring: bool,
    my_status: Option<String>,
    occurrences: Vec<RsvpOccurrenceDto>,
}

fn compact(value: &str) -> String {
    value.chars().filter(|c| *c != '-' && *c != ':').collect()
}

/// The `RECURRENCE-ID` of an occurrence as the series writes it: a changed instance's own, a
/// UTC series' in UTC, any other in the start's own zone.
fn written_id(
    calendar: &holtmail_pim::CalendarObject,
    master: &holtmail_pim::Event,
    occurrence: &holtmail_recur::Occurrence,
) -> Option<String> {
    let named = occurrence.recurrence_id.as_deref()?;
    if occurrence.changed {
        let time = calendar
            .components
            .get(occurrence.component)?
            .recurrence_id
            .as_ref()?;
        return Some(compact(&time.value));
    }
    let utc = master
        .start
        .as_ref()
        .is_some_and(|s| s.value.ends_with('Z'));
    Some(compact(if utc { named } else { &occurrence.start_local }))
}

fn read_invitation(raw: &[u8], own: &[String], now: i64) -> Read {
    let mut read = Read {
        recurring: false,
        my_status: None,
        occurrences: Vec::new(),
    };
    let holtmail_pim::Normalized::Calendar(calendar) =
        holtmail_pim::normalize(ItemKind::Event, raw)
    else {
        return read;
    };
    let Some(master) = calendar
        .components
        .iter()
        .find(|c| c.recurrence_id.is_none())
        .or(calendar.components.first())
    else {
        return read;
    };
    read.recurring = master.recurring;
    read.my_status = master
        .attendees
        .iter()
        .find(|a| a.email.as_ref().is_some_and(|e| own.contains(e)))
        .and_then(|a| a.partstat.as_ref())
        .map(|p| p.to_ascii_lowercase());
    if master.recurring {
        let window = holtmail_recur::Window {
            from: now - OCCURRENCES_BEFORE_MS,
            to: now + OCCURRENCES_AFTER_MS,
        };
        read.occurrences = holtmail_recur::expand(&calendar, window)
            .occurrences
            .iter()
            .filter(|o| !o.cancelled)
            .filter_map(|o| {
                Some(RsvpOccurrenceDto {
                    recurrence_id: written_id(&calendar, master, o)?,
                    key: o.recurrence_id.clone()?,
                    starts_at: o.start,
                    ends_at: o.end,
                    start_local: o.start_local.clone(),
                    time_zone: o.time_zone.clone(),
                    all_day: o.all_day,
                })
            })
            .take(MAX_OCCURRENCES)
            .collect();
    }
    read
}

async fn options(
    state: &ApiState,
    user: &User,
    scopes: &Scopes,
    start: Start,
) -> ApiResult<RsvpOptionsDto> {
    let target = resolve(state, user, start).await?;
    let own: Vec<String> = crate::pim_edit::edits(state)
        .own_addresses(target.archive)
        .await?
        .iter()
        .filter_map(|a| holtmail_pim::normalize_email(a))
        .collect();
    let read = read_invitation(&target.invitation, &own, Timestamp::now().as_millis());
    let mut dto = RsvpOptionsDto {
        allowed: false,
        reason: None,
        route: None,
        copy: "none".into(),
        item_id: None,
        collection_name: None,
        account_name: None,
        from: None,
        attendee: None,
        provider: None,
        my_status: read.my_status,
        recurring: read.recurring,
        occurrences: read.occurrences,
        local_edit: None,
    };
    if let Some((collection, effective)) = &target.copy {
        dto.item_id = effective.item.as_ref().map(|i| i.item.id.to_string());
        dto.collection_name = Some(collection.name.clone());
        dto.account_name = crate::pim_edit::account_name(state, collection.account_id).await;
        dto.local_edit =
            crate::pim_edit::local_edit(state, collection, effective.change.as_ref()).await;
    }
    let refuse = |mut dto: RsvpOptionsDto, reason: &str| {
        dto.reason = Some(reason.into());
        Ok(dto)
    };
    if holtmail_pim_edit::organizer(&target.invitation).is_some_and(|o| own.contains(&o)) {
        return refuse(dto, "organizer");
    }
    let invited: Vec<String> = attendees(&target.invitation)
        .into_iter()
        .filter(|a| own.contains(a))
        .collect();
    let Some(first) = invited.first().cloned() else {
        return refuse(dto, "not_invited");
    };
    let account = match answering_account(state, user, scopes, &target).await {
        Ok(account) => account,
        Err(ApiError(StatusCode::FORBIDDEN, _)) => return refuse(dto, "locked"),
        Err(error) => return Err(error),
    };
    let mine = mine(state, user, target.archive).await?;
    let agent = holtmail_pim_edit::reply_agent(&target.invitation);
    let (host, caps) = answer_host(state, &target).await;
    let route = holtmail_pim_edit::reply_route(host, &caps, agent);
    dto.route = Some(route_word(route).into());
    dto.copy = match (&target.copy, route) {
        (None, _) => "none",
        (Some(_), ReplyRoute::Server) => "respond",
        (Some(_), ReplyRoute::Imip { copy } | ReplyRoute::Quiet { copy }) => match copy {
            CopyUpdate::Respond => "respond",
            CopyUpdate::Put => "put",
            CopyUpdate::Held => "held",
        },
    }
    .into();
    dto.attendee = Some(first);
    if target.copy.is_none() && matches!(route, ReplyRoute::Quiet { .. }) {
        return refuse(dto, "no_answers");
    }
    if target.copy.as_ref().is_some_and(|(_, effective)| {
        effective
            .change
            .as_ref()
            .is_some_and(|c| c.class == holtmail_store::ChangeClass::Edit)
    }) {
        return refuse(dto, "open_edit");
    }
    if !matches!(route, ReplyRoute::Imip { .. }) {
        match copy_attendee(&invited, &account, &mine) {
            Some(address) => dto.attendee = Some(address),
            None if target.copy.is_some() => return refuse(dto, "not_invited"),
            None => {}
        }
    }
    if let ReplyRoute::Imip { .. } = route {
        if state.sending.is_none()
            || may_send_somehow(state, user, scopes, target.archive)
                .await
                .is_err()
        {
            return refuse(dto, "cannot_send");
        }
        let routing = answer_routing(state, &target, &account).await?;
        match relay::route(state, user, &routing, None).await? {
            Ok(routed) => {
                if transport_for(state, user, scopes, target.archive, &routed)
                    .await
                    .is_err()
                {
                    return refuse(dto, "cannot_send");
                }
                let preferred = holtmail_route::attendee_address(&routed).to_ascii_lowercase();
                if invited.contains(&preferred) {
                    dto.attendee = Some(preferred);
                    dto.from = Some(routed.route.from.clone());
                    dto.provider =
                        crate::drafts::route_dto(&routed, &routing).and_then(|r| r.provider);
                } else {
                    let Some(candidate) = invited.iter().find(|a| mine.addresses.contains(a))
                    else {
                        return refuse(dto, "not_invited");
                    };
                    let Some(identity) = mine.identity_of(candidate) else {
                        return refuse(dto, "cannot_send");
                    };
                    dto.attendee = Some(candidate.clone());
                    dto.from = Some(identity.fields.address.clone());
                }
            }
            Err(RouteError::NeedsReverseAlias { alias, .. }) => {
                if invited.contains(&alias) {
                    dto.attendee = Some(alias);
                }
                dto.provider = routing.relay.as_ref().map(|r| r.hit.provider.clone());
            }
            Err(_) => return refuse(dto, "cannot_send"),
        }
    }
    dto.allowed = true;
    Ok(dto)
}

#[utoipa::path(
    get,
    path = "/api/items/{id}/rsvp",
    params(("id" = String, Path, description = "Event item id: an account's copy or the event from mail")),
    responses((status = 200, body = RsvpOptionsDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such item", body = ErrorDto), (status = 422, description = "Not an event, or its invitation cannot be read", body = ErrorDto)),
)]
pub(crate) async fn rsvp_item_options(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<RsvpOptionsDto>> {
    let item: ItemId = raw.parse().map_err(|_| ApiError::not_found("item"))?;
    Ok(Json(
        options(&state, &user, &scopes, Start::Item(item)).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/api/messages/{id}/rsvp",
    params(("id" = String, Path, description = "Id of a message carrying an invitation")),
    responses((status = 200, body = RsvpOptionsDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such message", body = ErrorDto), (status = 422, description = "The message holds no invitation", body = ErrorDto)),
)]
pub(crate) async fn rsvp_message_options(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
) -> ApiResult<Json<RsvpOptionsDto>> {
    let message = crate::drafts::readable_message(&state, &user, &raw).await?;
    Ok(Json(
        options(&state, &user, &scopes, Start::Message(Box::new(message))).await?,
    ))
}

/// Who would send the invitations of an event the user organizes in a calendar. Reading it
/// records and sends nothing.
#[derive(Debug, Serialize, ToSchema)]
pub struct OrganizingDto {
    /// `server` (the calendar's server sends them when the change is written), `deferred` (it
    /// will once the change reaches the account), `imip` (Holtmail mails them from `from`),
    /// `unknown` (the server cannot be asked now, so saving with invitations is refused) or
    /// `none` (the event is organized by someone else, so saving it mails nobody).
    pub route: String,
    /// For `imip`: the address invitations are mailed from: the event's organizer, or for a
    /// new event the identity it would be organized as.
    pub from: Option<String>,
    /// The calendar's account.
    pub account_name: Option<String>,
    /// Whether invitations can go out as `route` says: false for `imip` when no identity of
    /// the user sends as `from` or the user may not send mail, and for `unknown`. Save with
    /// `invitations: false` then.
    pub allowed: bool,
}

/// The event an organizing preview is for.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OrganizingParams {
    /// An existing event of the calendar; absent for a new one.
    pub item: Option<String>,
}

#[utoipa::path(
    get,
    path = "/api/collections/{id}/organizing",
    params(("id" = String, Path, description = "Calendar id"), OrganizingParams),
    responses((status = 200, body = OrganizingDto), (status = 401, description = "Not signed in", body = ErrorDto), (status = 404, description = "No such calendar or item", body = ErrorDto), (status = 422, description = "Not a calendar of an account", body = ErrorDto)),
)]
pub(crate) async fn organizing(
    State(state): AppState,
    Extension(CurrentUser(user)): Extension<CurrentUser>,
    Extension(scopes): Extension<Scopes>,
    Path(raw): Path<String>,
    axum::extract::Query(params): axum::extract::Query<OrganizingParams>,
) -> ApiResult<Json<OrganizingDto>> {
    let id: holtmail_core::CollectionId =
        raw.parse().map_err(|_| ApiError::not_found("collection"))?;
    let collection = crate::pim_edit::collection_of(&state, &user, id).await?;
    let Some(account_id) = collection.account_id.filter(|_| is_event(&collection)) else {
        return Err(unprocessable("not a calendar of an account"));
    };
    let account = state
        .store
        .get_account(account_id)
        .await?
        .ok_or_else(|| ApiError::not_found("account"))?;
    let archive = collection.archive_id;
    let mut dto = OrganizingDto {
        route: "none".into(),
        from: None,
        account_name: Some(account.name.clone()),
        allowed: true,
    };
    let organizer = match &params.item {
        Some(item) => {
            let item: ItemId = item.parse().map_err(|_| ApiError::not_found("item"))?;
            let effective = state
                .store
                .effective(&ChangeTarget::Item { item })
                .await?
                .filter(|e| e.collection_id == collection.id)
                .ok_or_else(|| ApiError::not_found("item"))?;
            let served = crate::pim_edit::edits(&state)
                .served(&effective)
                .await
                .map_err(|_| ApiError::not_found("item bytes"))?;
            match served.and_then(|s| holtmail_pim_edit::organizer(&s.raw)) {
                Some(organizer) => organizer,
                None => match organizer_identity(&state, &user, &account).await? {
                    Some((address, _)) => address,
                    None => return Ok(Json(dto)),
                },
            }
        }
        None => match organizer_identity(&state, &user, &account).await? {
            Some((address, _)) => address,
            None => return Ok(Json(dto)),
        },
    };
    let organizer = holtmail_pim::normalize_email(&organizer).unwrap_or(organizer);
    let own = crate::pim_edit::edits(&state)
        .own_addresses(archive)
        .await?
        .iter()
        .any(|a| holtmail_pim::normalize_email(a).as_deref() == Some(&organizer));
    let identity = state
        .store
        .user_identities(user.id, archive)
        .await?
        .into_iter()
        .find(|i| holtmail_pim::normalize_email(&i.fields.address).as_deref() == Some(&organizer));
    if !own && identity.is_none() {
        return Ok(Json(dto));
    }
    let change = if params.item.is_some() {
        OrganizerChange::Update
    } else {
        OrganizerChange::Create
    };
    let Some((host, caps)) = host_of(&state, &collection, Capability::PimEdit).await else {
        dto.route = "unknown".into();
        dto.allowed = false;
        return Ok(Json(dto));
    };
    dto.route = match holtmail_pim_edit::organizer_route(host, &caps, change) {
        OrganizerRoute::Server => "server",
        OrganizerRoute::Deferred => "deferred",
        OrganizerRoute::Imip => "imip",
    }
    .into();
    if dto.route != "imip" {
        return Ok(Json(dto));
    }
    dto.from = Some(organizer);
    dto.allowed = match identity {
        Some(identity) if state.sending.is_some() => {
            let routed = relay::route(
                &state,
                &user,
                &relay::new_message(archive),
                Some(&identity.id.to_string()),
            )
            .await?;
            may_send_somehow(&state, &user, &scopes, archive)
                .await
                .is_ok()
                && match routed {
                    Ok(routed) => transport_for(&state, &user, &scopes, archive, &routed)
                        .await
                        .is_ok(),
                    Err(_) => false,
                }
        }
        _ => false,
    };
    Ok(Json(dto))
}
