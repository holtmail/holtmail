import type { Locale } from './strings-writing.ts'

type Unit = 'week' | 'day' | 'hour' | 'minute'

function span(minutes: number): [number, Unit] {
  if (minutes % 10080 === 0) return [minutes / 10080, 'week']
  if (minutes % 1440 === 0) return [minutes / 1440, 'day']
  if (minutes % 60 === 0) return [minutes / 60, 'hour']
  return [minutes, 'minute']
}

const ruUnits: Record<Unit, [string, string, string]> = {
  week: ['неделю', 'недели', 'недель'],
  day: ['день', 'дня', 'дней'],
  hour: ['час', 'часа', 'часов'],
  minute: ['минуту', 'минуты', 'минут'],
}

function ruPlural(n: number, one: string, few: string, many: string): string {
  const mod10 = n % 10
  const mod100 = n % 100
  if (mod10 === 1 && mod100 !== 11) return one
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return few
  return many
}

const en = {
  mark: {
    label: 'Local edit',
    held: (account: string) => `Not sent to ${account}`,
    sending: 'Sending',
    conflict: (account: string) => `Changed in ${account} too`,
    rejected: (account: string) => `${account} refused`,
    account: 'the account',
  },
  reasons: {
    overlap: 'You and the account changed the same fields.',
    reply_obsolete: 'The invitation changed after your answer.',
    upstream_deleted: 'It was deleted in the account.',
    upstream_changed: 'It was changed in the account after your edit.',
    upstream_unreadable: "The account's version cannot be read.",
    refused: 'The account refused the change.',
  } as Record<string, string>,
  ops: { create: 'New', update: 'Change', delete: 'Delete' } as Record<string, string>,
  classes: {
    noop: 'No change',
    personal: 'Reminders only, kept in Holtmail',
    reply: 'Answer',
    edit: 'Edit',
    create: 'New item',
    delete: 'Delete',
  } as Record<string, string>,
  states: {
    held: 'Held',
    sending: 'Sending',
    applied: 'Applied',
    superseded: 'Replaced by a later edit',
    conflict: 'Conflict',
    rejected: 'Refused',
    discarded: 'Discarded',
  } as Record<string, string>,
  actions: {
    edit: 'Edit',
    delete: 'Delete',
    newEvent: 'New event',
    newContact: 'New contact',
    addSender: 'Add sender to contacts',
    editCard: 'Edit card',
    keep_mine: 'Keep mine',
    take_theirs: 'Take theirs',
    discard: 'Discard',
    retry: 'Retry',
    showDiff: 'Show differences',
    hideDiff: 'Hide differences',
  },
  dialog: {
    newEvent: 'New event',
    editEvent: 'Edit event',
    newContact: 'New contact',
    editContact: 'Edit contact',
    calendar: 'Calendar',
    addressBook: 'Address book',
    form: 'Form',
    rawEvent: 'iCalendar',
    rawContact: 'vCard',
    rawLabel: 'The item as text',
    rawHint:
      'For experts: the whole item as text. Holtmail checks it before saving and keeps the UID.',
    formUnavailable: 'This item has parts the form cannot show, so it opens as text.',
    save: 'Save',
    saving: 'Saving…',
    cancel: 'Cancel',
    loadFailed: 'Could not load the item for editing',
    stale: 'Someone saved this item meanwhile. Close and open it again to see their version.',
    held: (account: string) =>
      `Edits stay in Holtmail until sending to ${account} is turned on in Settings → Contacts and calendars.`,
    through: (account: string) => `Saved edits are sent to ${account}.`,
    heldCreate: (account: string) =>
      `Sending to ${account} is off, so the new item waits under Settings → Contacts and calendars and shows in lists and the calendar once it is sent.`,
    noBook:
      'No address book can be edited. Turn on editing contacts and calendars for an account under Writing.',
    fromSender: (address: string) => `From the sender ${address}`,
    created: 'Saved. The item appears once it reaches the account.',
  },
  event: {
    title: 'Title',
    allDay: 'All day',
    starts: 'Starts',
    ends: 'Ends',
    date: 'Date',
    time: 'Time',
    timeZone: 'Time zone',
    floating: 'Leave empty for the same clock time everywhere',
    repeat: 'Repeats',
    repeats: {
      none: 'Does not repeat',
      daily: 'Every day',
      weekly: 'Every week',
      monthly: 'Every month',
      yearly: 'Every year',
      custom: 'Custom rule',
    },
    rule: 'Rule',
    ruleHint: 'An RRULE such as FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10',
    location: 'Location',
    description: 'Description',
    invitees: 'Invitees',
    inviteesHint: 'People you invite get an invitation when you save.',
    addInvitee: 'Add invitee',
    inviteeEmail: 'Email',
    inviteeName: 'Name',
    remove: 'Remove',
    alarms: 'Reminders',
    addAlarm: 'Add reminder',
    alarm: (minutes: number) => {
      if (minutes === 0) return 'At the start'
      const [n, unit] = span(minutes)
      return `${n} ${unit}${n === 1 ? '' : 's'} before`
    },
    organizer: (who: string) => `Organized by ${who}`,
  },
  contact: {
    fullName: 'Shown name',
    given: 'First name',
    family: 'Last name',
    additional: 'Middle name',
    prefix: 'Prefix',
    suffix: 'Suffix',
    moreNames: 'More name parts',
    organization: 'Organization',
    title: 'Job title',
    emails: 'Emails',
    phones: 'Phones',
    addresses: 'Addresses',
    notes: 'Notes',
    addEmail: 'Add email',
    addPhone: 'Add phone',
    addAddress: 'Add address',
    label: 'Label',
    labels: { '': 'No label', home: 'Home', work: 'Work', cell: 'Mobile' } as Record<
      string,
      string
    >,
    email: 'Email',
    phone: 'Phone',
    street: 'Street',
    extended: 'Apartment, suite',
    poBox: 'PO box',
    locality: 'City',
    region: 'Region',
    postalCode: 'Postal code',
    country: 'Country',
    remove: 'Remove',
    photo: 'Photos, birthdays and other fields stay as they are; edit them as text.',
  },
  errors: {
    start: 'Choose the start date.',
    time: 'Choose the start time.',
    endBefore: 'The end is before the start.',
    email: (value: string) => `${value || 'An empty address'} is not an email address.`,
    rule: 'Write the repeat rule.',
    empty: 'Give the contact a name, an organization or an email address.',
    raw: 'Write the item as text.',
    collection: 'Choose where to save it.',
  },
  remove: {
    title: 'Delete this item?',
    event:
      'The event is deleted in its calendar once edits are sent to the account. The archive keeps it with its history.',
    cancel: (who: string) => `${who} Attendees are told it is cancelled.`,
    keep: 'Tell attendees it is cancelled',
    silent: 'Attendees are not told.',
    contact:
      'The contact is deleted in its address book once edits are sent to the account. The archive keeps it with its history.',
    confirm: 'Delete',
  },
  timeline: {
    change: (origin: string): string =>
      origin === 'dav' ? 'Changed on a device' : 'Changed in Holtmail',
    writeback: 'Written to the account by Holtmail',
    raw: 'As text',
    diffFailed: 'Could not load the differences',
  },
  diff: {
    caption: 'Differences field by field',
    field: 'Field',
    base: 'Before',
    mine: 'Yours',
    theirs: 'In the account',
    empty: '—',
    conflict: 'Both changed',
    none: 'No field differs.',
    gone: 'The item was deleted in the account.',
    deletes: 'Your change deletes the item.',
  },
  settings: {
    nav: 'Contacts and calendars',
    lead: 'Edits to contacts and events made in Holtmail or on your devices, and whether they are sent to the accounts the address books and calendars come from.',
    archive: 'Archive',
    noArchives: 'There are no archives yet.',
    loadFailed: 'Could not load the address books and calendars',
    modesTitle: 'Edits',
    modesLead:
      'For each address book and calendar: whether it can be edited, and whether edits stay in Holtmail or are sent to the account.',
    none: 'No address books or calendars yet.',
    modes: {
      read_only: 'Read-only',
      held: 'Keep edits in Holtmail',
      write_through: 'Send edits to the account',
    } as Record<string, string>,
    modeHints: {
      read_only: 'Nothing can be changed.',
      held: 'Edits are kept here and never sent.',
      write_through: 'Edits are sent to the account.',
    } as Record<string, string>,
    mode: (name: string) => `Edits in ${name}`,
    imported: 'Imported or found in mail; stays read-only.',
    managersOnly: 'Owners and admins set this.',
    cannotSend:
      'Sending needs Edit contacts and calendars under Writing for this account, and your right to change this archive.',
    confirmTitle: (account: string) => `Send edits to ${account}?`,
    confirmBody: (count: number, account: string) =>
      count > 0
        ? `${count} open ${count === 1 ? 'edit' : 'edits'} will be sent to ${account}, and every later edit too. The archive keeps every version.`
        : `Every later edit is sent to ${account}. The archive keeps every version.`,
    confirm: 'Send edits',
    open: (count: number) => `${count} open`,
    changesTitle: 'Open changes',
    changesLead: 'Edits not yet in the account, with conflicts and refusals that need you.',
    noChanges: 'No open changes.',
    newItem: 'New item, not in the account yet',
    unknownCollection: 'Unknown address book or calendar',
    decide: (count: number) => `${count} need a decision`,
    tries: (count: number) => `${count} ${count === 1 ? 'try' : 'tries'}`,
    lastError: (code: string) => `Last try failed: ${code}`,
    resolveFailed: 'Could not decide the change',
    navCount: (count: number) => `${count} changes need a decision`,
  },
  rsvp: {
    label: 'Your answer',
    answers: { accepted: 'Accept', tentative: 'Maybe', declined: 'Decline' } as Record<
      string,
      string
    >,
    addNote: 'Add a note',
    note: 'Note to the organizer',
    scope: 'Answer for',
    series: 'Whole series',
    one: 'One occurrence',
    occurrence: 'Occurrence',
    route: {
      imip: (from: string) => `Holtmail will email your answer to the organizer from ${from}.`,
      alias: (service: string, attendee: string, from: string) =>
        `Holtmail will email your answer to the organizer through ${service} as ${attendee}, from ${from}.`,
      aliasOnly: (service: string, attendee: string) =>
        `Holtmail will email your answer to the organizer through ${service} as ${attendee}.`,
      server: (account: string) =>
        `Your answer goes to the organizer through ${account}'s calendar.`,
      quiet: 'The organizer asked for no answers, so nobody is told. Only your calendar changes.',
    },
    held: (account: string) =>
      `Your calendar keeps this answer in Holtmail. ${account} gets it once the calendar writes through.`,
    done: {
      waiting: (seconds: number) => `Answer emailed to the organizer in ${seconds} s.`,
      sent: 'Your answer is on its way to the organizer.',
      server: (account: string) => `${account} tells the organizer.`,
      quiet: 'Saved in your calendar.',
      undone: 'Answer taken back.',
      pending: 'Your answer will be emailed to the organizer. You can still undo it.',
      undoneMailOnly:
        'The email was cancelled, but your calendar keeps the answer. Open Local edits to change it.',
    },
    undo: 'Undo',
    undoing: 'Taking back…',
    tooLate: 'Too late to undo.',
    openEdit:
      'An edit of this event has not reached the account yet. Send or discard it to answer.',
    openOutbox: 'Open outbox',
    failed: 'Could not answer',
    tally: (accepted: number, maybe: number, declined: number, none: number) =>
      `${accepted} accepted · ${maybe} maybe · ${declined} declined · ${none} not answered`,
    invitations: {
      send: 'Send invitations',
      imip: (from: string) => `Invitations will be sent by email from ${from}.`,
      server: (account: string) => `${account}'s calendar server will send the invitations.`,
      deferred: (account: string) =>
        `${account}'s calendar server will send the invitations once this change reaches it.`,
      none: 'Someone else organizes this event, so saving it mails nobody.',
      denied: (from: string) =>
        `You cannot send mail as ${from}, so this event is saved without invitations.`,
      off: 'Nobody is told about this change.',
      unknown: (account: string) =>
        `${account}'s calendar server cannot be reached, so Holtmail cannot tell who sends the invitations. Try again later, or turn off sending.`,
      loading: 'Checking who sends the invitations…',
      failed: 'Could not check who sends the invitations. Try again.',
    },
  },
}

