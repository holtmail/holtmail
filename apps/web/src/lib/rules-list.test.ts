import { describe, expect, test } from 'bun:test'

import {
  appendStep,
  canMove,
  chainOf,
  chainWords,
  moveStep,
  removeStep,
  withChain,
} from './rules-list.ts'
import { type FlowDefinition, type NodeType, readCatalogue } from './rules.ts'

const entry = (id: string, role: string, ports: string[]) => ({
  id,
  version: 1,
  role,
  ports,
  capability: null,
  triggers: ['message.archived'],
  templates: [],
  confirmable: false,
  docs: { en: { title: id, summary: '' }, ru: { title: id, summary: '' } },
  schema: {},
})

const catalogue: NodeType[] = readCatalogue([
  entry('message.archived', 'trigger', ['next']),
  entry('query', 'condition', ['yes', 'no']),
  entry('label', 'action', ['next']),
  entry('stop', 'action', []),
])

const linear: FlowDefinition = {
  format: 1,
  trigger: { type: 'message.archived', next: 'q' },
  nodes: [
    { id: 'q', type: 'query', params: { query: 'from:shop@example.com' }, next: { yes: 'l' } },
    { id: 'l', type: 'label', params: { name: 'Orders' }, next: { next: 's' } },
    { id: 's', type: 'stop' },
  ],
}

describe('chainOf', () => {
  test('follows yes and next from the trigger', () => {
    expect(chainOf(linear, catalogue)).toEqual(['q', 'l', 's'])
  })

  test('a flow with only its trigger is linear', () => {
    expect(
      chainOf({ format: 1, trigger: { type: 'message.archived' }, nodes: [] }, catalogue),
    ).toEqual([])
  })

  test('a linked no port branches', () => {
    const flow = {
      ...linear,
      nodes: linear.nodes.map((n) => (n.id === 'q' ? { ...n, next: { yes: 'l', no: 's' } } : n)),
    }
    expect(chainOf(flow, catalogue)).toBeNull()
  })

  test('a step no link reaches, a cycle or an unknown type is not linear', () => {
    expect(
      chainOf({ ...linear, nodes: [...linear.nodes, { id: 'x', type: 'label' }] }, catalogue),
    ).toBeNull()
    const cycle: FlowDefinition = {
      ...linear,
      nodes: [
        { id: 'q', type: 'query', next: { yes: 'l' } },
        { id: 'l', type: 'label', next: { next: 'q' } },
      ],
    }
    expect(chainOf(cycle, catalogue)).toBeNull()
    const unknown = {
      ...linear,
      trigger: { type: 'message.archived', next: 'u' },
      nodes: [{ id: 'u', type: 'nope' }],
    }
    expect(chainOf(unknown, catalogue)).toBeNull()
  })
})

describe('list editing', () => {
  test('withChain links in order and lays the steps out in one row', () => {
    const flow = withChain(linear, catalogue, ['l', 'q', 's'])
    expect(flow.trigger.next).toBe('l')
    expect(flow.nodes.map((n) => [n.id, n.next])).toEqual([
      ['l', { next: 'q' }],
      ['q', { yes: 's' }],
      ['s', undefined],
    ])
    expect(new Set(flow.nodes.map((n) => n.position?.y))).toEqual(new Set([0]))
    expect(chainOf(flow, catalogue)).toEqual(['l', 'q', 's'])
  })

  test('a closing stop stays last', () => {
    const chain = ['q', 'l', 's']
    expect(canMove(linear, catalogue, chain, 2, -1)).toBe(false)
    expect(canMove(linear, catalogue, chain, 1, 1)).toBe(false)
    expect(canMove(linear, catalogue, chain, 0, 1)).toBe(true)
    expect(canMove(linear, catalogue, chain, 0, -1)).toBe(false)
    expect(chainOf(moveStep(linear, catalogue, chain, 0, 1), catalogue)).toEqual(['l', 'q', 's'])
    expect(moveStep(linear, catalogue, chain, 2, -1)).toBe(linear)
  })

  test('removing a step closes the gap', () => {
    const flow = removeStep(linear, catalogue, ['q', 'l', 's'], 'l')
    expect(chainOf(flow, catalogue)).toEqual(['q', 's'])
    expect(flow.nodes.find((n) => n.id === 'q')?.next).toEqual({ yes: 's' })
  })

  test('a new step goes before a closing stop, else at the end', () => {
    const added = { ...linear, nodes: [...linear.nodes, { id: 'l2', type: 'label' }] }
    expect(chainOf(appendStep(added, catalogue, ['q', 'l', 's'], 'l2'), catalogue)).toEqual([
      'q',
      'l',
      'l2',
      's',
    ])
    const open = withChain(linear, catalogue, ['q', 'l'])
    const more = { ...open, nodes: [...open.nodes, { id: 'l2', type: 'label' }] }
    expect(chainOf(appendStep(more, catalogue, ['q', 'l'], 'l2'), catalogue)).toEqual([
      'q',
      'l',
      'l2',
    ])
  })

  test('chainWords reads when, if, then and and', () => {
    const flow = withChain(
      { ...linear, nodes: [...linear.nodes, { id: 'q2', type: 'query' }] },
      catalogue,
      ['q', 'q2', 'l', 's'],
    )
    expect([...chainWords(flow, catalogue, ['q', 'q2', 'l', 's'])]).toEqual([
      ['trigger', 'when'],
      ['q', 'if'],
      ['q2', 'and'],
      ['l', 'do'],
      ['s', 'and'],
    ])
  })
})
