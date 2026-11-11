import type { DeviceFormProblem, DeviceProtocol, DeviceScope, SystemFlag } from './lib/devices.ts'
import type { Locale } from './strings-writing.ts'

interface Described {
  label: string
  hint: string
}

interface FlagNames {
  on: string
  off: string
}

function ruPlural(n: number, one: string, few: string, many: string): string {
  const mod10 = n % 10
  const mod100 = n % 100
  if (mod10 === 1 && mod100 !== 11) return one
  if (mod10 >= 2 && mod10 <= 4 && (mod100 < 12 || mod100 > 14)) return few
  return many
}

const en = {
  nav: 'Devices',
  lead: 'Mail apps, phones and scripts that connect with an app password of their own.',
  add: 'New device',
  empty: 'No devices yet. Add one to read your archive in another app.',
  loadFailed: 'Could not load your devices.',
  noCreate: 'On this server only the owner and admins can make devices.',
  revokedHeading: 'Revoked',
  servers: {
    heading: 'Connecting',
    none: 'This server does not serve mail apps yet. A device made now starts working once the owner turns that on.',
    lead: 'Enter these in your mail app, with your sign-in address as the user name and the device’s app password.',
    protocol: 'Service',
    host: 'Server',
    port: 'Port',
    security: 'Security',
    user: 'User name',
    tls: 'TLS',
    starttls: 'STARTTLS',
    names: {
      smtp: 'Outgoing (SMTP)',
      imap: 'Incoming (IMAP)',
      jmap: 'JMAP (HTTPS)',
      dav: 'CalDAV and CardDAV (HTTPS)',
    } as Record<string, string>,
  },
  form: {
    newTitle: 'New device',
    editTitle: (name: string) => `Change ${name}`,
    lead: 'A device can never do more than you can, and only what you tick here.',
    name: 'Name',
    namePlaceholder: 'Phone',
    protocols: 'How it connects',
    protocolOff: 'Not turned on on this server yet.',
    mailArchive: 'Mail archive',
    mailArchiveHint: 'IMAP and SMTP work with one archive; this is the one the app sees.',
    archives: 'Archives it can see',
    allArchives: 'Every archive I can read',
    allArchivesHint: 'Includes archives you are given later.',
    scopes: 'What it may do',
    implicitSeen: 'Opening a message marks it as read',
    implicitSeenHint: 'Off means the app can download mail without changing what is unread.',
    create: 'Make device',
    creating: 'Making…',
    save: 'Save',
    saving: 'Saving…',
    cancel: 'Cancel',
    problems: {
      name: 'Give the device a name.',
      protocol: 'Choose how it connects.',
      scope: 'Choose at least one thing it may do.',
      mailArchive: 'Choose the mail archive.',
      archives: 'Choose at least one archive.',
      smtpNeedsSend: 'SMTP can only sign in with “Send mail” ticked.',
      needsRead: 'Everything except SMTP needs “Read” ticked.',
    } satisfies Record<DeviceFormProblem, string>,
  },
  protocolNames: {
    imap: { label: 'IMAP', hint: 'Mail apps such as Thunderbird or Apple Mail.' },
    smtp: { label: 'SMTP', hint: 'Sending from a mail app.' },
    jmap: { label: 'JMAP', hint: 'Newer mail apps, over the web.' },
    dav: { label: 'CalDAV and CardDAV', hint: 'Calendar and contacts apps.' },
    api: { label: 'Holtmail API', hint: 'Your own scripts.' },
  } satisfies Record<DeviceProtocol, Described>,
  scopeNames: {
    read: {
      label: 'Read',
      hint: 'See mail, contacts and calendars in these archives. Nothing on your servers changes.',
    },
    'write:mail': {
      label: 'Change mail',
      hint: 'Mark as read, flag, move, delete and save messages. Where an account allows it, the changes reach your real mailbox.',
    },
    'send:mail': {
      label: 'Send mail',
      hint: 'Write to people in your name, from the addresses you may send from. Anyone with this password can do the same.',
    },
    'write:pim': {
      label: 'Change contacts and calendars',
      hint: 'Edit contacts and events and answer invitations. Changes leave Holtmail only for address books and calendars set to send them.',
    },
    notify: {
      label: 'Notifications',
      hint: 'Be told when new mail arrives.',
    },
  } satisfies Record<DeviceScope, Described>,
  row: {
    actions: (name: string) => `Actions for ${name}`,
    edit: 'Change',
    revoke: 'Revoke',
    made: (date: string) => `Added ${date}`,
    neverUsed: 'Never used',
    lastUsed: (date: string, protocol: string) => `Last used ${date} over ${protocol}`,
    revokedOn: (date: string) => `Revoked ${date}`,
    mailArchive: (name: string) => `Mail from ${name}`,
    allArchives: 'Every archive you can read',
    someArchives: (names: string) => `Archives: ${names}`,
    unknownArchive: 'a removed archive',
    notServed: (names: string) =>
      `This server does not offer ${names} now, so the device cannot connect that way until it does.`,
  },
  origin: {
    named: (name: string) => `Made on ${name}`,
    unknown: 'Made on a device',
  },
  secret: {
    title: (name: string) => `App password for ${name}`,
    lead: 'Copy it now. Holtmail keeps only a fingerprint of it and cannot show it again.',
    label: 'App password',
    copy: 'Copy',
    copied: 'Copied',
    done: 'I have saved it',
  },
  setup: {
    heading: 'Before you add an account',
    notBoth:
      'Use Holtmail or the original account on a device, not both: two accounts showing the same mail, events or contacts give duplicates and double notifications.',
    iosPush:
      'iPhone and iPad Mail checks accounts like this one only every 15 minutes or when you pull to refresh. Install Holtmail from the browser to be told about new mail at once.',
    cloud:
      'The new Outlook for Windows and Gmail’s “other accounts” connect through Microsoft’s and Google’s servers: they work only when this server is reachable from the internet, and the app password is kept by them.',
    davx5: 'Open DAVx5 with this server filled in',
    davx5Hint: 'On Android, for calendars and contacts. Paste the app password when DAVx5 asks.',
    thunderbird: (domains: string) =>
      `Thunderbird finds this server by itself for addresses at ${domains}: enter the address and the device’s app password.`,
  },
  profile: {
    menu: 'Profile for iPhone, iPad or Mac',
    title: (name: string) => `Profile for ${name}`,
    lead: 'A profile sets up mail, calendars and contacts on an iPhone, iPad or Mac in one step.',
    fresh:
      'Opening the link gives the device a new app password, which the profile carries; the old one stops working and apps using it must sign in again.',
    once: 'The link works once, for 10 minutes. Open it on the device that should get the account.',
    unsigned:
      'iOS and macOS show the profile as “Not Verified” because it is not signed. Install it under Settings → General → VPN & Device Management.',
    standalone:
      'An app on the home screen cannot hand a profile to iOS. Copy the link and open it in Safari on this device.',
    opened:
      'The link opened in a new page. Confirm there to download the profile; once it is downloaded the link is used up, so make a new one to try again.',
    make: 'Make link',
    making: 'Making…',
    download: 'Download profile',
    copy: 'Copy link',
    copied: 'Copied',
    expires: (time: string) => `Works until ${time}.`,
    done: 'Done',
    cancel: 'Cancel',
    offer: 'Set up an iPhone, iPad or Mac with a profile instead',
  },
  revoke: {
    title: (name: string) => `Revoke ${name}?`,
    lead: 'It is signed out at once and its app password stops working for good. It stays in this list as a record.',
    action: 'Revoke',
    busy: 'Revoking…',
  },
  signIn: {
    title: 'Confirm it’s you',
    lead: 'Making a device or giving one more rights needs a recent sign-in. Enter your password to continue.',
    password: 'Password',
    action: 'Continue',
    busy: 'Checking…',
    cancel: 'Cancel',
  },
  serving: {
    heading: 'In mail apps',
    lead: 'How each archive is prepared for mail apps, and what is kept only in Holtmail.',
    loadFailed: 'Could not load how mail apps are served.',
    building: (percent: number, assigned: string, total: string) =>
      `Preparing for mail apps: ${percent}% (${assigned} of ${total} messages). Apps that open a mailbox now are asked to try again shortly.`,
    ready: (mailboxes: number) =>
      mailboxes === 1
        ? 'Ready in mail apps: 1 mailbox.'
        : `Ready in mail apps: ${mailboxes} mailboxes.`,
    progressLabel: (archive: string) => `Preparing ${archive} for mail apps`,
    localLead:
      'Where a mail server does not let Holtmail change a message, read and flag marks made in mail apps are kept here for you alone and never sent to the server. Clearing them shows the server’s marks again.',
    localCount: (count: number) =>
      count === 1
        ? '1 message has marks kept only in Holtmail.'
        : `${count} messages have marks kept only in Holtmail.`,
    localOff:
      'Keeping marks in Holtmail is turned off on this server: mail apps do not see these marks, and changes a server refuses are refused.',
    show: 'Show',
    hide: 'Hide',
    clearAll: 'Clear all',
    clear: 'Clear',
    clearLabel: (subject: string) => `Clear marks on ${subject}`,
    shown: (shown: number, total: number) => `The newest ${shown} of ${total} are listed.`,
    noSubject: '(no subject)',
    listFailed: 'Could not load the marks.',
    clearFailed: 'Could not clear the marks.',
    confirmTitle: (archive: string) => `Clear marks kept in ${archive}?`,
    confirmLead:
      'Mail apps and Holtmail show the mail server’s read and flag marks again for these messages. Nothing changes on the server.',
    confirmAction: 'Clear all',
    confirmBusy: 'Clearing…',
    flags: {
      seen: { on: 'Read', off: 'Unread' },
      flagged: { on: 'Flagged', off: 'Not flagged' },
      answered: { on: 'Answered', off: 'Not answered' },
      draft: { on: 'Draft', off: 'Not a draft' },
    } satisfies Record<SystemFlag, FlagNames>,
    keyword: (name: string, present: boolean) => (present ? name : `Without ${name}`),
  },
  notice: {
    made: (name: string, date: string) => `A new device, ${name}, was added on ${date}.`,
    raised: (name: string, date: string) => `The device ${name} was given more rights on ${date}.`,
    many: (count: number) => `${count} devices were added or given more rights recently.`,
    notYou: 'If this was not you, revoke it and change your password.',
    review: 'Review',
    dismiss: 'Dismiss',
  },
}