export type EditStrings = typeof en

const ru: EditStrings = {
  mark: {
    label: 'Локальная правка',
    held: (account: string) => `Не отправлено в ${account}`,
    sending: 'Отправляется',
    conflict: (account: string) => `Изменено и в ${account}`,
    rejected: (account: string) => `${account} отклонил`,
    account: 'аккаунт',
  },
  reasons: {
    overlap: 'Вы и аккаунт изменили одни и те же поля.',
    reply_obsolete: 'Приглашение изменилось после вашего ответа.',
    upstream_deleted: 'Удалено в аккаунте.',
    upstream_changed: 'Изменено в аккаунте после вашей правки.',
    upstream_unreadable: 'Версию из аккаунта не удаётся прочитать.',
    refused: 'Аккаунт отклонил изменение.',
  },
  ops: { create: 'Новое', update: 'Изменение', delete: 'Удаление' },
  classes: {
    noop: 'Без изменений',
    personal: 'Только напоминания, остаются в Holtmail',
    reply: 'Ответ',
    edit: 'Правка',
    create: 'Новый элемент',
    delete: 'Удаление',
  },
  states: {
    held: 'Задержано',
    sending: 'Отправляется',
    applied: 'Применено',
    superseded: 'Заменено более поздней правкой',
    conflict: 'Конфликт',
    rejected: 'Отклонено',
    discarded: 'Отменено',
  },
  actions: {
    edit: 'Изменить',
    delete: 'Удалить',
    newEvent: 'Новое событие',
    newContact: 'Новый контакт',
    addSender: 'Добавить отправителя в контакты',
    editCard: 'Изменить карточку',
    keep_mine: 'Оставить моё',
    take_theirs: 'Взять из аккаунта',
    discard: 'Отменить',
    retry: 'Повторить',
    showDiff: 'Показать различия',
    hideDiff: 'Скрыть различия',
  },
  dialog: {
    newEvent: 'Новое событие',
    editEvent: 'Изменить событие',
    newContact: 'Новый контакт',
    editContact: 'Изменить контакт',
    calendar: 'Календарь',
    addressBook: 'Адресная книга',
    form: 'Форма',
    rawEvent: 'iCalendar',
    rawContact: 'vCard',
    rawLabel: 'Элемент как текст',
    rawHint:
      'Для опытных: весь элемент как текст. Holtmail проверяет его перед сохранением и сохраняет UID.',
    formUnavailable:
      'У этого элемента есть части, которые форма не показывает, поэтому он открыт как текст.',
    save: 'Сохранить',
    saving: 'Сохранение…',
    cancel: 'Отмена',
    loadFailed: 'Не удалось загрузить элемент для правки',
    stale:
      'Кто-то сохранил этот элемент за это время. Закройте и откройте снова, чтобы увидеть его версию.',
    held: (account: string) =>
      `Правки остаются в Holtmail, пока в Настройках → Контакты и календари не включена отправка в ${account}.`,
    through: (account: string) => `Сохранённые правки отправляются в ${account}.`,
    heldCreate: (account: string) =>
      `Отправка в ${account} выключена, поэтому новый элемент ждёт в Настройках → Контакты и календари и появится в списках и календаре после отправки.`,
    noBook:
      'Ни одну адресную книгу нельзя изменить. Включите правку контактов и календарей для аккаунта в разделе «Запись».',
    fromSender: (address: string) => `Из отправителя ${address}`,
    created: 'Сохранено. Элемент появится, когда попадёт в аккаунт.',
  },
  event: {
    title: 'Название',
    allDay: 'Весь день',
    starts: 'Начало',
    ends: 'Конец',
    date: 'Дата',
    time: 'Время',
    timeZone: 'Часовой пояс',
    floating: 'Оставьте пустым, чтобы время было одинаковым везде',
    repeat: 'Повтор',
    repeats: {
      none: 'Не повторяется',
      daily: 'Каждый день',
      weekly: 'Каждую неделю',
      monthly: 'Каждый месяц',
      yearly: 'Каждый год',
      custom: 'Своё правило',
    },
    rule: 'Правило',
    ruleHint: 'RRULE, например FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10',
    location: 'Место',
    description: 'Описание',
    invitees: 'Участники',
    inviteesHint: 'Участники получат приглашение, когда вы сохраните событие.',
    addInvitee: 'Добавить участника',
    inviteeEmail: 'Почта',
    inviteeName: 'Имя',
    remove: 'Убрать',
    alarms: 'Напоминания',
    addAlarm: 'Добавить напоминание',
    alarm: (minutes: number) => {
      if (minutes === 0) return 'В момент начала'
      const [n, unit] = span(minutes)
      return `За ${n} ${ruPlural(n, ...ruUnits[unit])}`
    },
    organizer: (who: string) => `Организатор: ${who}`,
  },
  contact: {
    fullName: 'Отображаемое имя',
    given: 'Имя',
    family: 'Фамилия',
    additional: 'Отчество',
    prefix: 'Обращение',
    suffix: 'Суффикс',
    moreNames: 'Другие части имени',
    organization: 'Организация',
    title: 'Должность',
    emails: 'Почта',
    phones: 'Телефоны',
    addresses: 'Адреса',
    notes: 'Заметки',
    addEmail: 'Добавить почту',
    addPhone: 'Добавить телефон',
    addAddress: 'Добавить адрес',
    label: 'Метка',
    labels: { '': 'Без метки', home: 'Домашний', work: 'Рабочий', cell: 'Мобильный' },
    email: 'Почта',
    phone: 'Телефон',
    street: 'Улица',
    extended: 'Квартира, офис',
    poBox: 'Абонентский ящик',
    locality: 'Город',
    region: 'Регион',
    postalCode: 'Индекс',
    country: 'Страна',
    remove: 'Убрать',
    photo: 'Фото, дни рождения и другие поля остаются как есть; их можно изменить как текст.',
  },
  errors: {
    start: 'Выберите дату начала.',
    time: 'Выберите время начала.',
    endBefore: 'Конец раньше начала.',
    email: (value: string) => `${value || 'Пустой адрес'} — не адрес почты.`,
    rule: 'Напишите правило повтора.',
    empty: 'Укажите имя, организацию или адрес почты.',
    raw: 'Напишите элемент как текст.',
    collection: 'Выберите, куда сохранить.',
  },
  remove: {
    title: 'Удалить этот элемент?',
    event:
      'Событие удаляется в календаре, когда правки отправляются в аккаунт. Архив хранит его вместе с историей.',
    cancel: (who: string) => `${who} Участникам сообщат об отмене.`,
    keep: 'Сообщить участникам об отмене',
    silent: 'Участникам не сообщат.',
    contact:
      'Контакт удаляется в адресной книге, когда правки отправляются в аккаунт. Архив хранит его вместе с историей.',
    confirm: 'Удалить',
  },
  timeline: {
    change: (origin: string) =>
      origin === 'dav' ? 'Изменено на устройстве' : 'Изменено в Holtmail',
    writeback: 'Записано в аккаунт Holtmail',
    raw: 'Как текст',
    diffFailed: 'Не удалось загрузить различия',
  },
  diff: {
    caption: 'Различия по полям',
    field: 'Поле',
    base: 'Было',
    mine: 'Ваше',
    theirs: 'В аккаунте',
    empty: '—',
    conflict: 'Изменено обоими',
    none: 'Поля не различаются.',
    gone: 'Элемент удалён в аккаунте.',
    deletes: 'Ваше изменение удаляет элемент.',
  },
  settings: {
    nav: 'Контакты и календари',
    lead: 'Правки контактов и событий, сделанные в Holtmail или на ваших устройствах, и отправляются ли они в аккаунты, откуда пришли адресные книги и календари.',
    archive: 'Архив',
    noArchives: 'Архивов пока нет.',
    loadFailed: 'Не удалось загрузить адресные книги и календари',
    modesTitle: 'Правки',
    modesLead:
      'Для каждой адресной книги и календаря: можно ли их менять и остаются ли правки в Holtmail или отправляются в аккаунт.',
    none: 'Адресных книг и календарей пока нет.',
    modes: {
      read_only: 'Только чтение',
      held: 'Хранить правки в Holtmail',
      write_through: 'Отправлять правки в аккаунт',
    },
    modeHints: {
      read_only: 'Ничего нельзя изменить.',
      held: 'Правки хранятся здесь и никуда не отправляются.',
      write_through: 'Правки отправляются в аккаунт.',
    },
    mode: (name: string) => `Правки в «${name}»`,
    imported: 'Импортировано или найдено в почте; только чтение.',
    managersOnly: 'Это меняют владелец и администраторы.',
    cannotSend:
      'Для отправки нужна правка контактов и календарей в разделе «Запись» этого аккаунта и ваше право менять этот архив.',
    confirmTitle: (account: string) => `Отправлять правки в ${account}?`,
    confirmBody: (count: number, account: string) =>
      count > 0
        ? `${count} ${ruPlural(count, 'открытая правка будет отправлена', 'открытые правки будут отправлены', 'открытых правок будут отправлены')} в ${account}, как и все следующие. Архив хранит каждую версию.`
        : `Все следующие правки отправляются в ${account}. Архив хранит каждую версию.`,
    confirm: 'Отправлять правки',
    open: (count: number) => `${count} ${ruPlural(count, 'открытая', 'открытые', 'открытых')}`,
    changesTitle: 'Открытые изменения',
    changesLead: 'Правки, которых ещё нет в аккаунте, и конфликты с отказами, которые ждут вас.',
    noChanges: 'Открытых изменений нет.',
    newItem: 'Новый элемент, в аккаунте его ещё нет',
    unknownCollection: 'Неизвестная адресная книга или календарь',
    decide: (count: number) =>
      `${count} ${ruPlural(count, 'ждёт решения', 'ждут решения', 'ждут решения')}`,
    tries: (count: number) => `${count} ${ruPlural(count, 'попытка', 'попытки', 'попыток')}`,
    lastError: (code: string) => `Последняя попытка не удалась: ${code}`,
    resolveFailed: 'Не удалось решить изменение',
    navCount: (count: number) =>
      `${count} ${ruPlural(count, 'изменение ждёт', 'изменения ждут', 'изменений ждут')} решения`,
  },
  rsvp: {
    label: 'Ваш ответ',
    answers: { accepted: 'Принять', tentative: 'Возможно', declined: 'Отклонить' },
    addNote: 'Добавить заметку',
    note: 'Заметка организатору',
    scope: 'Ответить на',
    series: 'Всю серию',
    one: 'Одно повторение',
    occurrence: 'Повторение',
    route: {
      imip: (from: string) => `Holtmail отправит ваш ответ организатору письмом с ${from}.`,
      alias: (service: string, attendee: string, from: string) =>
        `Holtmail отправит ваш ответ организатору письмом через ${service} от имени ${attendee}, с адреса ${from}.`,
      aliasOnly: (service: string, attendee: string) =>
        `Holtmail отправит ваш ответ организатору письмом через ${service} от имени ${attendee}.`,
      server: (account: string) => `Ваш ответ придёт организатору через календарь ${account}.`,
      quiet:
        'Организатор не ждёт ответов, поэтому никто не узнает. Изменится только ваш календарь.',
    },
    held: (account: string) =>
      `Этот ответ хранится в Holtmail. ${account} получит его, когда календарь начнёт записывать изменения.`,
    done: {
      waiting: (seconds: number) => `Ответ уйдёт организатору через ${seconds} с.`,
      sent: 'Ваш ответ отправляется организатору.',
      server: (account: string) => `${account} сообщит организатору.`,
      quiet: 'Сохранено в вашем календаре.',
      undone: 'Ответ отменён.',
      pending: 'Ответ будет отправлен организатору письмом. Его ещё можно отменить.',
      undoneMailOnly:
        'Письмо отменено, но ответ остался в вашем календаре. Измените его в локальных правках.',
    },
    undo: 'Отменить',
    undoing: 'Отменяем…',
    tooLate: 'Отменить уже нельзя.',
    openEdit:
      'Правка этого события ещё не дошла до аккаунта. Отправьте или отмените её, чтобы ответить.',
    openOutbox: 'Открыть исходящие',
    failed: 'Не удалось ответить',
    tally: (accepted: number, maybe: number, declined: number, none: number) =>
      `приняли: ${accepted} · возможно: ${maybe} · отклонили: ${declined} · не ответили: ${none}`,
    invitations: {
      send: 'Отправить приглашения',
      imip: (from: string) => `Приглашения уйдут письмом с ${from}.`,
      server: (account: string) => `Приглашения разошлёт сервер календаря ${account}.`,
      deferred: (account: string) =>
        `Приглашения разошлёт сервер календаря ${account}, когда изменение дойдёт до него.`,
      none: 'Это событие организует кто-то другой, поэтому при сохранении писем не будет.',
      denied: (from: string) =>
        `Вы не можете отправлять почту как ${from}, поэтому событие сохранится без приглашений.`,
      off: 'Об этом изменении никому не сообщат.',
      unknown: (account: string) =>
        `Сервер календаря ${account} недоступен, поэтому Holtmail не знает, кто разошлёт приглашения. Повторите позже или отключите отправку.`,
      loading: 'Проверяем, кто разошлёт приглашения…',
      failed: 'Не удалось проверить, кто разошлёт приглашения. Повторите.',
    },
  },
}

export const editStrings: Record<Locale, EditStrings> = { en, ru }
