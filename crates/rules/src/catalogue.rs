use schemars::{Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::params::*;

/// What starts a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TriggerKind {
    /// A live message was archived; never backfills, imports, resyncs or rebuilds.
    #[serde(rename = "message.archived")]
    MessageArchived,
    /// A message is about to be sent; its actions change the route.
    #[serde(rename = "message.sending")]
    MessageSending,
    /// A live message carried a calendar invitation.
    #[serde(rename = "invitation.received")]
    InvitationReceived,
    /// An event starts within the trigger's lead time.
    #[serde(rename = "event.upcoming")]
    EventUpcoming,
    /// A cron schedule fired.
    #[serde(rename = "schedule")]
    Schedule,
    /// A person ran the flow on a selection or a search.
    #[serde(rename = "manual")]
    Manual,
}

impl TriggerKind {
    /// Every trigger, in catalogue order.
    pub const ALL: [TriggerKind; 6] = [
        Self::MessageArchived,
        Self::MessageSending,
        Self::InvitationReceived,
        Self::EventUpcoming,
        Self::Schedule,
        Self::Manual,
    ];

    /// The catalogue id, such as `message.archived`.
    pub fn id(self) -> &'static str {
        match self {
            Self::MessageArchived => "message.archived",
            Self::MessageSending => "message.sending",
            Self::InvitationReceived => "invitation.received",
            Self::EventUpcoming => "event.upcoming",
            Self::Schedule => "schedule",
            Self::Manual => "manual",
        }
    }

    /// The trigger with this catalogue id.
    pub fn from_id(id: &str) -> Option<TriggerKind> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }
}

/// The account capability an action needs, named as the account capability it maps to.
///
/// Actions without one are local: they change nothing on a mail server and send nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    /// Flags on the server.
    #[serde(rename = "mail.flags")]
    MailFlags,
    /// Moving between folders.
    #[serde(rename = "mail.move")]
    MailMove,
    /// Moving to Trash.
    #[serde(rename = "mail.delete")]
    MailDelete,
    /// Sending through an identity, and changing how a message is sent.
    #[serde(rename = "mail.send")]
    MailSend,
    /// Creating contacts and events upstream.
    #[serde(rename = "pim.edit")]
    PimEdit,
    /// Answering invitations upstream.
    #[serde(rename = "pim.respond")]
    PimRespond,
}

/// The part a node type plays in a flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Starts a run; only in [`crate::Flow::trigger`].
    Trigger,
    /// Picks the `yes` or `no` port and changes nothing.
    Condition,
    /// Plans something to do, then goes on through `next`.
    Action,
}

/// A node type's name and description in one language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Text {
    /// Short name for the palette.
    pub title: &'static str,
    /// What it does, in a sentence or two.
    pub summary: &'static str,
}

/// A node type's documentation in every language the interface has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Docs {
    /// English.
    pub en: Text,
    /// Russian.
    pub ru: Text,
}

/// A node's parameters after they were checked against its type.
#[derive(Debug, Clone, PartialEq)]
#[allow(missing_docs)]
pub enum Spec {
    MessageArchived,
    MessageSending,
    InvitationReceived,
    EventUpcoming(UpcomingParams),
    Schedule(ScheduleParams),
    Manual,
    Query(QueryParams),
    Header(HeaderParams),
    Alias(AliasParams),
    Trust(TrustParams),
    Insight(InsightParams),
    SenderIsPerson,
    HasInvitation,
    Counter(CounterParams),
    TimeWindow(TimeWindowParams),
    Label(LabelParams),
    SetFlags(FlagsParams),
    Move(MoveParams),
    Delete,
    Reply(ReplyParams),
    Forward(ForwardParams),
    Send(SendParams),
    Rsvp(RsvpParams),
    CreateEvent(CreateEventParams),
    CreateContact(CreateContactParams),
    Notify(NotifyParams),
    Webhook(WebhookParams),
    Stop,
    SetIdentity(IdentityParams),
    SetTransport(TransportParams),
    SetAliasStrategy(StrategyParams),
    AddBcc(BccParams),
    RequireConfirmation(ReasonParams),
    Block(ReasonParams),
}

