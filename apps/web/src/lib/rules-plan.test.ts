import { describe, expect, test } from 'bun:test'

import {
  actionSummary,
  highlightOf,
  pendingActions,
  readPlan,
  readResults,
  stepAt,
} from './rules-plan.ts'

const plan = {
  ran: true,
  path: [
    { node: 'q', type: 'query', outcome: 'branch', port: 'yes' },
    { node: 'l', type: 'label', outcome: 'planned', action: 0 },
    {
      node: 'r',
      type: 'reply',
      outcome: 'skipped',
      reason: { code: 'replied_recently', until: '2026-11-18T00:00:00Z' },
    },
    { node: 's', type: 'stop', outcome: 'stopped' },
    { node: 'x', type: 'odd', outcome: 'surprise' },
  ],
  actions: [
    {
      node: 'l',
      needs_confirmation: false,
      action: { type: 'label', message: '0190c0de', name: 'Orders' },
    },
    {
      node: 'f',
      capability: 'mail.send',
      needs_confirmation: true,
      action: {
        type: 'forward',
        message: '0190c0de',
        identity: 'auto',
        to: ['accounting@example.org'],
        auto_submitted: 'auto-generated',
        as_attachment: false,
      },
    },
  ],
  stopped: true,
}

describe('readPlan', () => {
  test('reads every outcome and leaves out unknown ones', () => {
    const read = readPlan(plan)
    expect(read.ran).toBe(true)
    expect(read.stopped).toBe(true)
    expect(read.path.map((s) => s.outcome)).toEqual([
      { kind: 'branch', port: 'yes' },
      { kind: 'planned', action: 0 },
      { kind: 'skipped', reason: 'replied_recently', until: '2026-11-18T00:00:00Z' },
      { kind: 'stopped' },
    ])
    expect(read.actions[1]).toMatchObject({ capability: 'mail.send', needsConfirmation: true })
    expect(stepAt(read, 'l')?.type).toBe('label')
  })

  test('anything else is a plan that did not run', () => {
    expect(readPlan(null)).toEqual({
      ran: false,
      ignored: null,
      path: [],
      actions: [],
      stopped: false,
    })
    expect(readPlan({ ran: false, ignored: 'not_live' }).ignored).toBe('not_live')
  })
})

describe('highlightOf', () => {
  test('marks the trigger, each step and the ports between them', () => {
    const { nodes, edges } = highlightOf(readPlan(plan))
    expect([...nodes]).toEqual(['trigger', 'q', 'l', 'r', 's'])
    expect([...edges]).toEqual(['trigger:next', 'q:yes', 'l:next', 'r:next'])
  })

  test('a flow that did not run marks nothing', () => {
    const { nodes, edges } = highlightOf(readPlan({ ran: false, path: plan.path }))
    expect(nodes.size + edges.size).toBe(0)
  })
})

describe('actions and results', () => {
  test('a summary leaves out the type, message id and auto-submitted value', () => {
    const read = readPlan(plan)
    expect(actionSummary(read.actions[0]?.action ?? {})).toBe('Orders')
    expect(actionSummary(read.actions[1]?.action ?? {})).toBe('accounting@example.org')
  })

  test('results read with defaults, and pending ones are found', () => {
    const results = readResults([
      { node: 'l', type: 'label', result: 'done' },
      { node: 'f', type: 'forward', result: 'pending', detail: 'waits for a person to confirm' },
      'junk',
    ])
    expect(results).toHaveLength(2)
    expect(results[0]).toEqual({
      node: 'l',
      type: 'label',
      result: 'done',
      detail: null,
      outbox: null,
      draft: null,
    })
    expect(pendingActions(results)).toEqual([1])
    expect(readResults(null)).toEqual([])
  })
})
