import type { Locale } from '../strings-writing.ts'

export type NodeRole = 'trigger' | 'condition' | 'action'

export interface NodeText {
  title: string
  summary: string
}

export interface JsonSchema {
  type?: string | string[]
  description?: string
  title?: string
  default?: unknown
  const?: unknown
  enum?: unknown[]
  oneOf?: JsonSchema[]
  anyOf?: JsonSchema[]
  items?: JsonSchema
  properties?: Record<string, JsonSchema>
  required?: string[]
  $ref?: string
  $defs?: Record<string, JsonSchema>
  minimum?: number
  maximum?: number
  minLength?: number
  maxLength?: number
  minItems?: number
  maxItems?: number
  'x-holtmail'?: string
}

/** A node type of the catalogue, as `GET /api/rules/catalogue` serves it. */
export interface NodeType {
  id: string
  version: number
  role: NodeRole
  ports: string[]
  capability: string | null
  triggers: string[]
  templates: string[]
  confirmable: boolean
  docs: Record<Locale, NodeText>
  schema: JsonSchema
}

export interface Point {
  x: number
  y: number
}

interface TriggerNode {
  type: string
  version?: number
  params?: Record<string, unknown>
  next?: string
  position?: Point
}

interface FlowNode {
  id: string
  type: string
  version?: number
  params?: Record<string, unknown>
  next?: Record<string, string>
  confirm?: boolean
  position?: Point
}

/** A flow in the `holtmail-rules` JSON format. */
export interface FlowDefinition {
  format: number
  trigger: TriggerNode
  nodes: FlowNode[]
}

/** The id the trigger has on the canvas and in the validator's issues. */
export const TRIGGER_ID = 'trigger'

const FLOW_FORMAT = 1

/** Canvas units between columns and rows when nodes without a position are laid out. */
export const COLUMN = 288
export const ROW = 128
/** The canvas grid nodes snap to. */
export const GRID = 16

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === 'string') : []

function readText(value: unknown): NodeText {
  const text = isRecord(value) ? value : {}
  return {
    title: typeof text.title === 'string' ? text.title : '',
    summary: typeof text.summary === 'string' ? text.summary : '',
  }
}

function readSchema(value: unknown): JsonSchema {
  if (!isRecord(value)) return {}
  const out: JsonSchema = {}
  if (typeof value.type === 'string') out.type = value.type
  else if (Array.isArray(value.type)) out.type = strings(value.type)
  for (const key of ['description', 'title', '$ref', 'x-holtmail'] as const) {
    const v = value[key]
    if (typeof v === 'string') out[key] = v
  }
  for (const key of [
    'minimum',
    'maximum',
    'minLength',
    'maxLength',
    'minItems',
    'maxItems',
  ] as const) {
    const v = value[key]
    if (typeof v === 'number') out[key] = v
  }
  if ('default' in value) out.default = value.default
  if ('const' in value) out.const = value.const
  if (Array.isArray(value.enum)) out.enum = value.enum
  if (Array.isArray(value.oneOf)) out.oneOf = value.oneOf.map(readSchema)
  if (Array.isArray(value.anyOf)) out.anyOf = value.anyOf.map(readSchema)
  if (value.items !== undefined) out.items = readSchema(value.items)
  if (Array.isArray(value.required)) out.required = strings(value.required)
  for (const key of ['properties', '$defs'] as const) {
    const map = value[key]
    if (isRecord(map)) {
      out[key] = Object.fromEntries(Object.entries(map).map(([k, v]) => [k, readSchema(v)]))
    }
  }
  return out
}

const roles: readonly NodeRole[] = ['trigger', 'condition', 'action']

/** Reads the catalogue the server sent; entries without an id or a known role are dropped. */
export function readCatalogue(value: unknown): NodeType[] {
  if (!Array.isArray(value)) return []
  const out: NodeType[] = []
  for (const entry of value) {
    if (!isRecord(entry) || typeof entry.id !== 'string') continue
    const role = roles.find((r) => r === entry.role)
    if (!role) continue
    const docs = isRecord(entry.docs) ? entry.docs : {}
    out.push({
      id: entry.id,
      version: typeof entry.version === 'number' ? entry.version : 1,
      role,
      ports: strings(entry.ports),
      capability: typeof entry.capability === 'string' ? entry.capability : null,
      triggers: strings(entry.triggers),
      templates: strings(entry.templates),
      confirmable: entry.confirmable === true,
      docs: { en: readText(docs.en), ru: readText(docs.ru) },
      schema: readSchema(entry.schema),
    })
  }
  return out
}

