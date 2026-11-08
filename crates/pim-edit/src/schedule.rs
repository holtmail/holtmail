use holtmail_collection::WriteCaps;

/// Where an event lives, as far as scheduling goes: which kind of collection holds it and
/// whether that server sends iTIP messages itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Host {
    /// A CalDAV calendar; `auto_schedule` when the server advertises `calendar-auto-schedule`
    /// (RFC 6638) for it.
    CalDav {
        /// The server schedules: it tells organizers and attendees when an event is written.
        auto_schedule: bool,
    },
    /// A Microsoft Graph calendar, which always schedules.
    Graph,
    /// A JMAP calendar; `scheduling` when writes ask the server to send scheduling messages
    /// (`sendSchedulingMessages`) and it is known to do so.
    Jmap {
        /// Writing an event makes the server send its scheduling messages.
        scheduling: bool,
    },
    /// The "From mail" calendar: events known only from invitations in mail. No server.
    Mail,
}

impl Host {
    /// Whether writing an event here makes the server send the iTIP messages about it.
    pub fn schedules(self) -> bool {
        match self {
            Self::CalDav { auto_schedule } => auto_schedule,
            Self::Graph => true,
            Self::Jmap { scheduling } => scheduling,
            Self::Mail => false,
        }
    }
}

/// Who sends the iTIP messages for a calendar user, from the `SCHEDULE-AGENT` parameter
/// (RFC 6638 section 7.1): on `ORGANIZER` for an attendee's answers, on each `ATTENDEE` for the
/// organizer's messages to that attendee.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ScheduleAgent {
    /// The server (also when the parameter is missing or unknown).
    #[default]
    Server,
    /// The client: a scheduling server stays silent, so Holtmail sends by mail.
    Client,
    /// Nobody: no message is sent at all.
    None,
}

impl ScheduleAgent {
    /// Reads a parameter value, ignoring case; a missing or unknown value is
    /// [`ScheduleAgent::Server`], as RFC 6638 says.
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.trim().to_ascii_uppercase()).as_deref() {
            Some("CLIENT") => Self::Client,
            Some("NONE") => Self::None,
            _ => Self::Server,
        }
    }
}

/// How the account's own copy of an answered invitation follows the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopyUpdate {
    /// `CollectionWriter::respond` with `notify_organizer` off.
    Respond,
    /// A held `reply` change written with `CollectionWriter::put`: safe because this server
    /// does not schedule for it.
    Put,
    /// Recorded in Holtmail only: the copy upstream cannot change now ("From mail", writes off,
    /// or a collection that only takes answers through a route it does not offer).
    Held,
}

/// How an answer to an invitation reaches the organizer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReplyRoute {
    /// `CollectionWriter::respond` with `notify_organizer`: the server updates the copy and
    /// tells the organizer. No mail from Holtmail.
    Server,
    /// An iMIP `REPLY` through the outbox, with the identity the route picks; `copy` says how
    /// the account's copy follows.
    Imip {
        /// How the copy follows the answer.
        copy: CopyUpdate,
    },
    /// Nobody is told (`SCHEDULE-AGENT=NONE`); only the copy follows.
    Quiet {
        /// How the copy follows the answer.
        copy: CopyUpdate,
    },
}

/// The route for answering an invitation held by `host`, given what the account may write
/// there now (`caps`: pass `WriteCaps::default()` when the collection is not `write_through`
/// or the account lacks `pim.respond`) and the `SCHEDULE-AGENT` on the invitation's
/// `ORGANIZER` ([`crate::reply_agent`]).
///
/// Guarantees:
/// - the organizer is told exactly once: [`ReplyRoute::Server`] only where the server
///   schedules, `caps.respond` is on and the agent is the server; never an iMIP reply as well;
/// - every other answer with an agent other than `NONE` is [`ReplyRoute::Imip`], including
///   "From mail", writes being off and `SCHEDULE-AGENT=CLIENT`;
/// - a [`CopyUpdate::Put`] is chosen only for CalDAV where a `PUT` cannot make the server send
///   a second answer (no auto-scheduling, or an agent other than the server);
/// - the same inputs always give the same route.
pub fn reply_route(host: Host, caps: &WriteCaps, agent: ScheduleAgent) -> ReplyRoute {
    if agent == ScheduleAgent::Server && host.schedules() && caps.respond {
        return ReplyRoute::Server;
    }
    let copy = if caps.respond {
        CopyUpdate::Respond
    } else if caps.update
        && matches!(host, Host::CalDav { auto_schedule } if !auto_schedule || agent != ScheduleAgent::Server)
    {
        CopyUpdate::Put
    } else {
        CopyUpdate::Held
    };
    match agent {
        ScheduleAgent::None => ReplyRoute::Quiet { copy },
        _ => ReplyRoute::Imip { copy },
    }
}

/// What the organizer did to an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrganizerChange {
    /// A new event.
    Create,
    /// A change of an existing event, including cancelled instances and removed attendees.
    Update,
    /// The whole event deleted.
    Delete,
}

/// Who sends the invitations, updates and cancellations of an event the user organizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrganizerRoute {
    /// The server sends them when the change is written; Holtmail mails only attendees whose
    /// `SCHEDULE-AGENT` is `CLIENT` ([`crate::Audience::ClientScheduled`]).
    Server,
    /// The server would send them, but the change cannot be written now (the collection is
    /// held or writes are off): the server sends them when the change reaches the account, so
    /// attendees never get the same message twice. Holtmail mails only attendees whose
    /// `SCHEDULE-AGENT` is `CLIENT` ([`crate::Audience::ClientScheduled`]), whom the server never
    /// tells, and does so at once.
    Deferred,
    /// Holtmail sends `REQUEST` and `CANCEL` through the outbox to every attendee whose
    /// `SCHEDULE-AGENT` is not `NONE` ([`crate::Audience::Everyone`]).
    Imip,
}

