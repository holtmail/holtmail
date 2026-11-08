import type { Mailbox, PersonSummary } from '@holtmail/api-client'

const addressPattern = /^[^\s@<>(),;:"[\]\\]+@[^\s@<>(),;:"[\]\\]+\.[^\s@<>(),;:"[\]\\]+$/

/** Whether `text` is a plain `local@domain.tld` address. */
export const isAddress = (text: string): boolean => addressPattern.test(text.trim())

/** Reads one recipient: `ada@example.com`, `Ada <ada@example.com>` or `"Lovelace, Ada"
 * <ada@example.com>`; `undefined` when it holds no valid address. */
export function parseMailbox(text: string): Mailbox | undefined {
  const value = text.trim()
  if (!value) return undefined
  const angle = /^(.*?)<([^<>]+)>$/.exec(value)
  if (angle) {
    const address = (angle[2] ?? '').trim()
    if (!isAddress(address)) return undefined
    const name = (angle[1] ?? '')
      .trim()
      .replace(/^"(.*)"$/, '$1')
      .trim()
    return name ? { name, address } : { address }
  }
  const bare = value.replace(/^mailto:/i, '')
  return isAddress(bare) ? { address: bare } : undefined
}

/** Splits typed or pasted text at commas, semicolons and line breaks outside quotes and angle
 * brackets. */
export function splitRecipientText(text: string): string[] {
  const parts: string[] = []
  let current = ''
  let quoted = false
  let angled = false
  for (const char of text) {
    if (char === '"' && !angled) quoted = !quoted
    else if (char === '<' && !quoted) angled = true
    else if (char === '>' && !quoted) angled = false
    if (!quoted && !angled && (char === ',' || char === ';' || char === '\n' || char === '\r')) {
      if (current.trim()) parts.push(current.trim())
      current = ''
    } else {
      current += char
    }
  }
  if (current.trim()) parts.push(current.trim())
  return parts
}

/** Turns typed text into recipients; parts that are no address come back in `invalid`. */
export function parseRecipients(text: string): { mailboxes: Mailbox[]; invalid: string[] } {
  const mailboxes: Mailbox[] = []
  const invalid: string[] = []
  for (const part of splitRecipientText(text)) {
    const mailbox = parseMailbox(part)
    if (mailbox) mailboxes.push(mailbox)
    else invalid.push(part)
  }
  return { mailboxes, invalid }
}

const key = (address: string) => address.trim().toLowerCase()

/** Adds recipients to a list, each address once ignoring case; the first spelling wins, but a
 * name fills in a recipient that had none. */
export function addRecipients(list: readonly Mailbox[], incoming: readonly Mailbox[]): Mailbox[] {
  const out = list.map((m) => ({ ...m }))
  for (const mailbox of incoming) {
    const existing = out.find((m) => key(m.address) === key(mailbox.address))
    if (!existing) out.push({ ...mailbox })
    else if (!existing.name && mailbox.name) existing.name = mailbox.name
  }
  return out
}

export const removeRecipient = (list: readonly Mailbox[], address: string): Mailbox[] =>
  list.filter((m) => key(m.address) !== key(address))

/** What a chip shows: the name when there is one, else the address. */
export const chipLabel = (mailbox: Mailbox): string => mailbox.name?.trim() || mailbox.address

/** The recipient as a header would write it. */
export function formatMailbox(mailbox: Mailbox): string {
  const name = mailbox.name?.trim()
  if (!name) return mailbox.address
  return /[",;<>@()]/.test(name)
    ? `"${name.replace(/"/g, '')}" <${mailbox.address}>`
    : `${name} <${mailbox.address}>`
}

/** Every recipient of the three fields, each address once. */
export const allRecipients = (...lists: readonly (readonly Mailbox[] | undefined)[]): Mailbox[] =>
  lists.reduce<Mailbox[]>((all, list) => addRecipients(all, list ?? []), [])

/** People addresses matching `query` (name or address, case-insensitive), without the ones
 * already chosen; at most `limit`, people with more mail first as the server sorts them. */
export function peopleSuggestions(
  people: readonly PersonSummary[],
  query: string,
  chosen: readonly Mailbox[],
  limit = 8,
): Mailbox[] {
  const q = query.trim().toLowerCase()
  if (!q) return []
  const taken = new Set(chosen.map((m) => key(m.address)))
  const out: Mailbox[] = []
  for (const person of people) {
    const name = person.name?.trim() || undefined
    const addresses = person.emails.length > 0 ? person.emails : person.email ? [person.email] : []
    for (const address of addresses) {
      if (taken.has(key(address))) continue
      const nameHit = name?.toLowerCase().includes(q) ?? false
      if (!nameHit && !address.toLowerCase().includes(q)) continue
      taken.add(key(address))
      out.push(name ? { name, address } : { address })
      if (out.length >= limit) return out
    }
  }
  return out
}
