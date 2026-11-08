import { describe, expect, test } from 'bun:test'

import type { Folder, MailChange, MessageDetail } from '@holtmail/api-client'

import { keymap, shortcutList } from './keymap.ts'
import {
  describeChange,
  heldLines,
  mayDismiss,
  mayRetry,
  messageActions,
  moveTargets,
  parseRetentionDays,
  planDelete,
  planReadOnOpen,
  planToggle,
  restoreTargets,
  shownFlags,
  withFlags,
  writableByAccount,
} from './mailbox.ts'
import { parseReadOnOpen } from './read-on-open.ts'

const folder = (id: string, account: string, path: string, role?: string): Folder => ({
  id,
  account_id: account,
  path,
  name: path.split('/').at(-1) ?? path,
  role: role ?? null,
  messages: 0,
  unread: 0,
})

const folders = [
  folder('in', 'work', 'INBOX', 'inbox'),
  folder('arch', 'work', 'Archive', 'archive'),
  folder('trash', 'work', 'Trash', 'trash'),
  folder('home-in', 'home', 'INBOX', 'inbox'),
  folder('home-old', 'home', 'Old'),
  folder('import', 'files', 'Imported'),
]

type Message = Pick<MessageDetail, 'locations' | 'pending'>

const message = (
  locations: { folder: string; flags?: string[]; removed?: number }[],
  pending: MailChange[] = [],
): Message => ({
  locations: locations.map((l) => ({
    folder_id: l.folder,
    flags: l.flags ?? [],
    removed_at: l.removed ?? null,
  })),
  pending,
})

const change = (overrides: Partial<MailChange>): MailChange => ({
  id: 'c1',
  account_id: 'work',
  folder_id: 'in',
  kind: 'flags',
  add: [],
  remove: [],
  origin: 'user',
  state: 'held',
  attempts: 0,
  created_at: 0,
  updated_at: 0,
  ...overrides,
})

const writable = writableByAccount([
  { id: 'work', writable: ['mail.flags', 'mail.move', 'mail.delete'] },
  { id: 'home', writable: ['mail.append'] },
  { id: 'files', writable: [] },
])

describe('mailbox actions', () => {
  test('offers each action only where an account allows it', () => {
    const inWork = message([{ folder: 'in' }])
    expect(messageActions(inWork, folders, writable)).toEqual({
      flags: true,
      move: true,
      delete: true,
      restore: true,
    })
    const inHome = message([{ folder: 'home-in' }])
    expect(messageActions(inHome, folders, writable)).toEqual({
      flags: false,
      move: false,
      delete: false,
      restore: true,
    })
    const nothing = writableByAccount([])
    expect(messageActions(inWork, folders, nothing)).toEqual({
      flags: false,
      move: false,
      delete: false,
      restore: false,
    })
  })

  test('a removed location and a message already in the trash offer no delete', () => {
    expect(messageActions(message([{ folder: 'in', removed: 5 }]), folders, writable).flags).toBe(
      false,
    )
    expect(messageActions(message([{ folder: 'trash' }]), folders, writable).delete).toBe(false)
  })

  test('only delete still offers the trash as a move target', () => {
    const onlyDelete = writableByAccount([{ id: 'work', writable: ['mail.delete'] }])
    const groups = moveTargets(message([{ folder: 'in' }]), folders, onlyDelete)
    expect(groups.flatMap((g) => g.folders.map((f) => f.id))).toEqual(['trash'])
    const all = moveTargets(message([{ folder: 'in' }]), folders, writable)
    expect(all.flatMap((g) => g.folders.map((f) => f.id))).toEqual(['arch', 'trash'])
  })

  test('restore lists the folders of accounts that may save into folders', () => {
    const groups = restoreTargets(folders, [], writable)
    expect(groups).toHaveLength(1)
    expect(groups[0]?.accountId).toBe('home')
    expect(groups[0]?.folders.map((f) => f.id)).toEqual(['home-in', 'home-old'])
  })
})

