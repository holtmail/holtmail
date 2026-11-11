import { describe, expect, test } from 'bun:test'

import { ApiError, type Device } from '@holtmail/api-client'

import {
  buildState,
  changeDevice,
  createDeviceRequest,
  deviceFormOf,
  deviceFormProblem,
  domainList,
  fitsProfile,
  formatDeviceDay,
  formatNoticeDate,
  hasChanges,
  NOTICE_WINDOW_MS,
  needsSignInAgain,
  newDeviceForm,
  parseDismissedNotices,
  recentDeviceNotices,
  runsOnIosHomeScreen,
  serverRows,
  sortedFlagWords,
  unservedProtocols,
  updateDeviceRequest,
  withProtocol,
} from './devices.ts'

const device: Device = {
  id: 'd1',
  name: 'Phone',
  mail_archive: 'a1',
  archives: ['a1'],
  protocols: ['imap', 'jmap'],
  scopes: ['read'],
  implicit_seen: false,
  has_secret: true,
  created_at: 1_000,
  last_used_day: null,
  last_protocol: null,
  revoked_at: null,
  raised_at: null,
}

const archives = [{ id: 'a1' }, { id: 'a2' }]

const form = () => ({ ...newDeviceForm(archives), name: 'Phone' })

describe('the form', () => {
  test('a new device reads only', () => {
    const fresh = newDeviceForm(archives)
    expect(fresh.scopes).toEqual(['read'])
    expect(fresh.protocols).toEqual([])
    expect(fresh.mailArchive).toBe('a1')
  })

  test('turning a protocol on adds the scope it cannot sign in without', () => {
    expect(withProtocol(form(), 'smtp', true).scopes).toEqual(['read', 'send:mail'])
    const noRead = { ...form(), scopes: ['send:mail'] }
    expect(withProtocol(noRead, 'imap', true).scopes).toEqual(['send:mail', 'read'])
    const off = withProtocol(withProtocol(form(), 'smtp', true), 'smtp', false)
    expect(off.protocols).toEqual([])
    expect(off.scopes).toContain('send:mail')
  })

  test('names the first problem', () => {
    expect(deviceFormProblem({ ...form(), name: '  ' })).toBe('name')
    expect(deviceFormProblem(form())).toBe('protocol')
    expect(deviceFormProblem({ ...form(), protocols: ['jmap'], scopes: [] })).toBe('scope')
    expect(deviceFormProblem({ ...form(), protocols: ['imap'], mailArchive: '' })).toBe(
      'mailArchive',
    )
    expect(deviceFormProblem({ ...form(), protocols: ['dav'], allArchives: false })).toBe(
      'archives',
    )
    expect(deviceFormProblem({ ...form(), protocols: ['smtp'] })).toBe('smtpNeedsSend')
    expect(deviceFormProblem({ ...form(), protocols: ['jmap'], scopes: ['notify'] })).toBe(
      'needsRead',
    )
    expect(deviceFormProblem({ ...form(), protocols: ['smtp'], scopes: ['send:mail'] })).toBeNull()
  })

  test('a create asks only for what its protocols use', () => {
    const smtp = createDeviceRequest({ ...form(), protocols: ['smtp'], scopes: ['send:mail'] })
    expect(smtp.mail_archive).toBe('a1')
    expect(smtp.archives).toEqual([])
    const jmap = createDeviceRequest({ ...form(), name: ' Tab ', protocols: ['jmap'] })
    expect(jmap.name).toBe('Tab')
    expect(jmap.mail_archive).toBeNull()
    expect(jmap.archives).toBeNull()
    const chosen = createDeviceRequest({
      ...form(),
      protocols: ['dav', 'imap'],
      allArchives: false,
      archives: ['a2'],
      scopes: ['write:pim', 'read'],
      implicitSeen: true,
    })
    expect(chosen.archives).toEqual(['a2'])
    expect(chosen.protocols).toEqual(['imap', 'dav'])
    expect(chosen.scopes).toEqual(['read', 'write:pim'])
    expect(chosen.implicit_seen).toBe(true)
  })

  test('an edit sends only what changed', () => {
    const same = deviceFormOf(device, archives)
    expect(hasChanges(updateDeviceRequest(device, same))).toBe(false)
    expect(updateDeviceRequest(device, { ...same, name: 'Tablet' })).toEqual({ name: 'Tablet' })
    expect(updateDeviceRequest(device, { ...same, allArchives: true })).toEqual({ archives: null })
    expect(updateDeviceRequest(device, { ...same, mailArchive: 'a2' })).toEqual({
      mail_archive: 'a2',
    })
    expect(updateDeviceRequest(device, { ...same, scopes: ['write:mail', 'read'] }).scopes).toEqual(
      ['read', 'write:mail'],
    )
    const reordered = { ...same, protocols: ['jmap', 'imap'] }
    expect(hasChanges(updateDeviceRequest(device, reordered))).toBe(false)
  })

  test('an archive the user lost is left out of the form and of the list sent', () => {
    const stale = { ...device, archives: ['a1', 'gone'] }
    const edited = deviceFormOf(stale, archives)
    expect(edited.archives).toEqual(['a1'])
    expect(hasChanges(updateDeviceRequest(stale, edited, archives))).toBe(false)
    expect(updateDeviceRequest(stale, { ...edited, archives: ['a1', 'a2'] }, archives)).toEqual({
      archives: ['a1', 'a2'],
    })
  })

  test('dropping the only protocol that uses an archive leaves it alone', () => {
    const imapOnly = { ...deviceFormOf(device, archives), protocols: ['imap'] }
    expect(updateDeviceRequest(device, imapOnly)).toEqual({ protocols: ['imap'] })
  })
})

