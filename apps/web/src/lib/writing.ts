import { ApiError } from '@holtmail/api-client'
import type {
  AccountCapabilities,
  Archive,
  Capability,
  DraftAttachment,
  DraftPayload,
  GrantRole,
  Identity,
  MessageDetail,
} from '@holtmail/api-client'

const roleOrder: GrantRole[] = ['reader', 'editor', 'sender']

const capabilities: readonly string[] = [
  'mail.flags',
  'mail.move',
  'mail.delete',
  'mail.expunge',
  'mail.append',
  'mail.send',
  'pim.edit',
  'pim.respond',
] satisfies Capability[]

export const isCapability = (value: string): value is Capability => capabilities.includes(value)

export const isGrantRole = (value: string): value is GrantRole =>
  (roleOrder as string[]).includes(value)

/** Whether `role` includes `needed`: sender includes editor, editor includes reader. */
export const roleIncludes = (role: string | undefined, needed: GrantRole): boolean =>
  role !== undefined && isGrantRole(role) && roleOrder.indexOf(role) >= roleOrder.indexOf(needed)

/** Whether "Apply anyway" shows for an attendee's unverified answer: it needs an editor grant. */
export const canApplyAnswers = (archive: Pick<Archive, 'rights'> | undefined): boolean =>
  roleIncludes(archive?.rights.role, 'editor')

/** Whether the compose button, reply actions and the outbox show in this archive. */
export const canCompose = (archive: Pick<Archive, 'rights'> | undefined): boolean =>
  archive?.rights.compose === true

/** The grant roles an actor may hand out: admins give only `reader`. */
export const grantableRoles = (actorRole: string | undefined): GrantRole[] =>
  actorRole === 'owner' ? roleOrder : ['reader']

/** Whether an actor may change a grant that is `current` now: only the owner touches writing
 * grants. */
export const mayChangeGrant = (actorRole: string | undefined, current: GrantRole | undefined) =>
  actorRole === 'owner' || current === undefined || current === 'reader'

export interface CapabilitySwitch {
  on: boolean
  /** On and allowed by the instance switches. */
  effective: boolean
  /** Cannot be turned on now: expunge needs delete. */
  blocked: boolean
}

export function capabilitySwitch(
  dto: Pick<AccountCapabilities, 'capabilities' | 'active'>,
  capability: Capability,
): CapabilitySwitch {
  const on = dto.capabilities.includes(capability)
  return {
    on,
    effective: dto.active.includes(capability),
    blocked: capability === 'mail.expunge' && !on && !dto.capabilities.includes('mail.delete'),
  }
}

/** The capability list after one switch: expunge cannot be on without delete, so turning delete
 * off turns expunge off and expunge cannot be turned on alone. */
export function toggleCapability(
  current: readonly string[],
  capability: Capability,
  on: boolean,
): Capability[] {
  const set = new Set(current.filter(isCapability))
  if (on) {
    if (capability === 'mail.expunge' && !set.has('mail.delete')) return [...set]
    set.add(capability)
  } else {
    set.delete(capability)
    if (capability === 'mail.delete') set.delete('mail.expunge')
  }
  return [...set]
}

export type ReaderAction =
  | 'reply'
  | 'replyAll'
  | 'replyList'
  | 'forward'
  | 'forwardAttachment'
  | 'editAsNew'

/** The answers the reader offers: none without a right to compose, Reply to list only when the
 * message names a list. */
export function readerActions(
  message: Pick<MessageDetail, 'list_post'> | undefined,
  compose: boolean,
): ReaderAction[] {
  if (!message || !compose) return []
  const actions: ReaderAction[] = ['reply', 'replyAll']
  if (message.list_post) actions.push('replyList')
  actions.push('forward', 'forwardAttachment', 'editAsNew')
  return actions
}

/** A new draft with the message's recipients, subject, text and files, to change and send
 * again. Inline parts are left out; threading headers are not copied. */
const mailboxes = (list: MessageDetail['to']) =>
  list.map((a) => (a.name ? { name: a.name, address: a.email } : { address: a.email }))

export function editAsNewPayload(message: MessageDetail): DraftPayload {
  const attachments: DraftAttachment[] = message.attachments
    .filter((a) => !a.inline)
    .map((a) => archivedAttachment(message.id, a))
  return {
    to: mailboxes(message.to),
    cc: mailboxes(message.cc),
    bcc: [],
    subject: message.subject ?? '',
    text: message.text ?? '',
    html: null,
    attachments,
    references: [],
    context: { kind: 'new', message: null },
  }
}

const quoteMarkers = ['<div class="holtmail-quote">', '<div class="holtmail-forward">']

/** Splits template HTML into the part the editor edits and the quoted or forwarded original,
 * which is sent unchanged after it. */
