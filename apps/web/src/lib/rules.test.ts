import { describe, expect, test } from 'bun:test'

import { ApiError, flowIssues } from '@holtmail/api-client'

import { ruleStrings } from '../strings-rules.ts'
import {
  addNode,
  changed,
  COLUMN,
  connect,
  defaultParams,
  disconnect,
  docsOf,
  fieldsOf,
  type FlowDefinition,
  inRunOrder,
  insertVariable,
  issuesByNode,
  layout,
  mayUseType,
  mayWriteRules,
  moveNode,
  newFlow,
  newNodeId,
  type NodeType,
  palette,
  paramSummary,
  readCatalogue,
  readFlow,
  readInteger,
  removeNode,
  setConfirm,
  setParams,
  setTrigger,
  splitList,
  toGraph,
  TRIGGER_ID,
  withParam,
} from './rules.ts'

const docs = (en: string, ru: string) => ({
  en: { title: en, summary: `${en}.` },
  ru: { title: ru, summary: `${ru}.` },
})
const noParams = { type: 'object', additionalProperties: false }
const message = ['message.archived', 'message.sending', 'invitation.received', 'manual']
const stored = ['message.archived', 'invitation.received', 'manual']

const raw = [
  {
    id: 'message.archived',
    version: 1,
    role: 'trigger',
    ports: ['next'],
    capability: null,
    triggers: message,
    templates: [],
    confirmable: false,
    docs: docs('Message archived', 'Письмо в архиве'),
    schema: noParams,
  },
  {
    id: 'message.sending',
    version: 1,
    role: 'trigger',
    ports: ['next'],
    capability: null,
    triggers: message,
    templates: [],
    confirmable: false,
    docs: docs('Message sending', 'Отправка письма'),
    schema: noParams,
  },
  {
    id: 'query',
    version: 1,
    role: 'condition',
    ports: ['yes', 'no'],
    capability: null,
    triggers: message,
    templates: [],
    confirmable: false,
    docs: docs('Search', 'Поиск'),
    schema: {
      type: 'object',
      properties: {
        query: { type: 'string', minLength: 1, maxLength: 2000, 'x-holtmail': 'query' },
      },
      required: ['query'],
    },
  },
  {
    id: 'header',
    version: 1,
    role: 'condition',
    ports: ['yes', 'no'],
    capability: null,
    triggers: message,
    templates: [],
    confirmable: false,
    docs: docs('Header', 'Заголовок'),
    schema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'Field name, such as `List-Id`.' },
        op: { description: 'The comparison.', $ref: '#/$defs/HeaderOp' },
        value: { type: 'string', default: '' },
      },
      required: ['name', 'op'],
      $defs: {
        HeaderOp: {
          oneOf: [
            { type: 'string', const: 'exists', description: 'The header is present.' },
            { type: 'string', const: 'contains', description: 'A value contains the text.' },
          ],
        },
      },
    },
  },
  {
    id: 'reply',
    version: 1,
    role: 'action',
    ports: ['next'],
    capability: 'mail.send',
    triggers: stored,
    templates: ['subject', 'body'],
    confirmable: true,
    docs: docs('Reply', 'Ответить'),
    schema: {
      type: 'object',
      properties: {
        body: { type: 'string', 'x-holtmail': 'template' },
        days: { type: 'integer', default: 7, minimum: 1, maximum: 365 },
        identity: { type: 'string', 'x-holtmail': 'identity' },
        subject: { type: 'string', default: 'Re: {{ subject }}', 'x-holtmail': 'template' },
      },
      required: ['identity', 'body'],
    },
  },
  {
    id: 'set_flags',
    version: 1,
    role: 'action',
    ports: ['next'],
    capability: 'mail.flags',
    triggers: stored,
    templates: [],
    confirmable: true,
    docs: docs('Set flags', 'Поставить отметки'),
    schema: {
      type: 'object',
      properties: {
        set: { type: 'array', default: [], items: { $ref: '#/$defs/Flag' } },
      },
      $defs: {
        Flag: {
          oneOf: [
            { type: 'string', const: 'seen' },
            { type: 'string', const: 'flagged' },
          ],
        },
      },
    },
  },
  {
    id: 'forward',
    version: 1,
    role: 'action',
    ports: ['next'],
    capability: 'mail.send',
    triggers: stored,
    templates: ['note'],
    confirmable: true,
    docs: docs('Forward', 'Переслать'),
    schema: {
      type: 'object',
      properties: {
        as_attachment: { type: 'boolean', default: false },
        note: { type: ['string', 'null'], 'x-holtmail': 'template' },
        to: { type: 'array', items: { type: 'string' }, minItems: 1, 'x-holtmail': 'addresses' },
      },
      required: ['to'],
    },
  },
  {
    id: 'set_identity',
    version: 1,
    role: 'action',
    ports: ['next'],
    capability: 'mail.send',
    triggers: ['message.sending'],
    templates: [],
    confirmable: true,
    docs: docs('Set identity', 'Выбрать личность'),
    schema: {
      type: 'object',
      properties: { identity: { type: 'string', 'x-holtmail': 'identity' } },
      required: ['identity'],
    },
  },
  {
    id: 'stop',
    version: 1,
    role: 'action',
    ports: [],
    capability: null,
    triggers: message,
    templates: [],
    confirmable: false,
    docs: docs('Stop', ''),
    schema: noParams,
  },
  { id: 'broken', role: 'nope' },
  'not an entry',
]