/// The organizer path for `change` of an event in `host`, given what the account may write
/// there now (`caps`, `WriteCaps::default()` when the collection is not `write_through` or the
/// account lacks `pim.edit`).
///
/// Guarantees: [`OrganizerRoute::Server`] only where the server schedules and `caps` allows
/// the change (`create`, `update` or `delete`); [`OrganizerRoute::Deferred`] where it schedules
/// but the change cannot be written; [`OrganizerRoute::Imip`] wherever the server does not
/// schedule, whatever `caps` says. The same inputs always give the same route.
pub fn organizer_route(host: Host, caps: &WriteCaps, change: OrganizerChange) -> OrganizerRoute {
    if !host.schedules() {
        return OrganizerRoute::Imip;
    }
    let allowed = match change {
        OrganizerChange::Create => caps.create,
        OrganizerChange::Update => caps.update,
        OrganizerChange::Delete => caps.delete,
    };
    if allowed {
        OrganizerRoute::Server
    } else {
        OrganizerRoute::Deferred
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(respond: bool, update: bool) -> WriteCaps {
        WriteCaps {
            create: update,
            update,
            delete: update,
            respond,
            ..WriteCaps::default()
        }
    }

    const DAV_SCHEDULING: Host = Host::CalDav {
        auto_schedule: true,
    };
    const DAV_PLAIN: Host = Host::CalDav {
        auto_schedule: false,
    };

    #[test]
    fn scheduling_servers_answer_when_they_may() {
        let open = caps(true, true);
        for host in [DAV_SCHEDULING, Host::Graph, Host::Jmap { scheduling: true }] {
            assert_eq!(
                reply_route(host, &open, ScheduleAgent::Server),
                ReplyRoute::Server,
                "{host:?}"
            );
        }
    }

    #[test]
    fn everything_else_is_an_imip_reply_with_the_best_copy() {
        let none = WriteCaps::default();
        assert_eq!(
            reply_route(Host::Mail, &none, ScheduleAgent::Server),
            ReplyRoute::Imip {
                copy: CopyUpdate::Held
            }
        );
        assert_eq!(
            reply_route(Host::Graph, &none, ScheduleAgent::Server),
            ReplyRoute::Imip {
                copy: CopyUpdate::Held
            }
        );
        assert_eq!(
            reply_route(
                Host::Jmap { scheduling: false },
                &caps(true, true),
                ScheduleAgent::Server
            ),
            ReplyRoute::Imip {
                copy: CopyUpdate::Respond
            }
        );
        assert_eq!(
            reply_route(DAV_PLAIN, &caps(false, true), ScheduleAgent::Server),
            ReplyRoute::Imip {
                copy: CopyUpdate::Put
            }
        );
        assert_eq!(
            reply_route(DAV_SCHEDULING, &caps(false, true), ScheduleAgent::Server),
            ReplyRoute::Imip {
                copy: CopyUpdate::Held
            }
        );
    }

    #[test]
    fn schedule_agent_client_and_none_are_respected() {
        assert_eq!(
            reply_route(DAV_SCHEDULING, &caps(true, true), ScheduleAgent::Client),
            ReplyRoute::Imip {
                copy: CopyUpdate::Respond
            }
        );
        assert_eq!(
            reply_route(DAV_SCHEDULING, &caps(false, true), ScheduleAgent::Client),
            ReplyRoute::Imip {
                copy: CopyUpdate::Put
            }
        );
        assert_eq!(
            reply_route(Host::Graph, &caps(true, true), ScheduleAgent::None),
            ReplyRoute::Quiet {
                copy: CopyUpdate::Respond
            }
        );
        assert_eq!(
            reply_route(Host::Mail, &WriteCaps::default(), ScheduleAgent::None),
            ReplyRoute::Quiet {
                copy: CopyUpdate::Held
            }
        );
        assert_eq!(ScheduleAgent::parse(Some(" client")), ScheduleAgent::Client);
        assert_eq!(ScheduleAgent::parse(Some("NONE")), ScheduleAgent::None);
        assert_eq!(ScheduleAgent::parse(Some("X-OTHER")), ScheduleAgent::Server);
        assert_eq!(ScheduleAgent::parse(None), ScheduleAgent::Server);
    }

    #[test]
    fn organizer_path_follows_the_server() {
        let open = caps(true, true);
        let none = WriteCaps::default();
        for change in [
            OrganizerChange::Create,
            OrganizerChange::Update,
            OrganizerChange::Delete,
        ] {
            assert_eq!(
                organizer_route(DAV_SCHEDULING, &open, change),
                OrganizerRoute::Server
            );
            assert_eq!(
                organizer_route(Host::Graph, &none, change),
                OrganizerRoute::Deferred
            );
            assert_eq!(
                organizer_route(DAV_PLAIN, &open, change),
                OrganizerRoute::Imip
            );
            assert_eq!(
                organizer_route(Host::Jmap { scheduling: false }, &none, change),
                OrganizerRoute::Imip
            );
            assert_eq!(
                organizer_route(Host::Mail, &none, change),
                OrganizerRoute::Imip
            );
        }
        let update_only = WriteCaps {
            update: true,
            ..WriteCaps::default()
        };
        assert_eq!(
            organizer_route(Host::Graph, &update_only, OrganizerChange::Delete),
            OrganizerRoute::Deferred
        );
        assert_eq!(
            organizer_route(Host::Graph, &update_only, OrganizerChange::Update),
            OrganizerRoute::Server
        );
    }
}