test('a notice date always names the day', () => {
  expect(formatNoticeDate(Date.UTC(2026, 10, 8, 12), 'en-GB')).toBe('8 Nov 2026')
})

test('only 428 asks to sign in again', () => {
  expect(needsSignInAgain(new ApiError(428, 'sign in again to do this'))).toBe(true)
  expect(needsSignInAgain(new ApiError(403, 'no'))).toBe(false)
  expect(needsSignInAgain(new Error('x'))).toBe(false)
})

test('server rows fall back to the browser host and follow the protocols', () => {
  const servers = [
    { protocol: 'smtp', host: null, port: 465, security: 'tls' },
    { protocol: 'imap', host: 'mail.example.com', port: 993, security: 'tls' },
  ]
  expect(serverRows(servers, 'web.example.org').map((row) => row.host)).toEqual([
    'web.example.org',
    'mail.example.com',
  ])
  expect(serverRows(servers, 'x', ['imap']).map((row) => row.port)).toEqual([993])
  expect(serverRows(undefined, 'x')).toEqual([])
})

test('an HTTPS protocol carries the address its client asks for', () => {
  const rows = serverRows(
    [
      {
        protocol: 'jmap',
        host: 'mail.example.com',
        port: 443,
        security: 'tls',
        url: 'https://mail.example.com/.well-known/jmap',
      },
      { protocol: 'imap', host: null, port: 993, security: 'tls' },
    ],
    'web.example.org',
  )
  expect(rows.map((row) => row.url)).toEqual(['https://mail.example.com/.well-known/jmap', null])
})

test('protocols without a listener are named, never the API', () => {
  const servers = [
    { protocol: 'imap', host: null, port: 993, security: 'tls' },
    { protocol: 'dav', host: 'mail.example.com', port: 443, security: 'tls' },
  ]
  expect(unservedProtocols(['api', 'jmap', 'smtp', 'imap', 'dav'], servers)).toEqual([
    'smtp',
    'jmap',
  ])
  expect(unservedProtocols(['api'], undefined)).toEqual([])
  expect(unservedProtocols(['imap'], undefined)).toEqual(['imap'])
})

test('a change names the viewer’s own device and no other', () => {
  const devices = [{ id: 'd1', name: 'Phone' }]
  expect(changeDevice('d1', devices)).toBe('Phone')
  expect(changeDevice('d9', devices)).toBeNull()
  expect(changeDevice('d1', undefined)).toBeNull()
  expect(changeDevice(null, devices)).toBeUndefined()
  expect(changeDevice(undefined, devices)).toBeUndefined()
})

test('the day of last use is a UTC day', () => {
  const midnight = Date.UTC(2026, 10, 8)
  expect(formatDeviceDay(midnight, 'en-GB')).toBe('8 Nov 2026')
})

describe('notices', () => {
  const now = 10 * NOTICE_WINDOW_MS

  test('a new device is noticed for a week, newest first', () => {
    const fresh = { ...device, id: 'd2', created_at: now - 2_000 }
    const newer = { ...device, id: 'd3', created_at: now - 1_000 }
    const old = { ...device, id: 'd4', created_at: now - NOTICE_WINDOW_MS }
    expect(recentDeviceNotices([fresh, old, newer], now).map((n) => n.key)).toEqual([
      `d3:${now - 1_000}`,
      `d2:${now - 2_000}`,
    ])
  })

  test('a raise is noticed as a raise and gets a new key', () => {
    const raised = { ...device, created_at: 5, raised_at: now - 1_000 }
    expect(recentDeviceNotices([raised], now)).toEqual([
      { key: `d1:${now - 1_000}`, name: 'Phone', kind: 'raised', at: now - 1_000 },
    ])
  })

  test('a revoked device needs no notice', () => {
    expect(recentDeviceNotices([{ ...device, created_at: now, revoked_at: now }], now)).toEqual([])
    expect(recentDeviceNotices(undefined, now)).toEqual([])
  })

  test('reads what this browser dismissed, whatever is stored', () => {
    expect(parseDismissedNotices('["a",1]')).toEqual(['a'])
    expect(parseDismissedNotices('{')).toEqual([])
    expect(parseDismissedNotices('{"a":1}')).toEqual([])
    expect(parseDismissedNotices(null)).toEqual([])
  })
})