/// One node type of the catalogue. The editor, the API's validation and the docs page are all
/// made from these, so what they show is what [`crate::evaluate`] accepts.
#[derive(Clone, Copy)]
pub struct NodeType {
    /// Stable id stored in flows.
    pub id: &'static str,
    /// The newest parameter version; flows with a newer one are refused.
    pub version: u32,
    /// Trigger, condition or action.
    pub role: Role,
    /// Output ports in drawing order: `yes`, `no` for conditions, `next` for actions and
    /// triggers, none for `stop`.
    pub ports: &'static [&'static str],
    /// The capability the action needs; `None` for local actions, conditions and triggers.
    pub capability: Option<Capability>,
    /// The triggers a flow may have for this node to appear in it.
    pub triggers: &'static [TriggerKind],
    /// Parameter fields that are templates.
    pub templates: &'static [&'static str],
    /// Documentation.
    pub docs: Docs,
    schema: fn() -> Schema,
    parse: fn(Value) -> Result<Spec, serde_json::Error>,
}

impl std::fmt::Debug for NodeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeType").field("id", &self.id).finish()
    }
}

impl NodeType {
    /// The JSON Schema of the parameters.
    pub fn schema(&self) -> Schema {
        (self.schema)()
    }

    /// Checks parameters against the type. `null` reads as `{}`.
    pub fn parse(&self, params: &Value) -> Result<Spec, serde_json::Error> {
        let params = if params.is_null() {
            Value::Object(serde_json::Map::new())
        } else {
            params.clone()
        };
        (self.parse)(params)
    }

    /// Whether `confirm` makes sense on it: actions that change or send something.
    pub fn confirmable(&self) -> bool {
        self.role == Role::Action && self.capability.is_some()
    }
}

/// A node type as the API and the editor receive it.
#[derive(Debug, Clone, Serialize)]
pub struct NodeTypeDoc {
    /// Stable id.
    pub id: &'static str,
    /// Newest parameter version.
    pub version: u32,
    /// Trigger, condition or action.
    pub role: Role,
    /// Output ports.
    pub ports: &'static [&'static str],
    /// Needed capability.
    pub capability: Option<Capability>,
    /// Triggers it may follow.
    pub triggers: &'static [TriggerKind],
    /// Template fields.
    pub templates: &'static [&'static str],
    /// Whether `confirm` is offered.
    pub confirmable: bool,
    /// Documentation.
    pub docs: Docs,
    /// JSON Schema of the parameters.
    pub schema: Schema,
}

const ALL: &[TriggerKind] = &TriggerKind::ALL;
const MESSAGE: &[TriggerKind] = &[
    TriggerKind::MessageArchived,
    TriggerKind::MessageSending,
    TriggerKind::InvitationReceived,
    TriggerKind::Manual,
];
const STORED: &[TriggerKind] = &[
    TriggerKind::MessageArchived,
    TriggerKind::InvitationReceived,
    TriggerKind::Manual,
];
const NOT_SENDING: &[TriggerKind] = &[
    TriggerKind::MessageArchived,
    TriggerKind::InvitationReceived,
    TriggerKind::EventUpcoming,
    TriggerKind::Schedule,
    TriggerKind::Manual,
];
const SENDING: &[TriggerKind] = &[TriggerKind::MessageSending];

const YES_NO: &[&str] = &["yes", "no"];
const NEXT: &[&str] = &["next"];

macro_rules! node {
    ($id:literal, $role:ident, $ports:expr, $cap:expr, $triggers:expr, $templates:expr,
     $params:ty, $variant:expr, $en:expr, $ru:expr) => {
        NodeType {
            id: $id,
            version: 1,
            role: Role::$role,
            ports: $ports,
            capability: $cap,
            triggers: $triggers,
            templates: $templates,
            docs: Docs {
                en: Text {
                    title: $en.0,
                    summary: $en.1,
                },
                ru: Text {
                    title: $ru.0,
                    summary: $ru.1,
                },
            },
            schema: || schema_for!($params),
            parse: |v| serde_json::from_value::<$params>(v).map($variant),
        }
    };
}

