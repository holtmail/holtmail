import { describe, expect, test } from 'bun:test'

import type { Folder } from '@holtmail/api-client'

import { findInbox, groupFolders, orderFolders } from './folders.ts'

let next = 0
const folder = (path: string, role?: string, account = 'acc1'): Folder => ({
  id: `f${(next += 1)}`,
  account_id: account,
  path,
  name: path.split('/').pop() ?? path,
  messages: 1,
  unread: 0,
  role: role ?? null,
})

describe('orderFolders', () => {
  test('roles first in a fixed order, then others alphabetically, children nested', () => {
    const folders = [
      folder('Work'),
      folder('Trash', 'trash'),
      folder('[Gmail]/Sent Mail', 'sent'),
      folder('INBOX', 'inbox'),
      folder('archive-2019'),
      folder('Work/Projects'),
      folder('Work/Projects/Atlas'),
      folder('Drafts', 'drafts'),
      folder('Junk', 'junk'),
      folder('Receipts'),
      folder('INBOX/Later'),
    ]
    expect(orderFolders(folders).map(({ folder: f, depth }) => `${depth}:${f.path}`)).toEqual([
      '0:INBOX',
      '1:INBOX/Later',
      '0:Drafts',
      '0:[Gmail]/Sent Mail',
      '0:Junk',
      '0:Trash',
      '0:archive-2019',
      '0:Receipts',
      '0:Work',
      '1:Work/Projects',
      '2:Work/Projects/Atlas',
    ])
  })

  test('a missing parent attaches the child to the nearest existing ancestor', () => {
    const folders = [folder('Clients'), folder('Clients/2023/Acme')]
    expect(orderFolders(folders).map(({ depth }) => depth)).toEqual([0, 1])
  })

  test('numbers sort naturally', () => {
    const folders = [folder('Project 10'), folder('Project 2')]
    expect(orderFolders(folders).map(({ folder: f }) => f.path)).toEqual([
      'Project 2',
      'Project 10',
    ])
  })
})

describe('groupFolders', () => {
  test('groups by account in account order, unknown accounts last', () => {
    const folders = [
      folder('INBOX', 'inbox', 'b'),
      folder('INBOX', 'inbox', 'a'),
      folder('x', undefined, 'z'),
    ]
    const idle = { running: false, new_messages: 0 }
    const groups = groupFolders(folders, [
      { id: 'a', name: 'Alpha', kind: 'imap', enabled: true, needs_sign_in: false, status: idle },
      { id: 'b', name: 'Beta', kind: 'imap', enabled: true, needs_sign_in: false, status: idle },
      { id: 'c', name: 'Empty', kind: 'imap', enabled: true, needs_sign_in: false, status: idle },
    ])
    expect(groups.map((group) => group.name)).toEqual(['Alpha', 'Beta', 'z'])
  })

  test('findInbox picks the inbox role', () => {
    const inbox = folder('Posteingang', 'inbox')
    expect(findInbox([folder('Other'), inbox])).toBe(inbox)
  })
})
