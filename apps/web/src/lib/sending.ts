import type {
  AccountCapabilities,
  Identity,
  IdentityRequest,
  IdentityUpdateRequest,
  IdentitySuggestion,
  Transport,
  TransportLimits,
  TransportRequest,
  TransportSecurity,
  TransportUpdateRequest,
} from '@holtmail/api-client'

export type TransportKind = 'smtp' | 'jmap' | 'graph' | 'account'
export type JmapAuth = 'password' | 'token'
/** What an account transport speaks: SMTP submission for IMAP accounts, JMAP submission for
 * JMAP accounts and Graph `sendMail` for Microsoft Graph accounts. */
export type AccountServer = 'smtp' | 'jmap' | 'graph'

export const transportKinds: readonly TransportKind[] = ['smtp', 'jmap', 'graph', 'account']

export interface TransportForm {
  name: string
  kind: TransportKind
  account: string
  host: string
  port: string
  security: TransportSecurity
  username: string
  password: string
  url: string
  auth: JmapAuth
  mailbox: string
  allowedFrom: string
  maxSizeMb: string
  maxRecipients: string
  perHour: string
  enabled: boolean
}

export const isTransportKind = (value: string): value is TransportKind =>
  (transportKinds as readonly string[]).includes(value)

export const emptyTransportForm = (kind: TransportKind = 'smtp'): TransportForm => ({
  name: '',
  kind,
  account: '',
  host: '',
  port: '',
  security: 'starttls',
  username: '',
  password: '',
  url: '',
  auth: 'password',
  mailbox: '',
  allowedFrom: '',
  maxSizeMb: '',
  maxRecipients: '',
  perHour: '',
  enabled: true,
})

const MB = 1024 * 1024

/** The server an account transport sends through, from the account's kind; an unknown or
 * missing account counts as IMAP. */
export function accountServer(accountKind: string | undefined): AccountServer {
  return accountKind === 'jmap' || accountKind === 'graph' ? accountKind : 'smtp'
}

/** What the form's transport speaks: its own kind, or for `account` the account's server. */
export const formServer = (form: TransportForm, accountKind?: string): AccountServer =>
  form.kind === 'account' ? accountServer(accountKind) : form.kind

/** The fields the form shows for a transport: SMTP gets a server and a sign-in; JMAP an
 * address, a sign-in kind and a password or token; Graph an optional endpoint, a mailbox and a
 * token; an account transport of a JMAP or Graph account only its addresses and limits, since
 * it signs in as the account. */
export function transportFields(form: TransportForm, accountKind?: string) {
  const server = formServer(form, accountKind)
  const own = form.kind !== 'account'
  return {
    server: server === 'smtp',
    url: own && server !== 'smtp',
    auth: own && server === 'jmap',
    mailbox: own && server === 'graph',
    username: server === 'smtp' || (own && server === 'jmap' && form.auth === 'password'),
    password: server === 'smtp' || own,
  }
}

function kindOf(t: Transport): TransportKind {
  return isTransportKind(t.kind) ? t.kind : 'smtp'
}

export function transportForm(t: Transport): TransportForm {
  return {
    name: t.name,
    kind: kindOf(t),
    account: t.account ?? '',
    host: t.host ?? '',
    port: t.port == null ? '' : String(t.port),
    security: t.security === 'tls' || t.security === 'none' ? t.security : 'starttls',
    username: t.username ?? '',
    password: '',
    url: t.url ?? '',
    auth: t.auth === 'token' ? 'token' : 'password',
    mailbox: t.mailbox ?? '',
    allowedFrom: t.allowed_from.join('\n'),
    maxSizeMb: t.limits.max_size == null ? '' : String(Math.round(t.limits.max_size / MB)),
    maxRecipients: t.limits.max_recipients == null ? '' : String(t.limits.max_recipients),
    perHour: t.limits.per_hour == null ? '' : String(t.limits.per_hour),
    enabled: t.enabled,
  }
}

/** Address patterns, one per line or separated by commas, blanks dropped. */
export const allowedFromLines = (text: string): string[] =>
  text
    .split(/[\n,]/)
    .map((line) => line.trim())
    .filter(Boolean)

/** Whether a pattern has the shape the server takes: `ada@example.com`, `*@example.org` or
 * `*@*.example.org`. The server checks again. */
export function validPattern(pattern: string): boolean {
  const at = pattern.lastIndexOf('@')
  if (at <= 0 || /\s/.test(pattern)) return false
  const local = pattern.slice(0, at)
  const domain = pattern.slice(at + 1).replace(/^\*\./, '')
  if (local.includes('*') && local !== '*') return false
  return /^[^*@]+\.[^*@.]+$/.test(domain)
}

/** A positive whole number, `null` for empty, `undefined` for anything else. */
export function parseLimit(text: string): number | null | undefined {
  const value = text.trim()
  if (!value) return null
  if (!/^\d+$/.test(value)) return undefined
  const number = Number(value)
  return number > 0 ? number : undefined
}

