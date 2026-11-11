//! One client connection: sign-in, the selected mailbox and every command.

use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use holtmail_core::DeviceProtocol;
use holtmail_device::{DeviceAuthError, Principal, RevocationFeed};
use holtmail_mailview::{MailViewError, Member, Viewer};
use imap_next::server::{Error as ServerError, Event, Options};
use imap_next::types::CommandAuthenticate;
use imap_types::auth::{AuthMechanism, AuthenticateData};
use imap_types::command::{Command, CommandBody};
use imap_types::core::Atom;
use imap_types::core::{IString, NString, QuotedChar, Tag, Vec1};
use imap_types::extensions::binary::LiteralOrLiteral8;
use imap_types::extensions::namespace::Namespace;
use imap_types::fetch::MessageDataItem;
use imap_types::flag::FlagNameAttribute;
use imap_types::mailbox::Mailbox;
use imap_types::response::{Capability, Code, CommandContinuationRequest, Data, Greeting, Status};
use tokio::time::Instant;

use crate::auth::{self, Sasl, Step};
use crate::conn::{Conn, Incoming, Wire};
use crate::respond::{self, Kind, code, nz, nz64, tagged};
use crate::selected::Selected;
use crate::{ChangeListener, INSTANCE_ID_FIELD, Shared, names};

mod mailboxes;
mod messages;

/// The largest non-synchronising literal taken (`LITERAL-`, RFC 7888).
const LITERAL_MINUS: usize = 4096;

/// The most a command may hold before sign-in; `LOGIN` and `AUTHENTICATE` never need more.
pub(crate) const PRE_AUTH_COMMAND: usize = 8 << 10;

const KEYWORDS: &[&str] = &[
    "\\Answered",
    "\\Flagged",
    "\\Deleted",
    "\\Seen",
    "\\Draft",
    "$Forwarded",
    "$MDNSent",
    "$Junk",
    "$NotJunk",
];

/// Runs one connection to its end.
pub(crate) async fn run<W: Wire>(shared: Arc<Shared>, wire: W) {
    let mut session = Session {
        shared,
        auth: None,
        sasl: None,
        ended: false,
    };
    let capabilities = session.capabilities(wire.can_start_tls(), wire.sign_in_allowed());
    let greeting = Greeting::ok(Some(Code::Capability(capabilities)), "Holtmail ready")
        .expect("fixed greeting");
    let mut options = Options::default();
    options.crlf_relaxed = true;
    let _ = options.set_literal_accept_text("Ready for the literal".into());
    let _ = options.set_literal_reject_text("The literal is larger than the server takes".into());
    options.max_literal_size = session.shared.settings.max_literal;
    options.max_command_size = session
        .shared
        .settings
        .max_literal
        .saturating_add(crate::MAX_LINE);
    let mut conn = Conn::new(wire, options, greeting);
    if let Err(error) = session.drive(&mut conn).await {
        tracing::debug!(%error, "imap connection ended");
    }
    if let Some(auth) = &session.auth {
        tracing::debug!(device = %auth.principal.device, "imap session closed");
    }
}

struct Authed {
    principal: Principal,
    viewer: Viewer,
    feed: Mutex<Box<dyn RevocationFeed>>,
    listener: Mutex<Box<dyn ChangeListener>>,
    listening: bool,
    condstore: bool,
    qresync: bool,
    selected: Option<Selected>,
    append_limit: u64,
}

