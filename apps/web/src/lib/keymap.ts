import type { strings } from '../strings.ts'

export const keymap = {
  next: ['j'],
  previous: ['k'],
  open: ['o'],
  back: ['esc'],
  search: ['/'],
  inbox: ['g i'],
  allMail: ['g a'],
  people: ['g c'],
  calendar: ['g e'],
  palette: ['mod+k'],
  help: ['?'],
  copyCode: ['c'],
  unsubscribe: ['shift+u'],
} as const satisfies Record<string, readonly string[]>

export type KeyAction = keyof typeof keymap

export const shortcutList: {
  action: keyof typeof strings.shortcuts.items
  keys: readonly string[]
}[] = [
  { action: 'next', keys: ['j', 'down'] },
  { action: 'previous', keys: ['k', 'up'] },
  { action: 'open', keys: ['enter', 'o'] },
  { action: 'back', keys: ['esc'] },
  { action: 'search', keys: ['/'] },
  { action: 'inbox', keys: ['g i'] },
  { action: 'allMail', keys: ['g a'] },
  { action: 'people', keys: ['g c'] },
  { action: 'calendar', keys: ['g e'] },
  { action: 'calendarToday', keys: ['t'] },
  { action: 'calendarViews', keys: ['a', 'w', 'm'] },
  { action: 'calendarPeriods', keys: ['left', 'right'] },
  { action: 'copyCode', keys: ['c'] },
  { action: 'unsubscribe', keys: ['shift+u'] },
  { action: 'palette', keys: ['mod+k'] },
  { action: 'help', keys: ['?'] },
]

export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad|iPod/.test(navigator.userAgent)