function readPoint(value: unknown): Point | undefined {
  if (!isRecord(value) || typeof value.x !== 'number' || typeof value.y !== 'number') {
    return undefined
  }
  return { x: value.x, y: value.y }
}

function readNext(value: unknown): Record<string, string> {
  if (!isRecord(value)) return {}
  return Object.fromEntries(
    Object.entries(value).filter(
      (entry): entry is [string, string] => typeof entry[1] === 'string',
    ),
  )
}

/** Reads a flow's JSON; `null` when it has no trigger. Unknown fields are left out. */
export function readFlow(value: unknown): FlowDefinition | null {
  if (!isRecord(value) || !isRecord(value.trigger)) return null
  const t = value.trigger
  if (typeof t.type !== 'string') return null
  const trigger: TriggerNode = { type: t.type }
  if (typeof t.version === 'number') trigger.version = t.version
  if (isRecord(t.params)) trigger.params = t.params
  if (typeof t.next === 'string') trigger.next = t.next
  const tp = readPoint(t.position)
  if (tp) trigger.position = tp
  const nodes: FlowNode[] = []
  for (const raw of Array.isArray(value.nodes) ? value.nodes : []) {
    if (!isRecord(raw) || typeof raw.id !== 'string' || typeof raw.type !== 'string') continue
    const node: FlowNode = { id: raw.id, type: raw.type }
    if (typeof raw.version === 'number') node.version = raw.version
    if (isRecord(raw.params)) node.params = raw.params
    const next = readNext(raw.next)
    if (Object.keys(next).length > 0) node.next = next
    if (raw.confirm === true) node.confirm = true
    const p = readPoint(raw.position)
    if (p) node.position = p
    nodes.push(node)
  }
  return { format: typeof value.format === 'number' ? value.format : FLOW_FORMAT, trigger, nodes }
}

export const nodeTypeOf = (catalogue: readonly NodeType[], id: string) =>
  catalogue.find((t) => t.id === id)

/** A node type's title and summary in the locale, in English when that text is missing. */
export function docsOf(type: NodeType | undefined, locale: Locale, fallbackTitle = ''): NodeText {
  if (!type) return { title: fallbackTitle, summary: '' }
  const text = type.docs[locale]
  return {
    title: text.title || type.docs.en.title || type.id,
    summary: text.summary || type.docs.en.summary,
  }
}

type FieldKind =
  | 'text'
  | 'template'
  | 'query'
  | 'integer'
  | 'boolean'
  | 'choice'
  | 'choices'
  | 'list'
  | 'time'
  | 'cron'
  | 'timezone'
  | 'identity'
  | 'transport'
  | 'folder'
  | 'calendar'
  | 'address_book'
  | 'webhook'
  | 'address'

interface FieldOption {
  value: string
  description: string
}

/** One input of the inspector, read from a node type's JSON Schema. */
export interface Field {
  key: string
  kind: FieldKind
  required: boolean
  nullable: boolean
  description: string
  default?: unknown
  minimum?: number
  maximum?: number
  maxLength?: number
  minItems?: number
  maxItems?: number
  options: FieldOption[]
}

function resolve(schema: JsonSchema, root: JsonSchema): JsonSchema {
  const ref = schema.$ref
  if (!ref?.startsWith('#/$defs/')) return schema
  const target = root.$defs?.[ref.slice('#/$defs/'.length)]
  if (!target) return schema
  const merged: JsonSchema = { ...target }
  for (const [key, value] of Object.entries(schema)) {
    if (key !== '$ref') Object.assign(merged, { [key]: value })
  }
  return merged
}

function optionsOf(schema: JsonSchema): FieldOption[] {
  if (schema.oneOf?.every((s) => typeof s.const === 'string')) {
    return schema.oneOf.map((s) => ({ value: String(s.const), description: s.description ?? '' }))
  }
  const values = (schema.enum ?? []).filter((v): v is string => typeof v === 'string')
  if (values.length > 0 && values.length === schema.enum?.length) {
    return values.map((value) => ({ value, description: '' }))
  }
  return []
}