struct Session {
    shared: Arc<Shared>,
    auth: Option<Authed>,
    sasl: Option<(Tag<'static>, Sasl)>,
    ended: bool,
}

enum Wake {
    Read(bool),
    Revoked,
    Timeout,
    Changed,
    Keepalive,
    Poll,
    TooLong,
}

async fn revoked(auth: &mut Option<Authed>) {
    match auth {
        Some(auth) => {
            let device = auth.principal.device;
            let feed = auth.feed.get_mut().unwrap_or_else(PoisonError::into_inner);
            while feed.next().await != device {}
        }
        None => std::future::pending().await,
    }
}

async fn feed_revoked(feed: &mut Mutex<Box<dyn RevocationFeed>>, device: holtmail_core::DeviceId) {
    let feed = feed.get_mut().unwrap_or_else(PoisonError::into_inner);
    while feed.next().await != device {}
}

async fn changed(listener: &mut Mutex<Box<dyn ChangeListener>>, listening: &mut bool) {
    if !*listening {
        return std::future::pending().await;
    }
    let listener = listener.get_mut().unwrap_or_else(PoisonError::into_inner);
    if listener.changed().await.is_none() {
        *listening = false;
        std::future::pending::<()>().await;
    }
}

fn view_error(tag: &Tag<'static>, error: MailViewError) -> Status<'static> {
    match error {
        MailViewError::NotFound(_) => tagged(
            tag,
            Kind::No,
            Some(code("NONEXISTENT")),
            "No such mailbox or message",
        ),
        MailViewError::Building { done, total } => tagged(
            tag,
            Kind::No,
            Some(code("UNAVAILABLE")),
            format!("The mailbox is being prepared ({done} of {total}), try again soon"),
        ),
        MailViewError::TooLarge { limit } => tagged(
            tag,
            Kind::No,
            Some(Code::TooBig),
            format!("The message is larger than the {limit} bytes allowed"),
        ),
        MailViewError::TooMany { limit } => tagged(
            tag,
            Kind::No,
            Some(code("LIMIT")),
            format!("The search matches more than {limit} messages, narrow it down"),
        ),
        MailViewError::Invalid(reason) => tagged(tag, Kind::Bad, None, reason),
        MailViewError::CannotCalculateChanges | MailViewError::Unavailable(_) => {
            tracing::warn!(%error, "mail view unavailable");
            tagged(
                tag,
                Kind::No,
                Some(code("UNAVAILABLE")),
                "Temporary failure, try again later",
            )
        }
    }
}

fn mailbox_name(mailbox: &Mailbox<'_>) -> String {
    match mailbox {
        Mailbox::Inbox => "INBOX".into(),
        Mailbox::Other(other) => names::decode(other.as_ref()),
    }
}

fn wire_mailbox(name: &str) -> Mailbox<'static> {
    Mailbox::try_from(names::encode(name)).unwrap_or(Mailbox::Inbox)
}

fn attribute(name: &str) -> FlagNameAttribute<'static> {
    FlagNameAttribute::from(Atom::try_from(name.to_string()).expect("attribute atom"))
}

fn has_flag(flags: &[String], flag: &str) -> bool {
    flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
}

fn literal_data<'a>(message: &'a LiteralOrLiteral8<'_>) -> (&'a [u8], bool) {
    match message {
        LiteralOrLiteral8::Literal(literal) => (
            literal.data(),
            literal.mode() == imap_types::core::LiteralMode::NonSync,
        ),
        LiteralOrLiteral8::Literal8(literal) => (literal.data.as_ref(), false),
    }
}

