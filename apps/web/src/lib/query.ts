export const operators = [
  'from:',
  'to:',
  'subject:',
  'has:attachment',
  'before:',
  'after:',
  'in:',
  'is:unread',
] as const

export function insertToken(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  token: string,
): { value: string; caret: number } {
  const start = Math.max(0, Math.min(selectionStart, value.length))
  const end = Math.max(start, Math.min(selectionEnd, value.length))
  const before = value.slice(0, start)
  const after = value.slice(end)
  const lead = before && !/\s$/.test(before) ? ' ' : ''
  const open = token.endsWith(':')
  const trail = open || /^\s/.test(after) ? '' : ' '
  const next = `${before}${lead}${token}${trail}${after}`
  return { value: next, caret: before.length + lead.length + token.length + trail.length }
}