const catalogue = readCatalogue(raw)
const type = (id: string): NodeType => {
  const found = catalogue.find((t) => t.id === id)
  if (!found) throw new Error(id)
  return found
}

const shop: FlowDefinition = {
  format: 1,
  trigger: { type: 'message.archived', next: 'shop' },
  nodes: [
    {
      id: 'shop',
      type: 'query',
      params: { query: 'from:*@shop.example.com' },
      next: { yes: 'reply', no: 'stop' },
    },
    { id: 'reply', type: 'reply', params: { identity: 'auto', body: 'Thanks' }, confirm: true },
    { id: 'stop', type: 'stop' },
  ],
}

describe('readCatalogue', () => {
  test('keeps entries with an id and a known role', () => {
    expect(catalogue.map((t) => t.id)).toEqual([
      'message.archived',
      'message.sending',
      'query',
      'header',
      'reply',
      'set_flags',
      'forward',
      'set_identity',
      'stop',
    ])
    expect(type('reply').capability).toBe('mail.send')
    expect(type('reply').docs.ru.title).toBe('Ответить')
    expect(readCatalogue({})).toEqual([])
  })

  test('docs fall back to English when a language is missing', () => {
    expect(docsOf(type('stop'), 'ru').title).toBe('Stop')
    expect(docsOf(undefined, 'en', 'gone').title).toBe('gone')
  })
})

describe('readFlow', () => {
  test('reads the server format and drops what it does not know', () => {
    const flow = readFlow({
      format: 1,
      trigger: { type: 'message.archived', next: 'a', position: { x: 1, y: 2 } },
      nodes: [
        { id: 'a', type: 'label', params: { name: 'Orders' }, next: { next: 'b', bad: 3 } },
        { id: 7, type: 'label' },
        { id: 'b', type: 'stop', confirm: 'yes' },
      ],
    })
    expect(flow).toEqual({
      format: 1,
      trigger: { type: 'message.archived', next: 'a', position: { x: 1, y: 2 } },
      nodes: [
        { id: 'a', type: 'label', params: { name: 'Orders' }, next: { next: 'b' } },
        { id: 'b', type: 'stop' },
      ],
    })
    expect(readFlow({ nodes: [] })).toBeNull()
    expect(readFlow(null)).toBeNull()
  })
})

