import { describe, expect, test } from 'bun:test'

import type { Outbox } from '@holtmail/api-client'

import {
  canUndo,
  groupOutbox,
  isScheduled,
  outboxActions,
  outboxGroup,
  refusedRecipients,
  resendRecipients,
  undoRemaining,
  undoSeconds,
} from './outbox.ts'

const may = { compose: true }

const entry = (patch: Partial<Outbox>): Outbox => ({
  id: 'o1',
  archive: 'a1',
  identity: 'i1',
  transport: 't1',
  mail_from: 'ada@example.com',
  state: 'queued',
  recipients: [{ address: 'bob@example.org', state: 'pending' }],
  size: 100,
  rfc_message_id: 'x@example.com',
  send_at: 10_000,
  attempts: 0,
  created_at: 0,
  updated_at: 0,
  ...patch,
})

describe('undo timing', () => {
  test('counts down to send_at while queued', () => {
    const queued = entry({ created_at: 0, send_at: 10_000 })
    expect(undoRemaining(queued, 0)).toBe(10_000)
    expect(undoSeconds(queued, 0)).toBe(10)
    expect(undoSeconds(queued, 9_001)).toBe(1)
    expect(canUndo(queued, 9_999)).toBe(true)
    expect(canUndo(queued, 10_000)).toBe(false)
    expect(undoSeconds(queued, 12_000)).toBe(0)
  })

  test('nothing to undo once it left the queue', () => {
    for (const state of ['sending', 'sent', 'cancelled', 'retrying']) {
      expect(canUndo(entry({ state, send_at: 100_000 }), 0)).toBe(false)
    }
  })

  test('a wait longer than a minute is a scheduled send', () => {
    expect(isScheduled(entry({ created_at: 0, send_at: 10_000 }))).toBe(false)
    expect(isScheduled(entry({ created_at: 0, send_at: 60_001 }))).toBe(true)
    expect(isScheduled(entry({ state: 'sent', created_at: 0, send_at: 3_600_000 }))).toBe(false)
  })
})

describe('outbox page', () => {
  test('groups by state', () => {
    expect(outboxGroup(entry({}))).toBe('waiting')
    expect(outboxGroup(entry({ send_at: 3_600_000 }))).toBe('scheduled')
    expect(outboxGroup(entry({ state: 'retrying' }))).toBe('waiting')
    expect(outboxGroup(entry({ state: 'uncertain' }))).toBe('uncertain')
    expect(outboxGroup(entry({ state: 'draft' }))).toBe('cancelled')
    const groups = groupOutbox([
      entry({ id: 'a', state: 'sent', created_at: 1 }),
      entry({ id: 'b', state: 'failed' }),
      entry({ id: 'c', state: 'sent', created_at: 5 }),
    ])
    expect(groups.map((g) => g.group)).toEqual(['failed', 'sent'])
    expect(groups[1]?.entries.map((e) => e.id)).toEqual(['c', 'a'])
  })

  test('offers the actions each state allows', () => {
    expect(outboxActions(entry({}), may)).toMatchObject({
      cancel: true,
      retry: false,
      resend: false,
    })
    expect(outboxActions(entry({ state: 'uncertain' }), may)).toMatchObject({
      cancel: false,
      retry: true,
      confirmRetry: true,
      resend: false,
    })
    expect(outboxActions(entry({ state: 'failed' }), may)).toMatchObject({
      retry: true,
      resend: true,
    })
    expect(outboxActions(entry({ state: 'sending' }), may)).toMatchObject({
      cancel: false,
      retry: false,
      resend: false,
    })
    const partly = entry({
      state: 'sent',
      recipients: [
        { address: 'bob@example.org', state: 'accepted' },
        { address: 'cy@example.net', state: 'refused', permanent: true, code: 550, text: 'no' },
      ],
    })
    expect(refusedRecipients(partly)).toEqual(['cy@example.net'])
    expect(outboxActions(partly, may)).toMatchObject({
      resend: true,
      resendRefused: true,
      confirmResend: true,
    })
    expect(resendRecipients(partly, 'resend')).toEqual(['bob@example.org', 'cy@example.net'])
    expect(resendRecipients(partly, 'resendRefused')).toEqual(['cy@example.net'])
    expect(outboxActions(entry({ state: 'failed' }), may)).toMatchObject({
      resendRefused: false,
      confirmResend: false,
    })
    expect(outboxActions(partly, { compose: false })).toMatchObject({
      retry: false,
      resend: false,
      resendRefused: false,
    })
    expect(outboxActions(entry({}), { compose: false })).toMatchObject({ cancel: true })
  })
})