export type DeviceStrings = typeof en

const ru: DeviceStrings = {
  nav: 'Устройства',
  lead: 'Почтовые приложения, телефоны и скрипты, которые подключаются со своим паролем приложения.',
  add: 'Новое устройство',
  empty: 'Устройств пока нет. Добавьте одно, чтобы читать архив в другом приложении.',
  loadFailed: 'Не удалось загрузить устройства.',
  noCreate: 'На этом сервере устройства могут создавать только владелец и администраторы.',
  revokedHeading: 'Отозванные',
  servers: {
    heading: 'Подключение',
    none: 'Этот сервер пока не обслуживает почтовые приложения. Созданное сейчас устройство заработает, когда владелец это включит.',
    lead: 'Укажите это в почтовом приложении: имя пользователя — ваш адрес для входа, пароль — пароль приложения устройства.',
    protocol: 'Служба',
    host: 'Сервер',
    port: 'Порт',
    security: 'Защита',
    user: 'Имя пользователя',
    tls: 'TLS',
    starttls: 'STARTTLS',
    names: {
      smtp: 'Исходящая (SMTP)',
      imap: 'Входящая (IMAP)',
      jmap: 'JMAP (HTTPS)',
      dav: 'CalDAV и CardDAV (HTTPS)',
    },
  },
  form: {
    newTitle: 'Новое устройство',
    editTitle: (name) => `Изменить: ${name}`,
    lead: 'Устройство не может больше, чем вы, и может только то, что отмечено здесь.',
    name: 'Название',
    namePlaceholder: 'Телефон',
    protocols: 'Как подключается',
    protocolOff: 'На этом сервере пока не включено.',
    mailArchive: 'Почтовый архив',
    mailArchiveHint: 'IMAP и SMTP работают с одним архивом — его и увидит приложение.',
    archives: 'Какие архивы видит',
    allArchives: 'Все архивы, которые я могу читать',
    allArchivesHint: 'Включая архивы, к которым вам дадут доступ позже.',
    scopes: 'Что может делать',
    implicitSeen: 'Открытие письма отмечает его прочитанным',
    implicitSeenHint: 'Если выключено, приложение скачивает почту, не меняя непрочитанные.',
    create: 'Создать устройство',
    creating: 'Создание…',
    save: 'Сохранить',
    saving: 'Сохранение…',
    cancel: 'Отмена',
    problems: {
      name: 'Дайте устройству название.',
      protocol: 'Выберите, как оно подключается.',
      scope: 'Выберите хотя бы одно разрешение.',
      mailArchive: 'Выберите почтовый архив.',
      archives: 'Выберите хотя бы один архив.',
      smtpNeedsSend: 'SMTP может войти только с разрешением «Отправлять почту».',
      needsRead: 'Всё, кроме SMTP, требует разрешения «Читать».',
    },
  },
  protocolNames: {
    imap: { label: 'IMAP', hint: 'Почтовые приложения вроде Thunderbird или Apple Mail.' },
    smtp: { label: 'SMTP', hint: 'Отправка из почтового приложения.' },
    jmap: { label: 'JMAP', hint: 'Новые почтовые приложения, через веб.' },
    dav: { label: 'CalDAV и CardDAV', hint: 'Приложения календаря и контактов.' },
    api: { label: 'API Holtmail', hint: 'Ваши собственные скрипты.' },
  },
  scopeNames: {
    read: {
      label: 'Читать',
      hint: 'Видеть почту, контакты и календари в этих архивах. На ваших серверах ничего не меняется.',
    },
    'write:mail': {
      label: 'Изменять почту',
      hint: 'Отмечать прочитанным, ставить флаги, перемещать, удалять и сохранять письма. Если учётная запись это разрешает, изменения дойдут до настоящего ящика.',
    },
    'send:mail': {
      label: 'Отправлять почту',
      hint: 'Писать людям от вашего имени с адресов, с которых вы можете отправлять. То же сможет любой, у кого есть этот пароль.',
    },
    'write:pim': {
      label: 'Изменять контакты и календари',
      hint: 'Править контакты и события, отвечать на приглашения. Изменения покидают Holtmail только для адресных книг и календарей, настроенных на отправку.',
    },
    notify: {
      label: 'Уведомления',
      hint: 'Сообщать о новой почте.',
    },
  },
  row: {
    actions: (name) => `Действия: ${name}`,
    edit: 'Изменить',
    revoke: 'Отозвать',
    made: (date) => `Добавлено ${date}`,
    neverUsed: 'Ещё не использовалось',
    lastUsed: (date, protocol) => `Последнее использование: ${date}, ${protocol}`,
    revokedOn: (date) => `Отозвано ${date}`,
    mailArchive: (name) => `Почта из архива ${name}`,
    allArchives: 'Все архивы, которые вы можете читать',
    someArchives: (names) => `Архивы: ${names}`,
    unknownArchive: 'удалённый архив',
    notServed: (names) =>
      `Этот сервер сейчас не предоставляет ${names}, поэтому устройство не сможет так подключиться, пока он этого не начнёт.`,
  },
  origin: {
    named: (name) => `Сделано на устройстве «${name}»`,
    unknown: 'Сделано на устройстве',
  },
  secret: {
    title: (name) => `Пароль приложения для ${name}`,
    lead: 'Скопируйте его сейчас. Holtmail хранит только отпечаток и не сможет показать пароль снова.',
    label: 'Пароль приложения',
    copy: 'Копировать',
    copied: 'Скопировано',
    done: 'Я сохранил пароль',
  },
  setup: {
    heading: 'Перед тем как добавить учётную запись',
    notBoth:
      'На одном устройстве используйте либо Holtmail, либо исходную учётную запись, но не обе: две учётные записи с одной и той же почтой, событиями или контактами дают дубликаты и двойные уведомления.',
    iosPush:
      'Почта на iPhone и iPad проверяет такие учётные записи только раз в 15 минут или когда вы обновляете список вручную. Установите Holtmail из браузера, чтобы сразу узнавать о новых письмах.',
    cloud:
      'Новый Outlook для Windows и «другие аккаунты» в Gmail подключаются через серверы Microsoft и Google: они работают, только если этот сервер доступен из интернета, а пароль приложения хранится у них.',
    davx5: 'Открыть DAVx5 с этим сервером',
    davx5Hint:
      'На Android, для календарей и контактов. Вставьте пароль приложения, когда DAVx5 его спросит.',
    thunderbird: (domains) =>
      `Для адресов на ${domains} Thunderbird сам найдёт этот сервер: введите адрес и пароль приложения устройства.`,
  },
  profile: {
    menu: 'Профиль для iPhone, iPad или Mac',
    title: (name) => `Профиль для ${name}`,
    lead: 'Профиль за один шаг настраивает почту, календари и контакты на iPhone, iPad или Mac.',
    fresh:
      'Когда ссылку откроют, устройство получит новый пароль приложения, и он попадёт в профиль; старый перестанет работать, и приложениям с ним придётся войти снова.',
    once: 'Ссылка работает один раз в течение 10 минут. Откройте её на том устройстве, где нужна учётная запись.',
    unsigned:
      'iOS и macOS показывают профиль как «Не проверен», потому что он не подписан. Установите его в Настройках → Основные → VPN и управление устройством.',
    standalone:
      'Приложение на экране «Домой» не может передать профиль в iOS. Скопируйте ссылку и откройте её в Safari на этом устройстве.',
    opened:
      'Ссылка открылась на новой странице. Подтвердите там загрузку профиля; после загрузки ссылка больше не работает, и для новой попытки нужна новая ссылка.',
    make: 'Создать ссылку',
    making: 'Создание…',
    download: 'Скачать профиль',
    copy: 'Копировать ссылку',
    copied: 'Скопировано',
    expires: (time) => `Работает до ${time}.`,
    done: 'Готово',
    cancel: 'Отмена',
    offer: 'Или настройте iPhone, iPad или Mac профилем',
  },
  revoke: {
    title: (name) => `Отозвать ${name}?`,
    lead: 'Устройство сразу отключится, а его пароль перестанет работать навсегда. В этом списке оно останется как запись.',
    action: 'Отозвать',
    busy: 'Отзыв…',
  },
  signIn: {
    title: 'Подтвердите, что это вы',
    lead: 'Чтобы создать устройство или расширить его права, нужен недавний вход. Введите пароль, чтобы продолжить.',
    password: 'Пароль',
    action: 'Продолжить',
    busy: 'Проверка…',
    cancel: 'Отмена',
  },
  serving: {
    heading: 'В почтовых приложениях',
    lead: 'Как каждый архив подготовлен для почтовых приложений и что хранится только в Holtmail.',
    loadFailed: 'Не удалось загрузить состояние почтовых приложений.',
    building: (percent, assigned, total) =>
      `Подготовка для почтовых приложений: ${percent}% (${assigned} из ${total} писем). Приложения, которые откроют ящик сейчас, попросят повторить чуть позже.`,
    ready: (mailboxes) =>
      `Готово для почтовых приложений: ${mailboxes} ${ruPlural(mailboxes, 'ящик', 'ящика', 'ящиков')}.`,
    progressLabel: (archive) => `Подготовка архива ${archive} для почтовых приложений`,
    localLead:
      'Если почтовый сервер не даёт Holtmail менять письмо, отметки о прочтении и флаги из почтовых приложений хранятся здесь только для вас и никогда не отправляются на сервер. Если их очистить, снова будут видны отметки сервера.',
    localCount: (count) =>
      `${count} ${ruPlural(count, 'письмо имеет', 'письма имеют', 'писем имеют')} отметки только в Holtmail.`,
    localOff:
      'Хранение отметок в Holtmail на этом сервере выключено: почтовые приложения их не видят, а изменения, которые сервер не принимает, отклоняются.',
    show: 'Показать',
    hide: 'Скрыть',
    clearAll: 'Очистить все',
    clear: 'Очистить',
    clearLabel: (subject) => `Очистить отметки: ${subject}`,
    shown: (shown, total) => `Показаны ${shown} самых новых из ${total}.`,
    noSubject: '(без темы)',
    listFailed: 'Не удалось загрузить отметки.',
    clearFailed: 'Не удалось очистить отметки.',
    confirmTitle: (archive) => `Очистить отметки в архиве ${archive}?`,
    confirmLead:
      'Почтовые приложения и Holtmail снова покажут для этих писем отметки о прочтении и флаги почтового сервера. На сервере ничего не изменится.',
    confirmAction: 'Очистить все',
    confirmBusy: 'Очистка…',
    flags: {
      seen: { on: 'Прочитано', off: 'Не прочитано' },
      flagged: { on: 'С флагом', off: 'Без флага' },
      answered: { on: 'Отвечено', off: 'Без ответа' },
      draft: { on: 'Черновик', off: 'Не черновик' },
    },
    keyword: (name, present) => (present ? name : `Без ${name}`),
  },
  notice: {
    made: (name, date) => `Добавлено новое устройство «${name}», ${date}.`,
    raised: (name, date) => `Устройству «${name}» расширили права, ${date}.`,
    many: (count) => `Недавно добавлено устройств или расширено прав: ${count}.`,
    notYou: 'Если это были не вы, отзовите его и смените пароль.',
    review: 'Посмотреть',
    dismiss: 'Скрыть',
  },
}

export const deviceStrings: Record<Locale, DeviceStrings> = { en, ru }