const hinted = new Set<string>([
  'template',
  'query',
  'time',
  'cron',
  'timezone',
  'identity',
  'transport',
  'folder',
  'calendar',
  'address_book',
  'webhook',
  'address',
])

function isHint(value: string): value is FieldKind {
  return hinted.has(value)
}

function typesOf(schema: JsonSchema): string[] {
  if (Array.isArray(schema.type)) return schema.type
  return schema.type ? [schema.type] : []
}

/** The inspector's fields for a schema, in a stable order: required first, then by name. Every
 * property becomes a field; a shape the inspector cannot show becomes plain text. */
export function fieldsOf(schema: JsonSchema): Field[] {
  const required = new Set(schema.required ?? [])
  const entries = Object.entries(schema.properties ?? {}).toSorted(
    ([a], [b]) => Number(required.has(b)) - Number(required.has(a)) || a.localeCompare(b),
  )
  return entries.map(([key, raw]) => {
    const s = resolve(raw, schema)
    const types = typesOf(s)
    const nullable = types.includes('null')
    const hint = s['x-holtmail']
    const options = optionsOf(s)
    let kind: FieldKind = 'text'
    let fieldOptions = options
    if (types.includes('array')) {
      const items = resolve(s.items ?? {}, schema)
      const itemOptions = optionsOf(items)
      kind = itemOptions.length > 0 ? 'choices' : 'list'
      fieldOptions = itemOptions
    } else if (hint === 'addresses') {
      kind = 'list'
    } else if (hint && isHint(hint)) {
      kind = hint
    } else if (options.length > 0) {
      kind = 'choice'
    } else if (types.includes('integer') || types.includes('number')) {
      kind = 'integer'
    } else if (types.includes('boolean')) {
      kind = 'boolean'
    }
    const field: Field = {
      key,
      kind,
      required: required.has(key),
      nullable,
      description: s.description ?? '',
      options: fieldOptions,
    }
    if (s.default !== undefined) field.default = s.default
    if (s.minimum !== undefined) field.minimum = s.minimum
    if (s.maximum !== undefined) field.maximum = s.maximum
    if (s.maxLength !== undefined) field.maxLength = s.maxLength
    if (s.minItems !== undefined) field.minItems = s.minItems
    if (s.maxItems !== undefined) field.maxItems = s.maxItems
    return field
  })
}

/** Parameters for a new node: each required field's default, the first choice of a required
 * choice, `auto` for a required identity, and nothing else, so the validator names what is
 * still missing. */
export function defaultParams(type: NodeType): Record<string, unknown> {
  const params: Record<string, unknown> = {}
  for (const field of fieldsOf(type.schema)) {
    if (!field.required) continue
    if (field.default !== undefined) params[field.key] = field.default
    else if (field.kind === 'choice' && field.options[0]) params[field.key] = field.options[0].value
    else if (field.kind === 'identity') params[field.key] = 'auto'
    else if (field.kind === 'choices' && field.minItems && field.options[0]) {
      params[field.key] = [field.options[0].value]
    }
  }
  return params
}

/** Sets one parameter; `undefined` removes it, so the server's default applies. */
export function withParam(
  params: Record<string, unknown> | undefined,
  key: string,
  value: unknown,
): Record<string, unknown> {
  const next = { ...params }
  if (value === undefined) delete next[key]
  else next[key] = value
  return next
}

/** Splits a list typed as text: commas, semicolons and new lines separate entries. */
export const splitList = (text: string): string[] =>
  text
    .split(/[,;\n]/)
    .map((s) => s.trim())
    .filter(Boolean)

/** The value an integer field keeps for typed text: `undefined` for empty or not a number. */
export function readInteger(text: string): number | undefined {
  const t = text.trim()
  if (!/^-?\d+$/.test(t)) return undefined
  return Number(t)
}

export interface PaletteGroup {
  role: NodeRole
  types: NodeType[]
}

/** The node types that may follow `trigger`, matching `search` in the locale's title and summary
 * or the id, grouped as conditions then actions, each by title. */