export function splitQuote(html: string | null | undefined): { own: string; quote: string } {
  const value = html ?? ''
  const at = Math.min(
    ...quoteMarkers.map((marker) => {
      const index = value.indexOf(marker)
      return index < 0 ? Number.POSITIVE_INFINITY : index
    }),
  )
  if (!Number.isFinite(at)) return { own: value, quote: '' }
  return { own: value.slice(0, at), quote: value.slice(at) }
}

const escapeHtml = (text: string) =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;')

/** Plain text as editor paragraphs: blank lines start a paragraph, single breaks stay. */
export function textToHtml(text: string): string {
  const paragraphs = text.replace(/\r\n/g, '\n').split(/\n{2,}/)
  return paragraphs.map((p) => `<p>${escapeHtml(p).replace(/\n/g, '<br>')}</p>`).join('')
}

/** Editor HTML as plain text for the plain-text mode: paragraphs become blank-line separated,
 * list items get a dash, quotes get `> `. */
export function htmlToText(html: string): string {
  const out = html
    .replace(/<br\s*\/?>/gi, '\n')
    .replace(/<li[^>]*>/gi, '- ')
    .replace(/<\/(p|div|h[1-6]|li|blockquote|pre)>/gi, '\n\n')
    .replace(/<[^>]+>/g, '')
    .replace(/&nbsp;/g, ' ')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&')
  return out.replace(/\n{3,}/g, '\n\n').trim()
}

/** The signature as plain text with the usual `-- ` separator line; empty without one. */
export function signatureText(identity: Pick<Identity, 'signature_text' | 'signature_html'>) {
  const text = identity.signature_text?.trim() || htmlToText(identity.signature_html ?? '')
  return text ? `-- \n${text}` : ''
}

/** The signature as editor HTML; made from the text signature when there is no HTML one. */
export function signatureHtml(identity: Pick<Identity, 'signature_text' | 'signature_html'>) {
  const html = identity.signature_html?.trim()
  if (html) return `<p>-- </p>${html}`
  const text = identity.signature_text?.trim()
  return text ? `<p>-- </p>${textToHtml(text)}` : ''
}

/** Plain text with the signature placed before a quoted original, or at the end. */
export function insertTextSignature(text: string, signature: string): string {
  if (!signature || text.includes(signature)) return text
  const quote = text.search(/\n\n(On .* wrote:|---------- Forwarded message ----------)\n/)
  if (quote < 0) return `${text.replace(/\s+$/, '')}\n\n${signature}\n`
  return `${text.slice(0, quote).replace(/\s+$/, '')}\n\n${signature}${text.slice(quote)}`
}

/** A `datetime-local` value for a time, in the browser's zone. */
const pad = (n: number) => String(n).padStart(2, '0')

export function toLocalInput(time: number): string {
  const date = new Date(time)
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`
}

/** A `datetime-local` value as Unix milliseconds; `undefined` when empty or malformed. */
export function fromLocalInput(value: string): number | undefined {
  if (!value) return undefined
  const time = new Date(value).getTime()
  return Number.isNaN(time) ? undefined : time
}

/** Whether a failed send got a definite answer, so the next try must use a new
 * `Idempotency-Key`: the server keeps a `4xx` for the key and would only replay it, or refuse a
 * changed draft with `422`. A network error or a `5xx` keeps the key, so a repeat is safe. */
export const renewsSendKey = (error: unknown): boolean =>
  error instanceof ApiError && error.status >= 400 && error.status < 500

/** Whether closing a draft should delete it: a new draft left empty, or a draft this window
 * started (a reply, a forward, a new message) that the user never touched. */
export const discardsOnClose = (
  payload: DraftPayload,
  { fresh, touched }: { fresh: boolean; touched: boolean },
): boolean =>
  (fresh && !touched) || (isEmptyPayload(payload) && (payload.context?.kind ?? 'new') === 'new')

export function isEmptyPayload(p: DraftPayload): boolean {
  return (
    (p.to ?? []).length + (p.cc ?? []).length + (p.bcc ?? []).length === 0 &&
    !p.subject?.trim() &&
    !p.text?.trim() &&
    !htmlToText(p.html ?? '').trim() &&
    (p.attachments ?? []).length === 0
  )
}

/** An archived message's attachment as a draft attachment; the server resolves it to its blob. */
export function archivedAttachment(
  message: string,
  attachment: Pick<
    MessageDetail['attachments'][number],
    'ordinal' | 'filename' | 'content_type' | 'size'
  >,
): DraftAttachment {
  return {
    message,
    ordinal: attachment.ordinal,
    name: attachment.filename ?? null,
    content_type: attachment.content_type,
    size: attachment.size,
  }
}