impl Session {
    fn capabilities(
        &self,
        can_start_tls: bool,
        sign_in_allowed: bool,
    ) -> Vec1<Capability<'static>> {
        let mut names: Vec<String> = vec!["IMAP4rev1".into()];
        match &self.auth {
            None => {
                if can_start_tls {
                    names.push("STARTTLS".into());
                }
                if sign_in_allowed {
                    names.push("AUTH=PLAIN".into());
                    names.push("AUTH=LOGIN".into());
                } else {
                    names.push("LOGINDISABLED".into());
                }
                for name in ["SASL-IR", "LITERAL-", "ID", "ENABLE", "IDLE"] {
                    names.push(name.into());
                }
            }
            Some(auth) => {
                for name in [
                    "IDLE",
                    "UIDPLUS",
                    "MOVE",
                    "UNSELECT",
                    "ENABLE",
                    "CONDSTORE",
                    "QRESYNC",
                    "LITERAL-",
                    "SASL-IR",
                    "ID",
                    "NAMESPACE",
                    "SPECIAL-USE",
                    "CHILDREN",
                ] {
                    names.push(name.into());
                }
                names.push(format!("APPENDLIMIT={}", auth.append_limit));
            }
        }
        let capabilities: Vec<Capability<'static>> =
            names.iter().map(|n| respond::capability(n)).collect();
        Vec1::try_from(capabilities).expect("IMAP4rev1 is always there")
    }

    async fn drive<W: Wire>(&mut self, conn: &mut Conn<W>) -> io::Result<()> {
        let sign_in_by = tokio::time::Instant::now() + self.shared.settings.sign_in_timeout;
        loop {
            if self.ended {
                return conn.flush().await;
            }
            match conn.step().await? {
                Some(incoming) => self.incoming(conn, incoming).await?,
                None => {
                    if self.auth.is_none() && conn.unfinished() > PRE_AUTH_COMMAND {
                        conn.status(respond::bye("The command is too long before sign-in"));
                        return conn.flush().await;
                    }
                    let deadline = match self.auth {
                        Some(_) => tokio::time::Instant::now() + self.shared.settings.idle_timeout,
                        None => sign_in_by,
                    };
                    let wake = tokio::select! {
                        read = conn.read_more() => Wake::Read(read?),
                        () = revoked(&mut self.auth) => Wake::Revoked,
                        () = tokio::time::sleep_until(deadline) => Wake::Timeout,
                    };
                    match wake {
                        Wake::Read(true) => {}
                        Wake::Read(false) => return Ok(()),
                        Wake::Revoked => {
                            conn.status(respond::bye("This device's access ended"));
                            return conn.flush().await;
                        }
                        _ => {
                            conn.status(respond::bye("Autologout; idle for too long"));
                            return conn.flush().await;
                        }
                    }
                }
            }
        }
    }

    async fn incoming<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        incoming: Incoming,
    ) -> io::Result<()> {
        match incoming {
            Incoming::Error(error) => {
                self.bad_input(conn, error);
                Ok(())
            }
            Incoming::Event(event) => match *event {
                Event::CommandReceived { command } => self.command(conn, command).await,
                Event::CommandAuthenticateReceived {
                    command_authenticate,
                } => {
                    self.authenticate(conn, command_authenticate).await;
                    Ok(())
                }
                Event::AuthenticateDataReceived { authenticate_data } => {
                    self.authenticate_data(conn, authenticate_data).await;
                    Ok(())
                }
                Event::IdleCommandReceived { tag } => self.idle(conn, tag).await,
                _ => Ok(()),
            },
        }
    }

    fn bad_input<W: Wire>(&mut self, conn: &mut Conn<W>, error: ServerError) {
        let (bytes, text) = match &error {
            ServerError::MalformedMessage { discarded_bytes } => {
                (Some(discarded_bytes), "Could not parse the command")
            }
            ServerError::ExpectedCrlfGotLf { discarded_bytes } => {
                (Some(discarded_bytes), "Lines must end with CRLF")
            }
            ServerError::CommandTooLong { discarded_bytes } => {
                (Some(discarded_bytes), "The command is too long")
            }
            ServerError::LiteralTooLong { .. } => return,
        };
        let tag = bytes.and_then(|bytes| {
            let bytes = bytes.declassify();
            let end = bytes.iter().position(|b| *b == b' ')?;
            Tag::try_from(bytes[..end].to_vec()).ok()
        });
        match tag {
            Some(tag) => conn.status(tagged(&tag, Kind::Bad, None, text)),
            None => conn.status(Status::bad(None, None, text).expect("fixed text")),
        }
    }

    async fn command<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        command: Command<'static>,
    ) -> io::Result<()> {
        let tag = command.tag;
        let body = command.body;
        match body {
            CommandBody::Capability => {
                let capabilities =
                    self.capabilities(conn.wire.can_start_tls(), conn.wire.sign_in_allowed());
                conn.data(Data::Capability(capabilities));
                conn.status(tagged(&tag, Kind::Ok, None, "CAPABILITY completed"));
            }
            CommandBody::Noop | CommandBody::Check => {
                self.refresh(conn, true).await;
                conn.status(tagged(&tag, Kind::Ok, None, "Done"));
            }
            CommandBody::Logout => {
                conn.status(respond::bye("Logging out"));
                conn.status(tagged(&tag, Kind::Ok, None, "LOGOUT completed"));
                self.ended = true;
            }
            CommandBody::Id { .. } => {
                let mut fields: Vec<(IString<'static>, NString<'static>)> = vec![(
                    IString::try_from("name").expect("fixed"),
                    NString::try_from("Holtmail").expect("fixed"),
                )];
                if let Some(token) = &self.shared.settings.instance_token
                    && let Ok(value) = NString::try_from(token.clone())
                {
                    fields.push((IString::try_from(INSTANCE_ID_FIELD).expect("fixed"), value));
                }
                conn.data(Data::Id {
                    parameters: Some(fields),
                });
                conn.status(tagged(&tag, Kind::Ok, None, "ID completed"));
            }
            CommandBody::StartTLS => {
                if self.auth.is_some() || !conn.wire.can_start_tls() {
                    conn.status(tagged(&tag, Kind::Bad, None, "STARTTLS is not available"));
                } else {
                    conn.status(tagged(&tag, Kind::Ok, None, "Begin TLS negotiation now"));
                    conn.flush().await?;
                    if !conn.start_tls().await {
                        self.ended = true;
                        return Err(io::Error::other("TLS handshake failed"));
                    }
                }
            }
            CommandBody::Login { username, password } => {
                if self.auth.is_some() {
                    conn.status(tagged(&tag, Kind::Bad, None, "Already signed in"));
                } else {
                    let login = String::from_utf8_lossy(username.as_ref()).into_owned();
                    let secret =
                        String::from_utf8_lossy(password.declassify().as_ref()).into_owned();
                    let status = self.sign_in(conn, &tag, &login, &secret).await;
                    conn.status(status);
                }
            }
            body if self.auth.is_none() => {
                drop(body);
                conn.status(tagged(&tag, Kind::Bad, None, "Sign in first"));
            }
            CommandBody::Enable { capabilities } => self.enable(conn, &tag, capabilities),
            CommandBody::Namespace => {
                let personal = vec![Namespace::new(
                    IString::try_from("").expect("fixed"),
                    QuotedChar::try_from(names::DELIMITER).ok(),
                )];
                conn.data(Data::Namespace {
                    personal,
                    other: Vec::new(),
                    shared: Vec::new(),
                });
                conn.status(tagged(&tag, Kind::Ok, None, "NAMESPACE completed"));
            }
            CommandBody::List {
                reference,
                mailbox_wildcard,
            } => {
                self.list(conn, &tag, &reference, &mailbox_wildcard, false)
                    .await
            }
            CommandBody::Lsub {
                reference,
                mailbox_wildcard,
            } => {
                self.list(conn, &tag, &reference, &mailbox_wildcard, true)
                    .await
            }
            CommandBody::Status {
                mailbox,
                item_names,
            } => self.status(conn, &tag, &mailbox, &item_names).await,
            CommandBody::Select {
                mailbox,
                parameters,
            } => self.select(conn, &tag, &mailbox, parameters, false).await,
            CommandBody::Examine {
                mailbox,
                parameters,
            } => self.select(conn, &tag, &mailbox, parameters, true).await,
            CommandBody::Subscribe { mailbox } => self.subscribe(conn, &tag, &mailbox, true).await,
            CommandBody::Unsubscribe { mailbox } => {
                self.subscribe(conn, &tag, &mailbox, false).await
            }
            CommandBody::Create { .. }
            | CommandBody::Delete { .. }
            | CommandBody::Rename { .. } => {
                conn.status(tagged(
                    &tag,
                    Kind::No,
                    Some(code("CANNOT")),
                    "Mailboxes come from the accounts and cannot be changed here",
                ));
            }
            CommandBody::Append {
                mailbox,
                flags,
                date,
                message,
            } => {
                self.append(conn, &tag, &mailbox, flags, date, message)
                    .await
            }
            body if self.selected().is_none() => {
                drop(body);
                conn.status(tagged(&tag, Kind::Bad, None, "Select a mailbox first"));
            }
            CommandBody::Unselect => {
                self.deselect();
                conn.status(tagged(&tag, Kind::Ok, None, "UNSELECT completed"));
            }
            CommandBody::Close => {
                self.close().await;
                conn.status(tagged(&tag, Kind::Ok, None, "CLOSE completed"));
            }
            CommandBody::Expunge => self.expunge(conn, &tag, None).await,
            CommandBody::ExpungeUid { sequence_set } => {
                self.expunge(conn, &tag, Some(&sequence_set)).await
            }
            CommandBody::Search {
                charset,
                criteria,
                uid,
            } => {
                self.search(conn, &tag, charset, criteria.as_ref(), uid)
                    .await
            }
            CommandBody::Fetch {
                sequence_set,
                macro_or_item_names,
                uid,
                modifiers,
            } => {
                self.fetch(
                    conn,
                    &tag,
                    &sequence_set,
                    macro_or_item_names,
                    uid,
                    &modifiers,
                )
                .await?
            }
            CommandBody::Store {
                sequence_set,
                kind,
                response,
                flags,
                uid,
                modifiers,
            } => {
                self.store(
                    conn,
                    &tag,
                    &sequence_set,
                    kind,
                    response,
                    &flags,
                    uid,
                    &modifiers,
                )
                .await
            }
            CommandBody::Copy {
                sequence_set,
                mailbox,
                uid,
            } => {
                self.transfer(conn, &tag, &sequence_set, &mailbox, uid, false)
                    .await
            }
            CommandBody::Move {
                sequence_set,
                mailbox,
                uid,
            } => {
                self.transfer(conn, &tag, &sequence_set, &mailbox, uid, true)
                    .await
            }
            _ => conn.status(tagged(&tag, Kind::Bad, None, "Not supported")),
        }
        Ok(())
    }

    fn selected(&self) -> Option<&Selected> {
        self.auth.as_ref().and_then(|a| a.selected.as_ref())
    }

    fn deselect(&mut self) {
        if let Some(auth) = &mut self.auth {
            auth.selected = None;
        }
    }

    async fn sign_in<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: &Tag<'static>,
        login: &str,
        secret: &str,
    ) -> Status<'static> {
        if !conn.wire.sign_in_allowed() {
            return tagged(
                tag,
                Kind::No,
                Some(code("PRIVACYREQUIRED")),
                "Sign in only over TLS",
            );
        }
        let devices = self.shared.devices.clone();
        let feed = devices.revocations();
        let principal = match devices
            .verify(login, secret, conn.wire.client(), DeviceProtocol::Imap)
            .await
        {
            Ok(principal) => principal,
            Err(DeviceAuthError::TooMany { .. }) => {
                return tagged(
                    tag,
                    Kind::No,
                    Some(code("UNAVAILABLE")),
                    "Too many failed sign-ins, try again later",
                );
            }
            Err(DeviceAuthError::Unavailable(error)) => {
                tracing::warn!(%error, "device sign-in unavailable");
                return tagged(
                    tag,
                    Kind::No,
                    Some(code("UNAVAILABLE")),
                    "Sign-in is unavailable, try again later",
                );
            }
            Err(DeviceAuthError::Invalid | DeviceAuthError::Revoked) => {
                return tagged(
                    tag,
                    Kind::No,
                    Some(code("AUTHENTICATIONFAILED")),
                    "Wrong login or app password",
                );
            }
        };
        let Some(archive) = principal.mail_archive else {
            return tagged(
                tag,
                Kind::No,
                Some(code("AUTHENTICATIONFAILED")),
                "This device has no mail archive",
            );
        };
        if !conn.wire.authenticated(&principal.user) {
            return tagged(
                tag,
                Kind::No,
                Some(code("LIMIT")),
                "Too many connections for this user",
            );
        }
        let viewer = Viewer {
            user: principal.user,
            device: Some(principal.device),
            archive,
            scopes: principal.scopes.clone(),
        };
        let max = u64::from(self.shared.settings.max_literal);
        let append_limit = self
            .shared
            .actions
            .append_limit(&viewer)
            .await
            .map_or(max, |limit| limit.min(max));
        let listener = self.shared.signal.listen(archive, principal.user);
        tracing::info!(device = %principal.device, "imap sign-in");
        self.auth = Some(Authed {
            principal,
            viewer,
            feed: Mutex::new(feed),
            listener: Mutex::new(listener),
            listening: true,
            condstore: false,
            qresync: false,
            selected: None,
            append_limit,
        });
        let capabilities =
            self.capabilities(conn.wire.can_start_tls(), conn.wire.sign_in_allowed());
        tagged(
            tag,
            Kind::Ok,
            Some(Code::Capability(capabilities)),
            "Signed in",
        )
    }

    async fn authenticate<W: Wire>(&mut self, conn: &mut Conn<W>, command: CommandAuthenticate) {
        let tag = command.tag;
        if self.auth.is_some() {
            conn.authenticate_finish(tagged(&tag, Kind::Bad, None, "Already signed in"));
            return;
        }
        if !conn.wire.sign_in_allowed() {
            conn.authenticate_finish(tagged(
                &tag,
                Kind::No,
                Some(code("PRIVACYREQUIRED")),
                "Sign in only over TLS",
            ));
            return;
        }
        let initial = command
            .initial_response
            .map(|response| response.declassify().to_vec());
        let state = match command.mechanism {
            AuthMechanism::Plain => Sasl::Plain,
            AuthMechanism::Login => Sasl::LoginUser,
            _ => {
                conn.authenticate_finish(tagged(
                    &tag,
                    Kind::No,
                    None,
                    "Only PLAIN and LOGIN are offered",
                ));
                return;
            }
        };
        match initial {
            Some(data) => self.sasl_step(conn, tag, state, &data).await,
            None => {
                let challenge: &'static [u8] = match state {
                    Sasl::LoginUser => auth::USERNAME,
                    _ => b"",
                };
                self.sasl = Some((tag, state));
                conn.authenticate_continue(CommandContinuationRequest::base64(challenge));
            }
        }
    }

    async fn authenticate_data<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        data: AuthenticateData<'static>,
    ) {
        let Some((tag, state)) = self.sasl.take() else {
            return;
        };
        match data {
            AuthenticateData::Cancel => {
                conn.authenticate_finish(tagged(&tag, Kind::Bad, None, "Authentication cancelled"));
            }
            AuthenticateData::Continue(data) => {
                let data = data.declassify().to_vec();
                self.sasl_step(conn, tag, state, &data).await;
            }
        }
    }

    async fn sasl_step<W: Wire>(
        &mut self,
        conn: &mut Conn<W>,
        tag: Tag<'static>,
        state: Sasl,
        data: &[u8],
    ) {
        match state.step(data) {
            Step::Challenge(challenge, next) => {
                self.sasl = Some((tag, next));
                conn.authenticate_continue(CommandContinuationRequest::base64(challenge));
            }
            Step::Credentials { login, secret } => {
                let status = self.sign_in(conn, &tag, &login, &secret).await;
                conn.authenticate_finish(status);
            }
            Step::Invalid => {
                conn.authenticate_finish(tagged(
                    &tag,
                    Kind::No,
                    Some(code("AUTHENTICATIONFAILED")),
                    "Wrong login or app password",
                ));
            }
        }
    }

    async fn idle<W: Wire>(&mut self, conn: &mut Conn<W>, tag: Tag<'static>) -> io::Result<()> {
        if self.auth.is_none() {
            conn.idle_reject(tagged(&tag, Kind::Bad, None, "Sign in first"));
            return Ok(());
        }
        conn.idle_accept(
            CommandContinuationRequest::basic(None, "idling").expect("fixed continuation"),
        );
        self.refresh(conn, true).await;
        conn.flush().await?;
        let settings = self.shared.settings.clone();
        let until = Instant::now() + settings.idle_limit;
        let mut keepalive = Instant::now() + settings.keepalive;
        let mut poll = Instant::now() + settings.poll;
        loop {
            match conn.step().await? {
                Some(Incoming::Event(event)) => {
                    if matches!(*event, Event::IdleDoneReceived) {
                        self.refresh(conn, true).await;
                        conn.status(tagged(&tag, Kind::Ok, None, "IDLE terminated"));
                        return Ok(());
                    }
                }
                Some(Incoming::Error(_)) => {}
                None => {
                    let auth = self.auth.as_mut().expect("signed in");
                    let device = auth.principal.device;
                    let wake = tokio::select! {
                        read = conn.read_more() => Wake::Read(read?),
                        () = changed(&mut auth.listener, &mut auth.listening) => Wake::Changed,
                        () = feed_revoked(&mut auth.feed, device) => Wake::Revoked,
                        () = tokio::time::sleep_until(keepalive) => Wake::Keepalive,
                        () = tokio::time::sleep_until(poll) => Wake::Poll,
                        () = tokio::time::sleep_until(until) => Wake::TooLong,
                    };
                    match wake {
                        Wake::Read(true) => {}
                        Wake::Read(false) => {
                            self.ended = true;
                            return Ok(());
                        }
                        Wake::Changed | Wake::Poll => {
                            poll = Instant::now() + settings.poll;
                            self.refresh(conn, true).await;
                            conn.flush().await?;
                        }
                        Wake::Keepalive => {
                            keepalive = Instant::now() + settings.keepalive;
                            conn.status(respond::untagged_ok(None, "Still here"));
                            conn.flush().await?;
                        }
                        Wake::TooLong | Wake::Timeout => {
                            conn.status(respond::bye("IDLE for too long"));
                            self.ended = true;
                            return Ok(());
                        }
                        Wake::Revoked => {
                            conn.status(respond::bye("This device's access ended"));
                            self.ended = true;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// An unsolicited or `QRESYNC` flag update.
fn flags_fetch(seq: u32, member: &Member, flags: &[String], modseq: bool) -> Data<'static> {
    let mut items = vec![
        MessageDataItem::Uid(nz(member.uid)),
        MessageDataItem::Flags(respond::fetch_flags(flags)),
    ];
    if modseq {
        items.push(MessageDataItem::ModSeq(nz64(member.modseq)));
    }
    Data::Fetch {
        seq: nz(seq),
        items: Vec1::try_from(items).expect("not empty"),
    }
}
