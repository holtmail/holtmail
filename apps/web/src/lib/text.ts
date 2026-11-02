export type TextPart = { kind: 'text'; text: string } | { kind: 'link'; text: string; href: string }

const pattern = /\b(?:https?:\/\/|www\.)[^\s<>"]+|\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b/gi
const trailing = /[.,;:!?)\]}'"]+$/

export function linkify(input: string): TextPart[] {
  const parts: TextPart[] = []
  let cursor = 0
  for (const match of input.matchAll(pattern)) {
    let text = match[0]
    const start = match.index
    const cut = trailing.exec(text)
    if (cut && !(cut[0].startsWith(')') && text.includes('('))) text = text.slice(0, cut.index)
    if (!text) continue
    if (start > cursor) parts.push({ kind: 'text', text: input.slice(cursor, start) })
    const href =
      text.includes('@') && !/^(https?:\/\/|www\.)/i.test(text)
        ? `mailto:${text}`
        : text.toLowerCase().startsWith('www.')
          ? `https://${text}`
          : text
    parts.push({ kind: 'link', text, href })
    cursor = start + text.length
  }
  if (cursor < input.length) parts.push({ kind: 'text', text: input.slice(cursor) })
  return parts
}

export function previewOf(text: string | null | undefined, length = 160): string {
  const flat = (text ?? '')
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('>'))
    .join(' ')
    .replace(/\s+/g, ' ')
    .trim()
  return flat.length > length ? `${flat.slice(0, length - 1)}…` : flat
}

export function withLinkTarget(html: string): string {
  const base = '<base target="_blank">'
  if (/<base\b/i.test(html)) return html
  if (/<head\b[^>]*>/i.test(html)) return html.replace(/<head\b[^>]*>/i, (head) => head + base)
  return base + html
}