export function palette(
  catalogue: readonly NodeType[],
  trigger: string,
  search: string,
  locale: Locale,
): PaletteGroup[] {
  const words = search.toLocaleLowerCase().split(/\s+/).filter(Boolean)
  const fits = (type: NodeType) => {
    if (type.role === 'trigger' || !type.triggers.includes(trigger)) return false
    const text = docsOf(type, locale)
    const hay = `${type.id} ${text.title} ${text.summary}`.toLocaleLowerCase()
    return words.every((w) => hay.includes(w))
  }
  const groups: PaletteGroup[] = []
  for (const role of ['condition', 'action'] as const) {
    const types = catalogue
      .filter((t) => t.role === role && fits(t))
      .toSorted((a, b) => docsOf(a, locale).title.localeCompare(docsOf(b, locale).title, locale))
    if (types.length > 0) groups.push({ role, types })
  }
  return groups
}

export const triggers = (catalogue: readonly NodeType[]) =>
  catalogue.filter((t) => t.role === 'trigger')

/** A new flow with only its trigger. */
export function newFlow(type: NodeType): FlowDefinition {
  return {
    format: FLOW_FORMAT,
    trigger: { type: type.id, params: defaultParams(type), position: { x: 0, y: 0 } },
    nodes: [],
  }
}

/** An id for a new node of `type`: the type itself, or with `-2`, `-3`, ... when taken. */
export function newNodeId(flow: FlowDefinition, type: string): string {
  const taken = new Set([TRIGGER_ID, ...flow.nodes.map((n) => n.id)])
  const base = type.replace(/[^A-Za-z0-9_-]/g, '_').slice(0, 60) || 'node'
  if (!taken.has(base)) return base
  for (let i = 2; ; i += 1) {
    const id = `${base}-${i}`
    if (!taken.has(id)) return id
  }
}

export function addNode(
  flow: FlowDefinition,
  type: NodeType,
  position: Point,
): { flow: FlowDefinition; id: string } {
  const id = newNodeId(flow, type.id)
  const node: FlowNode = { id, type: type.id, params: defaultParams(type), position }
  if (type.version !== 1) node.version = type.version
  return { flow: { ...flow, nodes: [...flow.nodes, node] }, id }
}

/** Replaces the trigger type; the start node stays, the parameters start over. */
export function setTrigger(flow: FlowDefinition, type: NodeType): FlowDefinition {
  const trigger: TriggerNode = { type: type.id, params: defaultParams(type) }
  if (flow.trigger.next) trigger.next = flow.trigger.next
  if (flow.trigger.position) trigger.position = flow.trigger.position
  return { ...flow, trigger }
}

function updateNode(
  flow: FlowDefinition,
  id: string,
  change: (node: FlowNode) => FlowNode,
): FlowDefinition {
  return { ...flow, nodes: flow.nodes.map((n) => (n.id === id ? change(n) : n)) }
}

export function setParams(
  flow: FlowDefinition,
  id: string,
  params: Record<string, unknown>,
): FlowDefinition {
  if (id === TRIGGER_ID) return { ...flow, trigger: { ...flow.trigger, params } }
  return updateNode(flow, id, (n) => ({ ...n, params }))
}

export function paramsOf(flow: FlowDefinition, id: string): Record<string, unknown> {
  if (id === TRIGGER_ID) return flow.trigger.params ?? {}
  return flow.nodes.find((n) => n.id === id)?.params ?? {}
}

export function setConfirm(flow: FlowDefinition, id: string, confirm: boolean): FlowDefinition {
  return updateNode(flow, id, (n) => {
    const next = { ...n }
    if (confirm) next.confirm = true
    else delete next.confirm
    return next
  })
}

export function moveNode(flow: FlowDefinition, id: string, position: Point): FlowDefinition {
  const p = { x: Math.round(position.x), y: Math.round(position.y) }
  if (id === TRIGGER_ID) return { ...flow, trigger: { ...flow.trigger, position: p } }
  return updateNode(flow, id, (n) => ({ ...n, position: p }))
}

/** Points `port` of `source` at `target`; a port has one target, so an earlier one is
 * replaced. A link to itself or to the trigger changes nothing. */
