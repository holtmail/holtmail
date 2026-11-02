export interface KeyStroke {
  key: string
  mod: boolean
  alt: boolean
  shift: boolean
}

export interface KeyInput {
  key: string
  metaKey: boolean
  ctrlKey: boolean
  altKey: boolean
  shiftKey: boolean
}

const aliases: Record<string, string> = {
  esc: 'Escape',
  escape: 'Escape',
  enter: 'Enter',
  up: 'ArrowUp',
  down: 'ArrowDown',
  left: 'ArrowLeft',
  right: 'ArrowRight',
  space: ' ',
}

function parseStroke(text: string): KeyStroke {
  const parts = text === '+' ? ['+'] : text.split('+')
  const stroke: KeyStroke = { key: '', mod: false, alt: false, shift: false }
  for (const part of parts) {
    const lower = part.toLowerCase()
    if (part.length > 1 && (lower === 'mod' || lower === 'cmd' || lower === 'ctrl')) {
      stroke.mod = true
    } else if (part.length > 1 && lower === 'alt') stroke.alt = true
    else if (part.length > 1 && lower === 'shift') stroke.shift = true
    else stroke.key = aliases[lower] ?? part
  }
  return stroke
}

export function parseShortcut(text: string): KeyStroke[] {
  return text.trim().split(/\s+/).map(parseStroke)
}

function sameKey(a: string, b: string): boolean {
  return a.length === 1 && b.length === 1 ? a.toLowerCase() === b.toLowerCase() : a === b
}

function matchStroke(stroke: KeyStroke, input: KeyInput, isMac: boolean): boolean {
  const mod = isMac ? input.metaKey : input.ctrlKey
  const otherMod = isMac ? input.ctrlKey : input.metaKey
  if (stroke.mod !== mod || otherMod || stroke.alt !== input.altKey) return false
  const printable = input.key.length === 1
  const letter = printable && input.key.toLowerCase() !== input.key.toUpperCase()
  if (letter) {
    if (stroke.shift !== input.shiftKey) return false
    return sameKey(stroke.key, input.key)
  }
  if (!printable && stroke.shift !== input.shiftKey) return false
  return stroke.key === input.key
}

export interface SequenceMatcher<Id extends string> {
  feed(input: KeyInput, now: number): Id | undefined
  reset(): void
}

export function createMatcher<Id extends string>(
  bindings: Readonly<Record<Id, readonly string[]>>,
  isMac: boolean,
  timeout = 1000,
): SequenceMatcher<Id> {
  const parsed: { id: Id; strokes: KeyStroke[] }[] = []
  for (const id in bindings) {
    for (const text of bindings[id]) parsed.push({ id, strokes: parseShortcut(text) })
  }
  let pending: KeyInput[] = []
  let last = 0

  const attempt = (sequence: KeyInput[]) => {
    let prefix = false
    for (const { id, strokes } of parsed) {
      if (strokes.length < sequence.length) continue
      const matches = sequence.every((input, i) => {
        const stroke = strokes[i]
        return stroke !== undefined && matchStroke(stroke, input, isMac)
      })
      if (!matches) continue
      if (strokes.length === sequence.length) return { id, prefix: false }
      prefix = true
    }
    return { id: undefined, prefix }
  }

  return {
    feed(input, now) {
      if (['Shift', 'Control', 'Alt', 'Meta'].includes(input.key)) return undefined
      if (now - last > timeout) pending = []
      last = now
      for (const sequence of [[...pending, input], [input]]) {
        const { id, prefix } = attempt(sequence)
        if (id) {
          pending = []
          return id
        }
        if (prefix) {
          pending = sequence
          return undefined
        }
        if (pending.length === 0) break
      }
      pending = []
      return undefined
    },
    reset() {
      pending = []
    },
  }
}

export function displayStroke(text: string, isMac: boolean): string[] {
  return parseShortcut(text).map((stroke) => {
    const parts: string[] = []
    if (stroke.mod) parts.push(isMac ? '⌘' : 'Ctrl')
    if (stroke.alt) parts.push(isMac ? '⌥' : 'Alt')
    if (stroke.shift) parts.push(isMac ? '⇧' : 'Shift')
    const names: Record<string, string> = {
      ArrowUp: '↑',
      ArrowDown: '↓',
      ArrowLeft: '←',
      ArrowRight: '→',
      Escape: 'Esc',
      Enter: 'Enter',
      ' ': 'Space',
    }
    parts.push(names[stroke.key] ?? stroke.key.toUpperCase())
    return parts.join(isMac ? '' : '+')
  })
}

export function isEditable(target: EventTarget | null): boolean {
  if (typeof HTMLElement === 'undefined' || !(target instanceof HTMLElement)) return false
  if (target.isContentEditable) return true
  const tag = target.tagName
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true
  if (!(target instanceof HTMLInputElement)) return false
  return !['checkbox', 'radio', 'button', 'submit', 'reset'].includes(target.type)
}
