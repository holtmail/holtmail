import type { Address } from '@holtmail/api-client'

function displayName(address: Address | undefined): string | undefined {
  if (!address) return undefined
  return address.name?.trim() || address.email
}

export function senderLabel(from: readonly Address[], fallback: string): string {
  return displayName(from[0]) ?? fallback
}

export function formatAddress(address: Address): string {
  const name = address.name?.trim()
  return name && name !== address.email ? `${name} <${address.email}>` : address.email
}

export function replySubject(subject: string | null | undefined): string {
  const text = subject?.trim() ?? ''
  return /^re:/i.test(text) ? text : `Re: ${text}`.trim()
}

export interface ReplySource {
  from: readonly Address[]
  reply_to: readonly Address[]
  subject?: string | null
  rfc_message_id?: string | null
  references?: readonly string[]
  text?: string | null
}

const bracket = (id: string): string => (id.startsWith('<') ? id : `<${id}>`)

export function quoteLines(text: string | null | undefined, maxLines = 8): string[] {
  const lines = (text ?? '').replace(/\r\n?/g, '\n').split('\n')
  while (lines.length > 0 && lines[0]?.trim() === '') lines.shift()
  const kept = lines.slice(0, maxLines)
  while (kept.length > 0 && kept.at(-1)?.trim() === '') kept.pop()
  const quoted = kept.map((line) => (line.trim() === '' ? '>' : `> ${line.trimEnd()}`))
  if (kept.length > 0 && lines.slice(maxLines).some((line) => line.trim() !== '')) {
    quoted.push('> …')
  }
  return quoted
}

export function replyMailto(message: ReplySource, attribution?: string): string | undefined {
  const target = message.reply_to[0] ?? message.from[0]
  if (!target) return undefined
  const params = [`subject=${encodeURIComponent(replySubject(message.subject))}`]
  const id = message.rfc_message_id?.trim()
  if (id) {
    const chain = [
      ...new Set([...(message.references ?? []), id].map((ref) => bracket(ref.trim()))),
    ]
    params.push(`In-Reply-To=${encodeURIComponent(bracket(id))}`)
    params.push(`References=${encodeURIComponent(chain.join(' '))}`)
  }
  const quote = quoteLines(message.text)
  if (quote.length > 0) {
    const body = ['', '', ...(attribution ? [attribution] : []), ...quote].join('\r\n')
    params.push(`body=${encodeURIComponent(body)}`)
  }
  return `mailto:${encodeURIComponent(target.email).replace(/%40/g, '@')}?${params.join('&')}`
}