export type TransportFormError =
  | {
      field:
        | 'name'
        | 'host'
        | 'account'
        | 'port'
        | 'limits'
        | 'url'
        | 'username'
        | 'password'
        | 'secretAgain'
        | 'secretForAuth'
    }
  | { field: 'allowedFrom'; pattern: string }

interface Checked {
  name: string
  port: number | null
  patterns: string[]
  limits: TransportLimits
}

function check(
  form: TransportForm,
  accountKind: string | undefined,
  editing: Transport | undefined,
): Checked | { error: TransportFormError } {
  if (!form.name.trim()) return { error: { field: 'name' } }
  const server = formServer(form, accountKind)
  const fields = transportFields(form, accountKind)
  if (form.kind === 'smtp' && !form.host.trim()) return { error: { field: 'host' } }
  if (form.kind === 'account' && !form.account) return { error: { field: 'account' } }
  if (fields.url && server === 'jmap' && !form.url.trim()) return { error: { field: 'url' } }
  if (fields.auth && form.auth === 'password' && !form.username.trim()) {
    return { error: { field: 'username' } }
  }
  if (form.kind === 'jmap' || form.kind === 'graph') {
    if (!editing && !form.password) return { error: { field: 'password' } }
    if (editing && urlChanged(form, editing) && !form.password) {
      return { error: { field: 'secretAgain' } }
    }
    if (editing && fields.auth && form.auth !== (editing.auth ?? 'password') && !form.password) {
      return { error: { field: 'secretForAuth' } }
    }
  }
  const port = fields.server ? parseLimit(form.port) : null
  if (port === undefined || (port != null && port > 65_535)) return { error: { field: 'port' } }
  const patterns = allowedFromLines(form.allowedFrom)
  const bad = patterns.find((p) => !validPattern(p))
  if (bad) return { error: { field: 'allowedFrom', pattern: bad } }
  const maxSize = parseLimit(form.maxSizeMb)
  const maxRecipients = parseLimit(form.maxRecipients)
  const perHour = parseLimit(form.perHour)
  if (maxSize === undefined || maxRecipients === undefined || perHour === undefined) {
    return { error: { field: 'limits' } }
  }
  return {
    name: form.name.trim(),
    port,
    patterns,
    limits: {
      max_size: maxSize == null ? null : maxSize * MB,
      max_recipients: maxRecipients,
      per_hour: perHour,
    },
  }
}

/** Whether the form names another JMAP or Graph address than the stored one; the server then
 * asks for the password or token again, so a stored secret never reaches another server. */
export const urlChanged = (form: TransportForm, t: Transport): boolean =>
  form.url.trim() !== (t.url ?? '')

/** The request a transport form makes, or the first field that is wrong. `accountKind` is the
 * kind of the chosen account, for an account transport. */
export function transportRequest(
  form: TransportForm,
  accountKind?: string,
): { request: TransportRequest } | { error: TransportFormError } {
  const checked = check(form, accountKind, undefined)
  if ('error' in checked) return checked
  const fields = transportFields(form, accountKind)
  return {
    request: {
      name: checked.name,
      kind: form.kind,
      account: form.kind === 'account' ? form.account : null,
      host: fields.server ? form.host.trim() || null : null,
      port: checked.port,
      security: fields.server ? form.security : null,
      username: fields.username ? form.username.trim() || null : null,
      password: fields.password ? form.password || null : null,
      url: fields.url ? form.url.trim() || null : null,
      auth: fields.auth ? form.auth : null,
      mailbox: fields.mailbox ? form.mailbox.trim() || null : null,
      allowed_from: checked.patterns,
      limits: checked.limits,
      enabled: form.enabled,
    },
  }
}

/** The changes an edited transport sends, or the first field that is wrong. Only the fields of
 * its kind go out; a JMAP or Graph address goes out only when it changed, and an empty
 * password keeps the stored one. */
export function transportUpdate(
  form: TransportForm,
  transport: Transport,
  accountKind?: string,
): { request: TransportUpdateRequest } | { error: TransportFormError } {
  const checked = check({ ...form, kind: kindOf(transport) }, accountKind, transport)
  if ('error' in checked) return checked
  const fields = transportFields({ ...form, kind: kindOf(transport) }, accountKind)
  const request: TransportUpdateRequest = {
    name: checked.name,
    allowed_from: checked.patterns,
    limits: checked.limits,
    enabled: form.enabled,
  }
  if (fields.server) {
    request.host = form.host.trim()
    request.port = checked.port
    request.security = form.security
  }
  if (fields.url && urlChanged(form, transport)) request.url = form.url.trim()
  if (fields.auth) request.auth = form.auth
  if (fields.mailbox) request.mailbox = form.mailbox.trim()
  if (fields.username) request.username = form.username.trim()
  else if (fields.auth) request.username = ''
  if (fields.password && form.password) request.password = form.password
  return { request }
}