describe('flags shown and toggles', () => {
  test('open changes show at once, applied in order', () => {
    const m = message(
      [{ folder: 'in', flags: ['\\Flagged'] }],
      [
        change({ add: ['\\Seen'] }),
        change({ id: 'c2', remove: ['\\FLAGGED'] }),
        change({ id: 'c3', add: ['$Junk'], state: 'failed' }),
      ],
    )
    expect(shownFlags(m, folders)).toEqual(['\\Seen'])
  })

  test('withFlags compares ignoring case and keeps one copy', () => {
    expect(withFlags(['\\seen', '$a'], ['\\Seen', '$b'], ['$A'])).toEqual(['\\seen', '$b'])
  })

  test('toggling read marks read where unread and only on writable accounts', () => {
    const m = message([{ folder: 'in' }, { folder: 'home-in' }])
    expect(planToggle(m, folders, writable, 'read')).toEqual([
      { account: 'work', op: { kind: 'flags', add: ['\\Seen'], remove: [] } },
    ])
    const read = message([{ folder: 'in', flags: ['\\Seen'] }])
    expect(planToggle(read, folders, writable, 'read')).toEqual([
      { account: 'work', op: { kind: 'flags', add: [], remove: ['\\Seen'] } },
    ])
  })

  test('toggling the star follows what the reader shows, held changes included', () => {
    const m = message([{ folder: 'in' }], [change({ add: ['\\Flagged'] })])
    expect(planToggle(m, folders, writable, 'flagged')).toEqual([
      { account: 'work', op: { kind: 'flags', add: [], remove: ['\\Flagged'] } },
    ])
  })

  test('mark as read when opened is off by default and never repeats a held change', () => {
    const unread = message([{ folder: 'in' }])
    expect(planReadOnOpen(unread, folders, writable, false)).toEqual([])
    expect(planReadOnOpen(unread, folders, writable, true)).toHaveLength(1)
    const held = message([{ folder: 'in' }], [change({ remove: ['\\Seen'] })])
    expect(planReadOnOpen(held, folders, writable, true)).toEqual([])
    const read = message([{ folder: 'in', flags: ['\\Seen'] }])
    expect(planReadOnOpen(read, folders, writable, true)).toEqual([])
    const elsewhere = message([{ folder: 'home-in' }])
    expect(planReadOnOpen(elsewhere, folders, writable, true)).toEqual([])
  })

  test('the stored choice is on only when it says so', () => {
    expect(parseReadOnOpen('on')).toBe(true)
    expect(parseReadOnOpen('off')).toBe(false)
    expect(parseReadOnOpen(null)).toBe(false)
    expect(parseReadOnOpen('yes')).toBe(false)
  })

  test('delete asks each account with a place outside its trash', () => {
    const m = message([{ folder: 'in' }, { folder: 'trash' }, { folder: 'home-in' }])
    expect(planDelete(m, folders, writable)).toEqual([{ account: 'work', op: { kind: 'delete' } }])
    expect(planDelete(message([{ folder: 'trash' }]), folders, writable)).toEqual([])
  })
})

describe('held mark', () => {
  test('names each open change once, with its folder', () => {
    const pending = [
      change({ add: ['\\Seen'] }),
      change({ id: 'c2', add: ['\\Seen'], folder_id: 'home-in' }),
      change({ id: 'c3', kind: 'move', target_folder_id: 'arch' }),
      change({ id: 'c4', kind: 'move', target_folder_id: 'trash', state: 'sending' }),
      change({ id: 'c5', kind: 'append', folder_id: 'home-old' }),
      change({ id: 'c6', kind: 'delete', expunge: true, state: 'applied' }),
    ]
    expect(heldLines(pending, folders).map((l) => [l.kind, l.folder])).toEqual([
      ['read', undefined],
      ['move', 'Archive'],
      ['trash', 'Trash'],
      ['restore', 'Old'],
    ])
  })

  test('a failed change carries its code', () => {
    const line = describeChange(
      change({ state: 'failed', last_error: 'unsupported', remove: ['\\Flagged'] }),
      folders,
    )
    expect(line).toEqual({ id: 'c1', kind: 'unstarred', error: 'unsupported' })
    expect(describeChange(change({ add: ['$Work', '\\Seen'] }), folders).kind).toBe('flags')
  })
})

describe('failed changes', () => {
  test('retry needs the lock the change needs on its account', () => {
    const deleteOnly = writableByAccount([{ id: 'work', writable: ['mail.delete'] }])
    expect(mayRetry(change({}), folders, writable)).toBe(true)
    expect(mayRetry(change({ account_id: 'home' }), folders, writable)).toBe(false)
    expect(mayRetry(change({ kind: 'delete', expunge: true }), folders, writable)).toBe(false)
    expect(mayRetry(change({ kind: 'move', target_folder_id: 'trash' }), folders, deleteOnly)).toBe(
      true,
    )
    expect(mayRetry(change({ kind: 'move', target_folder_id: 'arch' }), folders, deleteOnly)).toBe(
      false,
    )
    expect(mayRetry(change({ account_id: 'home', kind: 'append' }), folders, writable)).toBe(true)
  })

  test('dismiss is for editors of the account, owners and admins', () => {
    expect(mayDismiss(change({}), writable, false)).toBe(true)
    expect(mayDismiss(change({ account_id: 'files' }), writable, false)).toBe(false)
    expect(mayDismiss(change({ account_id: 'files' }), writable, true)).toBe(true)
  })
})

describe('retention days', () => {
  test('whole days from 1 to 36 500, empty for none', () => {
    expect(parseRetentionDays('30')).toBe(30)
    expect(parseRetentionDays(' 365 ')).toBe(365)
    expect(parseRetentionDays('')).toBeNull()
    expect(parseRetentionDays('0')).toBeUndefined()
    expect(parseRetentionDays('36501')).toBeUndefined()
    expect(parseRetentionDays('1.5')).toBeUndefined()
    expect(parseRetentionDays('-3')).toBeUndefined()
  })
})

describe('shortcuts', () => {
  test('mailbox shortcuts are bound and listed with the same keys', () => {
    for (const action of ['toggleRead', 'toggleFlag', 'move', 'trash'] as const) {
      const listed = shortcutList.find((item) => item.action === action)
      expect(listed?.keys).toEqual([...keymap[action]])
    }
    const keys = Object.values(keymap).flat()
    expect(new Set(keys).size).toBe(keys.length)
  })
})