describe('fieldsOf', () => {
  test('required fields first, refs resolved, hints become kinds', () => {
    const header = fieldsOf(type('header').schema)
    expect(header.map((f) => [f.key, f.kind, f.required])).toEqual([
      ['name', 'text', true],
      ['op', 'choice', true],
      ['value', 'text', false],
    ])
    expect(header[1]?.options.map((o) => o.value)).toEqual(['exists', 'contains'])
    expect(header[1]?.description).toBe('The comparison.')

    const reply = fieldsOf(type('reply').schema)
    expect(reply.map((f) => [f.key, f.kind])).toEqual([
      ['body', 'template'],
      ['identity', 'identity'],
      ['days', 'integer'],
      ['subject', 'template'],
    ])
    expect(reply[2]).toMatchObject({ minimum: 1, maximum: 365, default: 7 })

    const forward = fieldsOf(type('forward').schema)
    expect(forward.map((f) => [f.key, f.kind, f.nullable])).toEqual([
      ['to', 'list', false],
      ['as_attachment', 'boolean', false],
      ['note', 'template', true],
    ])
    expect(fieldsOf(type('set_flags').schema)[0]).toMatchObject({
      kind: 'choices',
      options: [
        { value: 'seen', description: '' },
        { value: 'flagged', description: '' },
      ],
    })
    expect(fieldsOf(type('query').schema)[0]?.kind).toBe('query')
  })

  test('default parameters fill only what is required and has an obvious value', () => {
    expect(defaultParams(type('header'))).toEqual({ op: 'exists' })
    expect(defaultParams(type('reply'))).toEqual({ identity: 'auto' })
    expect(defaultParams(type('query'))).toEqual({})
    expect(defaultParams(type('stop'))).toEqual({})
  })
})

describe('palette', () => {
  test('only steps that may follow the trigger, conditions first, by title', () => {
    const groups = palette(catalogue, 'message.archived', '', 'en')
    expect(groups.map((g) => [g.role, g.types.map((t) => t.id)])).toEqual([
      ['condition', ['header', 'query']],
      ['action', ['forward', 'reply', 'set_flags', 'stop']],
    ])
    const sending = palette(catalogue, 'message.sending', '', 'en')
    expect(sending[1]?.types.map((t) => t.id)).toEqual(['set_identity', 'stop'])
  })

  test('search matches the title in the locale, the summary and the id', () => {
    expect(palette(catalogue, 'message.archived', 'отв', 'ru')[0]?.types.map((t) => t.id)).toEqual([
      'reply',
    ])
    expect(palette(catalogue, 'message.archived', 'set_fl', 'en')[0]?.types[0]?.id).toBe(
      'set_flags',
    )
    expect(palette(catalogue, 'message.archived', 'nothing here', 'en')).toEqual([])
  })

  test('steps that send need a sender grant', () => {
    expect(mayUseType(type('reply'), 'editor')).toBe(false)
    expect(mayUseType(type('reply'), 'sender')).toBe(true)
    expect(mayUseType(type('set_flags'), 'editor')).toBe(true)
  })
})

