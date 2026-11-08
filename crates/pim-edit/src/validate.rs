use holtmail_pim::{ItemKind, Normalized};

use crate::model::{Item, component_name, is_calendar_component};

/// The largest iCalendar object accepted: 2 MiB.
pub const MAX_ICALENDAR: usize = 2 * 1024 * 1024;
/// The largest vCard accepted: 5 MiB.
pub const MAX_VCARD: usize = 5 * 1024 * 1024;

/// Why bytes cannot be stored as a change. Each reason maps to the CalDAV or CardDAV
/// precondition a server names when it refuses a `PUT` with 403 ([`Invalid::precondition`]).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Invalid {
    /// Larger than [`MAX_ICALENDAR`] or [`MAX_VCARD`].
    #[error("larger than {limit} bytes")]
    TooLarge {
        /// The limit for the item's kind, in bytes.
        limit: usize,
    },
    /// Not one well-formed `VCALENDAR` or `VCARD` that `calcard` reads.
    #[error("not a readable iCalendar object or vCard")]
    Unreadable,
    /// A calendar object without a component of the item's kind, or with a component of
    /// another kind (an event where a task belongs).
    #[error("wrong component type")]
    WrongComponent,
    /// A component or the vCard has no `UID`.
    #[error("missing UID")]
    MissingUid,
    /// The components carry different `UID`s.
    #[error("more than one UID")]
    SeveralUids,
    /// Two components with the same `RECURRENCE-ID`, or two without one.
    #[error("the same instance twice")]
    DuplicateInstance,
    /// The `UID` differs from the one of the item it replaces.
    #[error("UID changed")]
    UidChanged,
}

impl Invalid {
    /// The precondition element of RFC 4791 §5.3.2.1 or RFC 6352 §6.3.2.1 for a calendar or
    /// address book of `kind`: `max-resource-size`, `no-uid-conflict`,
    /// `supported-calendar-component`, `valid-calendar-data` or `valid-address-data`.
    pub fn precondition(&self, kind: ItemKind) -> &'static str {
        match self {
            Invalid::TooLarge { .. } => "max-resource-size",
            Invalid::UidChanged => "no-uid-conflict",
            Invalid::WrongComponent if kind != ItemKind::Contact => "supported-calendar-component",
            _ if kind == ItemKind::Contact => "valid-address-data",
            _ => "valid-calendar-data",
        }
    }
}

/// What validation found in valid bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Valid {
    /// The item's `UID`.
    pub uid: String,
}

/// The size limit for an item of `kind`.
pub fn max_size(kind: ItemKind) -> usize {
    if kind == ItemKind::Contact {
        MAX_VCARD
    } else {
        MAX_ICALENDAR
    }
}

/// Checks that `raw` can be stored as an item of `kind` replacing `replaces` (`None` for a
/// create): within the size limit, one `VCALENDAR` (with only components of the kind, each
/// instance once) or one `VCARD`, readable by `calcard`, with one `UID` that equals the
/// replaced item's. A replaced item that is itself unreadable or has no `UID` imposes nothing.
/// Whether another item of the collection already has the `UID` is for the caller to check.
pub fn validate(kind: ItemKind, raw: &[u8], replaces: Option<&[u8]>) -> Result<Valid, Invalid> {
    let limit = max_size(kind);
    if raw.len() > limit {
        return Err(Invalid::TooLarge { limit });
    }
    let item = Item::parse(kind, raw).ok_or(Invalid::Unreadable)?;
    if !item.is_contact() {
        let want = component_name(kind);
        let mut found = false;
        for child in &item.top.children {
            if child.name == want {
                found = true;
            } else if is_calendar_component(&child.name) {
                return Err(Invalid::WrongComponent);
            }
        }
        if !found {
            return Err(Invalid::WrongComponent);
        }
        let mut keys: Vec<String> = item
            .components()
            .into_iter()
            .map(|c| item.component_key(c))
            .collect();
        let count = keys.len();
        keys.sort();
        keys.dedup();
        if keys.len() != count {
            return Err(Invalid::DuplicateInstance);
        }
    }
    let uid = single_uid(&item)?;
    if matches!(holtmail_pim::normalize(kind, raw), Normalized::Unreadable) {
        return Err(Invalid::Unreadable);
    }
    if let Some(old) = replaces.and_then(|b| Item::parse(kind, b))
        && let Ok(old_uid) = single_uid(&old)
        && old_uid != uid
    {
        return Err(Invalid::UidChanged);
    }
    Ok(Valid { uid })
}

pub(crate) fn single_uid(item: &Item) -> Result<String, Invalid> {
    let mut uids = item.uids().into_iter();
    let first = uids
        .next()
        .flatten()
        .filter(|u| !u.is_empty())
        .ok_or(Invalid::MissingUid)?;
    for uid in uids {
        match uid {
            None => return Err(Invalid::MissingUid),
            Some(uid) if uid != first => return Err(Invalid::SeveralUids),
            Some(_) => {}
        }
    }
    Ok(first)
}