describe('buildState', () => {
  const row = { archive_id: 'a', local_flagged: 0 }
  test('a built archive is ready', () => {
    expect(buildState({ ...row, mailboxes: 3, built: 3, assigned: 0, total: 0 })).toEqual({
      building: false,
      percent: 100,
    })
  })
  test('a building archive never shows 100 percent', () => {
    expect(buildState({ ...row, mailboxes: 3, built: 1, assigned: 999, total: 1000 })).toEqual({
      building: true,
      percent: 99,
    })
    expect(buildState({ ...row, mailboxes: 3, built: 1, assigned: 250, total: 1000 }).percent).toBe(
      25,
    )
  })
  test('a finished mailbox keeps counting, so the bar does not fall back', () => {
    expect(
      buildState({ ...row, mailboxes: 2, built: 1, assigned: 1100, total: 2000 }).percent,
    ).toBe(55)
  })
  test('an empty build starts at zero', () => {
    expect(buildState({ ...row, mailboxes: 1, built: 0, assigned: 0, total: 0 })).toEqual({
      building: true,
      percent: 0,
    })
  })
})

describe('sortedFlagWords', () => {
  test('system flags come first in a fixed order, keywords after by name', () => {
    const words = sortedFlagWords([
      { flag: 'Work', present: true },
      { flag: '\\Flagged', present: true },
      { flag: '$Label1', present: false },
      { flag: '\\SEEN', present: false },
    ])
    expect(words.map((w) => [w.system, w.name, w.present])).toEqual([
      ['seen', '\\SEEN', false],
      ['flagged', '\\Flagged', true],
      [null, '$Label1', false],
      [null, 'Work', true],
    ])
  })
  test('an unknown backslash flag is shown as written', () => {
    expect(sortedFlagWords([{ flag: '\\Recent', present: true }])[0]?.system).toBeNull()
  })
})

describe('fitsProfile', () => {
  test('needs every part the profile carries to be served', () => {
    const davOnly = [{ protocol: 'dav' }]
    const mailOnly = [{ protocol: 'imap' }, { protocol: 'smtp' }]
    expect(fitsProfile(['imap', 'smtp', 'dav'], davOnly)).toBe(false)
    expect(fitsProfile(['dav'], mailOnly)).toBe(false)
    expect(fitsProfile(['imap', 'smtp'], mailOnly)).toBe(true)
    expect(fitsProfile(['imap', 'dav'], davOnly)).toBe(true)
    expect(fitsProfile(['imap', 'smtp', 'dav'], [...mailOnly, ...davOnly])).toBe(true)
  })

  test('needs imap with smtp, or dav', () => {
    expect(fitsProfile(['imap', 'smtp'])).toBe(true)
    expect(fitsProfile(['dav'])).toBe(true)
    expect(fitsProfile(['imap', 'dav'])).toBe(true)
    expect(fitsProfile(['imap'])).toBe(false)
    expect(fitsProfile(['smtp', 'jmap'])).toBe(false)
    expect(fitsProfile([])).toBe(false)
  })
})

describe('domainList', () => {
  test('joins the domains in order, or gives null', () => {
    expect(domainList([], 'en')).toBeNull()
    expect(domainList(['example.com'], 'en')).toBe('example.com')
    expect(domainList(['example.com', 'example.org'], 'en')).toBe('example.com and example.org')
    expect(domainList(['example.com', 'example.org'], 'ru')).toBe('example.com и example.org')
  })
})

describe('runsOnIosHomeScreen', () => {
  test('only the iOS home-screen app counts', () => {
    expect(runsOnIosHomeScreen()).toBe(false)
    const realMatchMedia = globalThis.matchMedia
    globalThis.matchMedia = ((query: string) => ({
      matches: query.includes('standalone'),
    })) as never
    try {
      expect(runsOnIosHomeScreen()).toBe(false)
      Object.defineProperty(navigator, 'standalone', { value: true, configurable: true })
      expect(runsOnIosHomeScreen()).toBe(true)
    } finally {
      Reflect.deleteProperty(navigator, 'standalone')
      globalThis.matchMedia = realMatchMedia
    }
  })
})