describe('editing', () => {
  test('new flows and node ids', () => {
    const flow = newFlow(type('message.archived'))
    expect(flow).toEqual({
      format: 1,
      trigger: { type: 'message.archived', params: {}, position: { x: 0, y: 0 } },
      nodes: [],
    })
    expect(newNodeId(shop, 'reply')).toBe('reply-2')
    expect(newNodeId(shop, 'header')).toBe('header')
    expect(newNodeId(shop, 'trigger')).toBe('trigger-2')
    expect(newNodeId(shop, 'message.archived')).toBe('message_archived')
  })

  test('add, connect, disconnect and remove keep links consistent', () => {
    const added = addNode(shop, type('header'), { x: 10, y: 20 })
    expect(added.id).toBe('header')
    expect(added.flow.nodes.at(-1)).toEqual({
      id: 'header',
      type: 'header',
      params: { op: 'exists' },
      position: { x: 10, y: 20 },
    })
    const linked = connect(added.flow, 'reply', 'next', 'header')
    expect(linked.nodes.find((n) => n.id === 'reply')?.next).toEqual({ next: 'header' })
    expect(connect(linked, 'shop', 'yes', 'header').nodes[0]?.next).toEqual({
      yes: 'header',
      no: 'stop',
    })
    expect(connect(linked, 'shop', 'yes', TRIGGER_ID)).toBe(linked)
    expect(connect(linked, 'shop', 'yes', 'shop')).toBe(linked)
    expect(connect(linked, 'shop', 'yes', 'missing')).toBe(linked)
    expect(connect(linked, TRIGGER_ID, 'next', 'header').trigger.next).toBe('header')

    const unlinked = disconnect(linked, 'reply', 'next')
    expect(unlinked.nodes.find((n) => n.id === 'reply')?.next).toBeUndefined()
    expect(disconnect(linked, TRIGGER_ID, 'next').trigger.next).toBeUndefined()

    const removed = removeNode(linked, 'stop')
    expect(removed.nodes.map((n) => n.id)).toEqual(['shop', 'reply', 'header'])
    expect(removed.nodes[0]?.next).toEqual({ yes: 'reply' })
    expect(removeNode(linked, 'shop').trigger.next).toBeUndefined()
    expect(removeNode(linked, TRIGGER_ID)).toBe(linked)
  })

  test('parameters, confirmation, position and trigger', () => {
    const params = setParams(shop, 'shop', withParam({ query: 'a' }, 'query', 'b'))
    expect(params.nodes[0]?.params).toEqual({ query: 'b' })
    expect(withParam({ a: 1, b: 2 }, 'a', undefined)).toEqual({ b: 2 })
    expect(setParams(shop, TRIGGER_ID, { x: 1 }).trigger.params).toEqual({ x: 1 })
    expect(setConfirm(shop, 'reply', false).nodes[1]).not.toHaveProperty('confirm')
    expect(setConfirm(shop, 'shop', true).nodes[0]?.confirm).toBe(true)
    expect(moveNode(shop, 'shop', { x: 10.4, y: 20.6 }).nodes[0]?.position).toEqual({
      x: 10,
      y: 21,
    })
    const sending = setTrigger(shop, type('message.sending'))
    expect(sending.trigger).toEqual({ type: 'message.sending', params: {}, next: 'shop' })
  })

  test('changed ignores key order', () => {
    const reordered: FlowDefinition = {
      nodes: shop.nodes,
      trigger: { next: 'shop', type: 'message.archived' },
      format: 1,
    }
    expect(changed(shop, reordered)).toBe(false)
    expect(changed(shop, setConfirm(shop, 'reply', false))).toBe(true)
  })
})

describe('graph', () => {
  test('lays out nodes without a position by depth from the trigger', () => {
    const placed = layout({
      ...shop,
      nodes: [...shop.nodes, { id: 'lonely', type: 'stop' }],
    })
    expect(placed.trigger.position).toEqual({ x: 0, y: 0 })
    expect(placed.nodes.map((n) => [n.id, n.position])).toEqual([
      ['shop', { x: COLUMN, y: 0 }],
      ['reply', { x: 2 * COLUMN, y: 0 }],
      ['stop', { x: 2 * COLUMN, y: 128 }],
      ['lonely', { x: 3 * COLUMN, y: 0 }],
    ])
    const kept = moveNode(placed, 'shop', { x: 5, y: 5 })
    expect(layout(kept).nodes[0]?.position).toEqual({ x: 5, y: 5 })
  })

  test('nodes and edges for the canvas', () => {
    const graph = toGraph(
      { ...shop, nodes: [...shop.nodes, { id: 'odd', type: 'gone' }] },
      catalogue,
    )
    expect(graph.nodes.map((n) => [n.id, n.role, n.ports, n.known, n.confirm])).toEqual([
      [TRIGGER_ID, 'trigger', ['next'], true, false],
      ['shop', 'condition', ['yes', 'no'], true, false],
      ['reply', 'action', ['next'], true, true],
      ['stop', 'action', [], true, false],
      ['odd', 'action', [], false, false],
    ])
    expect(graph.edges).toEqual([
      { id: 'trigger:next', source: TRIGGER_ID, port: 'next', target: 'shop' },
      { id: 'shop:yes', source: 'shop', port: 'yes', target: 'reply' },
      { id: 'shop:no', source: 'shop', port: 'no', target: 'stop' },
    ])
  })
})