static CATALOGUE: &[NodeType] = &[
    node!(
        "message.archived",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        NoParams,
        |_| Spec::MessageArchived,
        (
            "Message archived",
            "A new message arrived in the archive. Runs only for live mail, never for backfills, imports, resyncs or rebuilds."
        ),
        (
            "Письмо в архиве",
            "В архив пришло новое письмо. Срабатывает только на живую почту, никогда на догрузку, импорт, пересинхронизацию или перестройку."
        )
    ),
    node!(
        "message.sending",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        NoParams,
        |_| Spec::MessageSending,
        (
            "Message sending",
            "A message is about to be sent. Its actions change how it is sent: identity, transport, alias strategy, Bcc, confirmation or a block."
        ),
        (
            "Отправка письма",
            "Письмо вот-вот уйдёт. Действия меняют способ отправки: личность, транспорт, стратегию алиаса, скрытую копию, подтверждение или запрет."
        )
    ),
    node!(
        "invitation.received",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        NoParams,
        |_| Spec::InvitationReceived,
        (
            "Invitation received",
            "A live message carried a calendar invitation."
        ),
        (
            "Пришло приглашение",
            "Живое письмо принесло приглашение в календарь."
        )
    ),
    node!(
        "event.upcoming",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        UpcomingParams,
        Spec::EventUpcoming,
        (
            "Event upcoming",
            "An event in the archive's calendars starts within the given number of minutes."
        ),
        (
            "Скоро событие",
            "Событие из календарей архива начнётся через указанное число минут."
        )
    ),
    node!(
        "schedule",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        ScheduleParams,
        Spec::Schedule,
        (
            "Schedule",
            "Runs on a cron schedule: minute, hour, day of month, month and day of week, in the chosen time zone."
        ),
        (
            "По расписанию",
            "Запускается по расписанию cron: минута, час, день месяца, месяц и день недели, в выбранном часовом поясе."
        )
    ),
    node!(
        "manual",
        Trigger,
        NEXT,
        None,
        ALL,
        &[],
        NoParams,
        |_| Spec::Manual,
        (
            "Run by hand",
            "A person runs the flow on selected messages or on a search."
        ),
        (
            "Вручную",
            "Человек запускает сценарий на выбранных письмах или на результатах поиска."
        )
    ),
    node!(
        "query",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        QueryParams,
        Spec::Query,
        (
            "Search matches",
            "The message matches a search written in the search language: from:, has:attachment, category:, alias: and the rest."
        ),
        (
            "Подходит под поиск",
            "Письмо подходит под поиск на языке поиска: from:, has:attachment, category:, alias: и остальное."
        )
    ),
    node!(
        "header",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        HeaderParams,
        Spec::Header,
        (
            "Header",
            "A header field exists, equals, contains, starts or ends with a text. Case is ignored."
        ),
        (
            "Заголовок",
            "Поле заголовка есть, равно тексту, содержит его, начинается или заканчивается им. Регистр не важен."
        )
    ),
    node!(
        "alias",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        AliasParams,
        Spec::Alias,
        (
            "Came through an alias",
            "The message reached the archive through an alias, optionally of one service."
        ),
        (
            "Пришло через алиас",
            "Письмо пришло в архив через алиас, по желанию только одного сервиса."
        )
    ),
    node!(
        "trust",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        TrustParams,
        Spec::Trust,
        (
            "Sender trust",
            "The sender's trust is one of the chosen states: aligned, relayed, unverified or unknown."
        ),
        (
            "Доверие к отправителю",
            "Доверие к отправителю в одном из выбранных состояний: подтверждён, переслан, не подтверждён или неизвестно."
        )
    ),
    node!(
        "insight",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        InsightParams,
        Spec::Insight,
        (
            "Has insight",
            "The message has an insight of the given kind, such as a code or a parcel."
        ),
        (
            "Есть находка",
            "В письме есть находка указанного вида, например код или посылка."
        )
    ),
    node!(
        "sender_is_person",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        NoParams,
        |_| Spec::SenderIsPerson,
        (
            "Sender is a person",
            "The sender's address belongs to a person in People."
        ),
        (
            "Отправитель — человек",
            "Адрес отправителя принадлежит человеку из раздела «Люди»."
        )
    ),
    node!(
        "has_invitation",
        Condition,
        YES_NO,
        None,
        STORED,
        &[],
        NoParams,
        |_| Spec::HasInvitation,
        (
            "Has invitation",
            "The message carries a calendar invitation."
        ),
        ("Есть приглашение", "Письмо несёт приглашение в календарь.")
    ),
    node!(
        "counter",
        Condition,
        YES_NO,
        None,
        MESSAGE,
        &[],
        CounterParams,
        Spec::Counter,
        (
            "Counter",
            "More than a number of messages from this sender, or runs of this flow, in the last hour, day or week."
        ),
        (
            "Счётчик",
            "Больше заданного числа писем от этого отправителя или запусков этого сценария за последний час, день или неделю."
        )
    ),
    node!(
        "time_window",
        Condition,
        YES_NO,
        None,
        ALL,
        &[],
        TimeWindowParams,
        Spec::TimeWindow,
        (
            "Time window",
            "The event happened on one of the chosen days between two times, in the chosen time zone."
        ),
        (
            "Время",
            "Событие случилось в один из выбранных дней между двумя моментами времени в выбранном часовом поясе."
        )
    ),
    node!(
        "label",
        Action,
        NEXT,
        None,
        STORED,
        &[],
        LabelParams,
        Spec::Label,
        (
            "Label",
            "Puts a local label on the message. Changes nothing on the mail server."
        ),
        (
            "Метка",
            "Ставит письму локальную метку. На почтовом сервере ничего не меняется."
        )
    ),
    node!(
        "set_flags",
        Action,
        NEXT,
        Some(Capability::MailFlags),
        STORED,
        &[],
        FlagsParams,
        Spec::SetFlags,
        (
            "Set flags",
            "Sets and clears seen, flagged and answered on the mail server."
        ),
        (
            "Флаги",
            "Ставит и снимает флаги «прочитано», «помечено» и «отвечено» на почтовом сервере."
        )
    ),
    node!(
        "move",
        Action,
        NEXT,
        Some(Capability::MailMove),
        STORED,
        &[],
        MoveParams,
        Spec::Move,
        (
            "Move",
            "Moves the message to a folder on the mail server. The archive keeps it."
        ),
        (
            "Переместить",
            "Перемещает письмо в папку на почтовом сервере. В архиве оно остаётся."
        )
    ),
    node!(
        "delete",
        Action,
        NEXT,
        Some(Capability::MailDelete),
        STORED,
        &[],
        NoParams,
        |_| Spec::Delete,
        (
            "Delete on server",
            "Moves the message to Trash on the mail server. The archive keeps it."
        ),
        (
            "Удалить на сервере",
            "Перемещает письмо в корзину на почтовом сервере. В архиве оно остаётся."
        )
    ),
    node!(
        "reply",
        Action,
        NEXT,
        Some(Capability::MailSend),
        STORED,
        &["subject", "body"],
        ReplyParams,
        Spec::Reply,
        (
            "Auto-reply",
            "Replies to the sender from a template, at most once per sender in the given days. Never replies to lists, bulk or automatic mail, no-reply addresses or your own addresses (RFC 3834)."
        ),
        (
            "Автоответ",
            "Отвечает отправителю по шаблону, не чаще раза в заданное число дней на отправителя. Не отвечает на рассылки, массовые и автоматические письма, адреса no-reply и ваши собственные адреса (RFC 3834)."
        )
    ),
    node!(
        "forward",
        Action,
        NEXT,
        Some(Capability::MailSend),
        STORED,
        &["note"],
        ForwardParams,
        Spec::Forward,
        (
            "Forward",
            "Forwards the message to fixed addresses through an identity, quoted or as an attachment."
        ),
        (
            "Переслать",
            "Пересылает письмо на заданные адреса через личность, цитатой или вложением."
        )
    ),
    node!(
        "send",
        Action,
        NEXT,
        Some(Capability::MailSend),
        NOT_SENDING,
        &["subject", "body"],
        SendParams,
        Spec::Send,
        (
            "Send message",
            "Sends a new message from a template through an identity."
        ),
        (
            "Отправить письмо",
            "Отправляет новое письмо по шаблону через личность."
        )
    ),
    node!(
        "rsvp",
        Action,
        NEXT,
        Some(Capability::PimRespond),
        STORED,
        &["comment"],
        RsvpParams,
        Spec::Rsvp,
        (
            "Answer invitation",
            "Accepts, tentatively accepts or declines the invitation the message carries."
        ),
        (
            "Ответить на приглашение",
            "Принимает, принимает под вопросом или отклоняет приглашение из письма."
        )
    ),
    node!(
        "create_event",
        Action,
        NEXT,
        Some(Capability::PimEdit),
        NOT_SENDING,
        &["title", "starts"],
        CreateEventParams,
        Spec::CreateEvent,
        (
            "Create event",
            "Creates an event in a calendar; the start is a template that gives an RFC 3339 time."
        ),
        (
            "Создать событие",
            "Создаёт событие в календаре; начало — шаблон, дающий время в формате RFC 3339."
        )
    ),
    node!(
        "create_contact",
        Action,
        NEXT,
        Some(Capability::PimEdit),
        STORED,
        &[],
        CreateContactParams,
        Spec::CreateContact,
        (
            "Create contact",
            "Adds the message's sender to an address book."
        ),
        (
            "Создать контакт",
            "Добавляет отправителя письма в адресную книгу."
        )
    ),
    node!(
        "notify",
        Action,
        NEXT,
        None,
        ALL,
        &["title", "body"],
        NotifyParams,
        Spec::Notify,
        ("Notify", "Sends a notification to your devices."),
        ("Уведомить", "Присылает уведомление на ваши устройства.")
    ),
    node!(
        "webhook",
        Action,
        NEXT,
        None,
        ALL,
        &[],
        WebhookParams,
        Spec::Webhook,
        (
            "Call webhook",
            "Calls one of the archive's webhooks with the event."
        ),
        (
            "Вызвать вебхук",
            "Вызывает один из вебхуков архива с этим событием."
        )
    ),
    node!(
        "stop",
        Action,
        &[],
        None,
        ALL,
        &[],
        NoParams,
        |_| Spec::Stop,
        (
            "Stop",
            "Ends this run and skips the flows after this one. Nothing is removed from the archive."
        ),
        (
            "Стоп",
            "Завершает этот запуск и пропускает следующие сценарии. Из архива ничего не удаляется."
        )
    ),
    node!(
        "set_identity",
        Action,
        NEXT,
        Some(Capability::MailSend),
        SENDING,
        &[],
        IdentityParams,
        Spec::SetIdentity,
        (
            "Send as identity",
            "Sends the outgoing message as another identity."
        ),
        (
            "Отправить от личности",
            "Отправляет исходящее письмо от другой личности."
        )
    ),
    node!(
        "set_transport",
        Action,
        NEXT,
        Some(Capability::MailSend),
        SENDING,
        &[],
        TransportParams,
        Spec::SetTransport,
        (
            "Use transport",
            "Sends the outgoing message through another transport."
        ),
        (
            "Через транспорт",
            "Отправляет исходящее письмо через другой транспорт."
        )
    ),
    node!(
        "set_alias_strategy",
        Action,
        NEXT,
        Some(Capability::MailSend),
        SENDING,
        &[],
        StrategyParams,
        Spec::SetAliasStrategy,
        (
            "Alias strategy",
            "Routes a message to an alias contact directly, through the reverse alias or to its Reply-To."
        ),
        (
            "Стратегия алиаса",
            "Отправляет письмо контакту за алиасом напрямую, через обратный алиас или на его Reply-To."
        )
    ),
    node!(
        "add_bcc",
        Action,
        NEXT,
        Some(Capability::MailSend),
        SENDING,
        &[],
        BccParams,
        Spec::AddBcc,
        (
            "Add Bcc",
            "Adds a blind copy recipient to the outgoing message."
        ),
        (
            "Скрытая копия",
            "Добавляет получателя скрытой копии к исходящему письму."
        )
    ),
    node!(
        "require_confirmation",
        Action,
        NEXT,
        None,
        SENDING,
        &[],
        ReasonParams,
        Spec::RequireConfirmation,
        (
            "Ask before sending",
            "Holds the outgoing message until the sender confirms it, showing the reason."
        ),
        (
            "Спросить перед отправкой",
            "Задерживает исходящее письмо, пока отправитель его не подтвердит, и показывает причину."
        )
    ),
    node!(
        "block",
        Action,
        NEXT,
        None,
        SENDING,
        &[],
        ReasonParams,
        Spec::Block,
        (
            "Block sending",
            "Refuses to send the outgoing message, showing the reason."
        ),
        (
            "Запретить отправку",
            "Не даёт отправить исходящее письмо и показывает причину."
        )
    ),
];

/// Every node type, triggers first, then conditions, then actions.
pub fn catalogue() -> &'static [NodeType] {
    CATALOGUE
}

/// The node type with this id.
pub fn node_type(id: &str) -> Option<&'static NodeType> {
    CATALOGUE.iter().find(|t| t.id == id)
}

/// The catalogue as the API serves it, with each schema.
pub fn describe() -> Vec<NodeTypeDoc> {
    CATALOGUE
        .iter()
        .map(|t| NodeTypeDoc {
            id: t.id,
            version: t.version,
            role: t.role,
            ports: t.ports,
            capability: t.capability,
            triggers: t.triggers,
            templates: t.templates,
            confirmable: t.confirmable(),
            docs: t.docs,
            schema: t.schema(),
        })
        .collect()
}
