export type Security = 'tls' | 'starttls' | 'plain'

export interface Provider {
  id: string
  name: string
  domains: readonly string[]
  host: string
  port: number
  security: Security
  note?: string
  help?: string
}

const appPassword = (where: string) =>
  `Most accounts need an app password instead of the usual one. Create it in ${where}.`

export const providers: readonly Provider[] = [
  {
    id: 'gmail',
    name: 'Gmail',
    domains: ['gmail.com', 'googlemail.com'],
    host: 'imap.gmail.com',
    port: 993,
    security: 'tls',
    note: appPassword('your Google account under Security, with 2-Step Verification on'),
    help: 'https://myaccount.google.com/apppasswords',
  },
  {
    id: 'outlook',
    name: 'Outlook',
    domains: ['outlook.com', 'hotmail.com', 'live.com', 'msn.com'],
    host: 'outlook.office365.com',
    port: 993,
    security: 'tls',
    note: appPassword('your Microsoft account security settings'),
    help: 'https://account.live.com/proofs/AppPassword',
  },
  {
    id: 'icloud',
    name: 'iCloud',
    domains: ['icloud.com', 'me.com', 'mac.com'],
    host: 'imap.mail.me.com',
    port: 993,
    security: 'tls',
    note: appPassword('your Apple Account under Sign-In and Security'),
    help: 'https://account.apple.com',
  },
  {
    id: 'yahoo',
    name: 'Yahoo',
    domains: ['yahoo.com', 'ymail.com', 'rocketmail.com'],
    host: 'imap.mail.yahoo.com',
    port: 993,
    security: 'tls',
    note: appPassword('Yahoo account security'),
    help: 'https://login.yahoo.com/account/security',
  },
  {
    id: 'fastmail',
    name: 'Fastmail',
    domains: ['fastmail.com', 'fastmail.fm'],
    host: 'imap.fastmail.com',
    port: 993,
    security: 'tls',
    note: appPassword('Fastmail settings under Privacy & Security'),
    help: 'https://app.fastmail.com/settings/security/apppasswords',
  },
  {
    id: 'yandex',
    name: 'Yandex',
    domains: ['yandex.ru', 'yandex.com', 'ya.ru'],
    host: 'imap.yandex.com',
    port: 993,
    security: 'tls',
    note: appPassword('Yandex ID under Security'),
    help: 'https://id.yandex.com/security/app-passwords',
  },
  {
    id: 'mailru',
    name: 'Mail.ru',
    domains: ['mail.ru', 'bk.ru', 'inbox.ru', 'list.ru', 'internet.ru'],
    host: 'imap.mail.ru',
    port: 993,
    security: 'tls',
    note: appPassword('Mail.ru account settings under Passwords for external apps'),
    help: 'https://account.mail.ru/user/2-step-auth/passwords',
  },
  {
    id: 'gmx',
    name: 'GMX',
    domains: ['gmx.com', 'gmx.net', 'gmx.de'],
    host: 'imap.gmx.net',
    port: 993,
    security: 'tls',
    note: 'Turn on IMAP access in the GMX settings first.',
  },
  {
    id: 'proton',
    name: 'Proton Mail',
    domains: ['proton.me', 'protonmail.com', 'pm.me'],
    host: '127.0.0.1',
    port: 1143,
    security: 'starttls',
    note: 'Proton Mail needs Proton Mail Bridge running next to Holtmail. Use the user name and password the Bridge shows.',
    help: 'https://proton.me/mail/bridge',
  },
]

export const defaultPort = (security: Security): number => (security === 'tls' ? 993 : 143)

export function domainOf(email: string): string {
  const at = email.lastIndexOf('@')
  return at < 0
    ? ''
    : email
        .slice(at + 1)
        .trim()
        .toLowerCase()
}

export function providerFor(email: string): Provider | undefined {
  const domain = domainOf(email)
  if (!domain) return undefined
  return providers.find((provider) => provider.domains.includes(domain))
}

export interface ServerGuess {
  host: string
  port: number
  security: Security
}

export function guessServer(email: string): ServerGuess | undefined {
  const provider = providerFor(email)
  if (provider) return { host: provider.host, port: provider.port, security: provider.security }
  const domain = domainOf(email)
  if (!domain || !domain.includes('.')) return undefined
  return { host: `imap.${domain}`, port: 993, security: 'tls' }
}