describe('helpers', () => {
  test('lists, integers and summaries', () => {
    expect(splitList(' a@example.com, b@example.org;\nc@example.org ,, ')).toEqual([
      'a@example.com',
      'b@example.org',
      'c@example.org',
    ])
    expect(readInteger(' 12 ')).toBe(12)
    expect(readInteger('')).toBeUndefined()
    expect(readInteger('1.5')).toBeUndefined()
    expect(paramSummary({ identity: 'auto', query: 'from:a@example.com' })).toBe(
      'from:a@example.com',
    )
    expect(paramSummary({ to: ['a@example.com', 'b@example.org'] })).toBe(
      'a@example.com, b@example.org',
    )
    expect(paramSummary({ body: 'x'.repeat(100) }, 10)).toBe(`${'x'.repeat(9)}…`)
    expect(paramSummary({ days: 7 })).toBe('')
  })

  test('variables go in at the selection', () => {
    expect(insertVariable('Hi !', 3, 3, 'sender.name')).toEqual({
      value: 'Hi {{ sender.name }}!',
      caret: 20,
    })
    expect(insertVariable('Hi X', 3, 4, 'subject').value).toBe('Hi {{ subject }}')
  })

  test('issues by node and from a 422', () => {
    const issues = [
      { node: 'shop', code: 'bad_params', detail: 'query: empty' },
      { code: 'format', detail: 'newer' },
      { node: 'shop', code: 'unknown_port', detail: 'maybe' },
    ]
    const map = issuesByNode(issues)
    expect(map.get('shop')?.length).toBe(2)
    expect(map.get('')?.[0]?.code).toBe('format')
    const error = new ApiError(422, 'the flow is not valid', undefined, {
      error: 'the flow is not valid',
      issues,
    })
    expect(flowIssues(error)).toEqual(issues)
    expect(flowIssues(new ApiError(409, 'conflict'))).toBeNull()
    expect(flowIssues(new ApiError(422, 'x', undefined, { issues: [{ code: 1 }] }))).toBeNull()
  })

  test('who may write rules and the run order', () => {
    expect(mayWriteRules({ rights: { role: 'editor' } })).toBe(true)
    expect(mayWriteRules({ rights: { role: 'sender' } })).toBe(true)
    expect(mayWriteRules({ rights: { role: 'reader' } })).toBe(false)
    const order = inRunOrder([
      { id: 'b', position: 1, created_at: 1 },
      { id: 'c', position: 0, created_at: 5 },
      { id: 'a', position: 0, created_at: 2 },
    ])
    expect(order.map((f) => f.id)).toEqual(['a', 'c', 'b'])
  })
})

describe('strings', () => {
  test('English and Russian have the same keys', () => {
    const keys = (value: unknown, prefix = ''): string[] =>
      typeof value === 'object' && value !== null
        ? Object.entries(value).flatMap(([k, v]) => keys(v, `${prefix}${k}.`))
        : [prefix]
    expect(keys(ruleStrings.ru).toSorted()).toEqual(keys(ruleStrings.en).toSorted())
  })

  test('Russian counts', () => {
    expect(ruleStrings.ru.editor.issues(1)).toBe('1 проблема')
    expect(ruleStrings.ru.editor.issues(3)).toBe('3 проблемы')
    expect(ruleStrings.ru.editor.issues(11)).toBe('11 проблем')
    expect(ruleStrings.ru.query.matches(21)).toBe('Подходит 21 письмо архива')
    expect(ruleStrings.ru.query.matches(5)).toBe('Подходят 5 писем архива')
    expect(ruleStrings.en.query.matches(1)).toBe('1 message in the archive matches')
  })
})