export type SentCopyChoice = 'provider' | 'append' | 'none'

/** Reads a stored policy: `provider`, `append:<folder>` or `none`. */
export function parseSentCopy(stored: string): { choice: SentCopyChoice; folder: string } {
  if (stored.startsWith('append:')) return { choice: 'append', folder: stored.slice(7) }
  return { choice: stored === 'provider' ? 'provider' : 'none', folder: '' }
}

export const sentCopyValue = (choice: SentCopyChoice, folder: string): string =>
  choice === 'append' ? `append:${folder.trim() || 'Sent'}` : choice

/** The account a sent copy is saved into with `append`: the identity's own account, else the
 * account of its account transport; `null` when there is neither. */
export function sentCopyAccount(
  form: { account: string | null; transport: string },
  transports: readonly Transport[],
): string | null {
  if (form.account) return form.account
  const transport = transports.find((t) => t.id === form.transport)
  return transport?.kind === 'account' ? (transport.account ?? null) : null
}

/** Whether `append` can work for an identity: `available` once the account has `mail.append`
 * in effect, `switchedOff` when it is on but the instance switch is not, `off` when it is not
 * turned on, `noAccount` without an account, `unknown` while the switches are not known (the
 * server decides then). */
export type AppendState = 'available' | 'switchedOff' | 'off' | 'noAccount' | 'unknown'

export function appendState(
  account: string | null,
  capabilities: AccountCapabilities | undefined,
): AppendState {
  if (!account) return 'noAccount'
  if (!capabilities) return 'unknown'
  if (capabilities.active.includes('mail.append')) return 'available'
  return capabilities.capabilities.includes('mail.append') ? 'switchedOff' : 'off'
}

export interface IdentityForm {
  address: string
  name: string
  replyTo: string
  transport: string
  sentCopy: SentCopyChoice
  sentFolder: string
  bccSelf: boolean
  plusAddresses: boolean
  signatureHtml: string
  signatureText: string
  account: string | null
  alias: string | null
  active: boolean
  users: string[]
}

export function identityForm(i: Identity): IdentityForm {
  const sent = parseSentCopy(i.sent_copy)
  return {
    address: i.address,
    name: i.name ?? '',
    replyTo: i.reply_to ?? '',
    transport: i.transport,
    sentCopy: sent.choice,
    sentFolder: sent.folder,
    bccSelf: i.bcc_self,
    plusAddresses: i.plus_addresses,
    signatureHtml: i.signature_html ?? '',
    signatureText: i.signature_text ?? '',
    account: i.account ?? null,
    alias: i.alias ?? null,
    active: i.active,
    users: i.users,
  }
}

/** A form for a new identity, from a suggestion when there is one; the account transport's
 * server keeps a sent copy, others keep none. */
export function newIdentityForm(
  transports: readonly Transport[],
  me: string | undefined,
  suggestion?: IdentitySuggestion,
): IdentityForm {
  const transport =
    suggestion?.transport ?? transports.find((t) => t.enabled)?.id ?? transports[0]?.id ?? ''
  const kind = transports.find((t) => t.id === transport)?.kind
  return {
    address: suggestion?.address ?? '',
    name: '',
    replyTo: '',
    transport,
    sentCopy: kind === 'account' || kind === 'jmap' || kind === 'graph' ? 'provider' : 'none',
    sentFolder: '',
    bccSelf: false,
    plusAddresses: false,
    signatureHtml: '',
    signatureText: '',
    account: suggestion?.account ?? null,
    alias: suggestion?.alias ?? null,
    active: true,
    users: me ? [me] : [],
  }
}

export function identityRequest(form: IdentityForm): IdentityRequest {
  const signature = form.signatureHtml.replace(/<p><\/p>/g, '').trim()
  return {
    address: form.address.trim(),
    name: form.name.trim() || null,
    reply_to: form.replyTo.trim() || null,
    transport: form.transport,
    sent_copy: sentCopyValue(form.sentCopy, form.sentFolder),
    bcc_self: form.bccSelf,
    plus_addresses: form.plusAddresses,
    signature_html: signature || null,
    signature_text: form.signatureText.trim() || null,
    account: form.account,
    alias: form.alias,
    active: form.active,
  }
}

/** The changes an edited identity sends: every field, with empty strings clearing the optional
 * ones as the server expects. The account and alias links stay as they are. */
export function identityUpdate(form: IdentityForm): IdentityUpdateRequest {
  const request = identityRequest(form)
  return {
    address: request.address,
    name: request.name ?? '',
    reply_to: request.reply_to ?? '',
    transport: request.transport,
    sent_copy: request.sent_copy,
    bcc_self: request.bcc_self,
    plus_addresses: request.plus_addresses,
    signature_html: request.signature_html ?? '',
    signature_text: request.signature_text ?? '',
    active: request.active,
  }
}