export function connect(
  flow: FlowDefinition,
  source: string,
  port: string,
  target: string,
): FlowDefinition {
  if (target === TRIGGER_ID || source === target) return flow
  if (!flow.nodes.some((n) => n.id === target)) return flow
  if (source === TRIGGER_ID) return { ...flow, trigger: { ...flow.trigger, next: target } }
  return updateNode(flow, source, (n) => ({ ...n, next: { ...n.next, [port]: target } }))
}

export function disconnect(flow: FlowDefinition, source: string, port: string): FlowDefinition {
  if (source === TRIGGER_ID) {
    const trigger = { ...flow.trigger }
    delete trigger.next
    return { ...flow, trigger }
  }
  return updateNode(flow, source, (n) => {
    const next = { ...n.next }
    delete next[port]
    return withNext(n, next)
  })
}

function withNext(node: FlowNode, next: Record<string, string>): FlowNode {
  const out: FlowNode = { ...node }
  if (Object.keys(next).length > 0) out.next = next
  else delete out.next
  return out
}

/** Removes a node and every link to it. The trigger cannot be removed. */
export function removeNode(flow: FlowDefinition, id: string): FlowDefinition {
  if (id === TRIGGER_ID) return flow
  const trigger = { ...flow.trigger }
  if (trigger.next === id) delete trigger.next
  const nodes = flow.nodes
    .filter((n) => n.id !== id)
    .map((n) => {
      if (!n.next || !Object.values(n.next).includes(id)) return n
      return withNext(
        n,
        Object.fromEntries(Object.entries(n.next).filter(([, target]) => target !== id)),
      )
    })
  return { ...flow, trigger, nodes }
}

/** Positions for nodes without one: columns by distance from the trigger along the links, rows
 * in flow order; nodes no link reaches go in a column after the rest. Positions that exist are
 * kept. */
export function layout(flow: FlowDefinition): FlowDefinition {
  if (flow.trigger.position && flow.nodes.every((n) => n.position)) return flow
  const depth = new Map<string, number>([[TRIGGER_ID, 0]])
  const queue: string[] = []
  if (flow.trigger.next) {
    depth.set(flow.trigger.next, 1)
    queue.push(flow.trigger.next)
  }
  const byId = new Map(flow.nodes.map((n) => [n.id, n]))
  while (queue.length > 0) {
    const id = queue.shift() ?? ''
    const d = depth.get(id) ?? 0
    for (const target of Object.values(byId.get(id)?.next ?? {})) {
      if (!depth.has(target) && byId.has(target)) {
        depth.set(target, d + 1)
        queue.push(target)
      }
    }
  }
  const last = Math.max(0, ...depth.values()) + 1
  const rows = new Map<number, number>()
  const place = (column: number): Point => {
    const row = rows.get(column) ?? 0
    rows.set(column, row + 1)
    return { x: column * COLUMN, y: row * ROW }
  }
  const trigger = { ...flow.trigger, position: flow.trigger.position ?? place(0) }
  const nodes = flow.nodes.map((n) =>
    n.position ? n : { ...n, position: place(depth.get(n.id) ?? last) },
  )
  return { ...flow, trigger, nodes }
}

export interface GraphNode {
  id: string
  type: string
  position: Point
  ports: string[]
  role: NodeRole
  known: boolean
  confirm: boolean
}

export interface GraphEdge {
  id: string
  source: string
  port: string
  target: string
}

/** The canvas's nodes and edges: the trigger first, then each node; one edge per linked port of
 * a known port and target. */
export function toGraph(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const placed = layout(flow)
  const triggerType = nodeTypeOf(catalogue, placed.trigger.type)
  const nodes: GraphNode[] = [
    {
      id: TRIGGER_ID,
      type: placed.trigger.type,
      position: placed.trigger.position ?? { x: 0, y: 0 },
      ports: ['next'],
      role: 'trigger',
      known: triggerType?.role === 'trigger',
      confirm: false,
    },
  ]
  const ids = new Set(placed.nodes.map((n) => n.id))
  const edges: GraphEdge[] = []
  if (placed.trigger.next && ids.has(placed.trigger.next)) {
    edges.push({
      id: `${TRIGGER_ID}:next`,
      source: TRIGGER_ID,
      port: 'next',
      target: placed.trigger.next,
    })
  }
  for (const node of placed.nodes) {
    const type = nodeTypeOf(catalogue, node.type)
    const ports = type?.ports ?? Object.keys(node.next ?? {})
    nodes.push({
      id: node.id,
      type: node.type,
      position: node.position ?? { x: 0, y: 0 },
      ports,
      role: type?.role ?? 'action',
      known: type !== undefined && type.role !== 'trigger',
      confirm: node.confirm === true,
    })
    for (const port of ports) {
      const target = node.next?.[port]
      if (target && ids.has(target)) {
        edges.push({ id: `${node.id}:${port}`, source: node.id, port, target })
      }
    }
  }
  return { nodes, edges }
}

/** Issues keyed by node id (`trigger` for the trigger, `''` for the whole flow). */
export function issuesByNode<T extends { node?: string | null }>(
  issues: readonly T[],
): Map<string, T[]> {
  const out = new Map<string, T[]>()
  for (const issue of issues) {
    const key = issue.node ?? ''
    out.set(key, [...(out.get(key) ?? []), issue])
  }
  return out
}

/** A one-line summary of a node's parameters for its box on the canvas: the query, the label,
 * recipients or the first filled text field, cut to `max` characters. */
export function paramSummary(params: Record<string, unknown> | undefined, max = 48): string {
  if (!params) return ''
  const order = ['query', 'name', 'folder', 'to', 'address', 'cron', 'answer', 'reason', 'title']
  const keys = [...order.filter((k) => k in params), ...Object.keys(params).toSorted()]
  for (const key of keys) {
    const value = params[key]
    const text = Array.isArray(value)
      ? value.filter((v) => typeof v === 'string').join(', ')
      : typeof value === 'string'
        ? value
        : ''
    if (text.trim()) {
      const line = text.replace(/\s+/g, ' ').trim()
      return line.length > max ? `${line.slice(0, max - 1)}…` : line
    }
  }
  return ''
}

/** The variables a template sees, for the template field's help; `insights.<kind>` stands for
 * every insight field. */
export const templateVariables = [
  'sender.name',
  'sender.address',
  'subject',
  'date',
  'alias',
  'insights',
  'invitation.summary',
  'invitation.starts',
  'invitation.location',
  'invitation.organizer.address',
  'event.summary',
  'event.starts',
  'event.location',
  'now',
] as const

/** Inserts `{{ variable }}` at the selection, as the template field's variable buttons do. */
export function insertVariable(
  value: string,
  selectionStart: number,
  selectionEnd: number,
  variable: string,
): { value: string; caret: number } {
  const start = Math.max(0, Math.min(selectionStart, value.length))
  const end = Math.max(start, Math.min(selectionEnd, value.length))
  const token = `{{ ${variable} }}`
  return {
    value: `${value.slice(0, start)}${token}${value.slice(end)}`,
    caret: start + token.length,
  }
}

/** Whether the editor's flow differs from the saved one, ignoring key order. */
export function changed(a: FlowDefinition | null, b: FlowDefinition | null): boolean {
  return canonical(a) !== canonical(b)
}

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (isRecord(value)) {
    return Object.fromEntries(
      Object.keys(value)
        .toSorted()
        .map((k) => [k, sortKeys(value[k])]),
    )
  }
  return value
}

const canonical = (flow: FlowDefinition | null) => JSON.stringify(sortKeys(flow))

/** Whether the user's grant on an archive lets them write rules: `editor` or `sender`. The
 * server also checks the `rules` switch, so a closed one still answers `403`. */
/** Whether a grant of `role` may save a flow with this step: steps that send mail need a
 * `sender` grant, as the server checks on save. */
export const mayUseType = (type: NodeType, role: string) =>
  type.capability !== 'mail.send' || role === 'sender'

export const mayWriteRules = (archive: { rights: { role: string } }) =>
  archive.rights.role === 'editor' || archive.rights.role === 'sender'

/** Flows in the order they run: by position, then by creation. */
export function inRunOrder<T extends { position: number; created_at: number }>(
  flows: readonly T[],
): T[] {
  return flows.toSorted((a, b) => a.position - b.position || a.created_at - b.created_at)
}
