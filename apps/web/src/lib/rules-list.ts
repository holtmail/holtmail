import {
  COLUMN,
  type FlowDefinition,
  type NodeType,
  nodeTypeOf,
  type Point,
  TRIGGER_ID,
} from './rules.ts'

/** The port a step continues through in a linear flow: `yes` for a condition (its `no` ends the
 * flow), `next` for an action, none for a step without ports (`stop`). */
function forward(type: NodeType | undefined): string | null | undefined {
  if (!type || type.role === 'trigger') return undefined
  if (type.ports.includes('yes')) return 'yes'
  if (type.ports.includes('next')) return 'next'
  return type.ports.length === 0 ? null : undefined
}

/**
 * The steps of a linear flow in order from the trigger ("when … if … then …"), or `null` when it
 * branches: a condition's `no` is linked, a step links from a port other than its forward one,
 * a step is reached twice or not at all, or a step type is unknown.
 */
export function chainOf(flow: FlowDefinition, catalogue: readonly NodeType[]): string[] | null {
  const byId = new Map(flow.nodes.map((n) => [n.id, n]))
  const chain: string[] = []
  let current = flow.trigger.next
  while (current !== undefined) {
    const node = byId.get(current)
    if (!node || chain.includes(current)) return null
    chain.push(current)
    const port = forward(nodeTypeOf(catalogue, node.type))
    if (port === undefined) return null
    const links = Object.entries(node.next ?? {})
    if (links.some(([p]) => p !== port)) return null
    current = port ? node.next?.[port] : undefined
  }
  return chain.length === flow.nodes.length ? chain : null
}

const rowPosition = (index: number): Point => ({ x: index * COLUMN, y: 0 })

/** Links the steps in this order from the trigger and lays them out in one row; steps not
 * named are removed. A step without ports can only be last, so one elsewhere ends the chain
 * there and the rest is removed. */
export function withChain(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  ids: readonly string[],
): FlowDefinition {
  const byId = new Map(flow.nodes.map((n) => [n.id, n]))
  const kept: string[] = []
  for (const id of ids) {
    if (!byId.has(id) || kept.includes(id)) continue
    kept.push(id)
    if (forward(nodeTypeOf(catalogue, byId.get(id)?.type ?? '')) === null) break
  }
  const nodes = kept.map((id, index) => {
    const { next: _next, ...node } = byId.get(id) ?? { id, type: '' }
    const port = forward(nodeTypeOf(catalogue, node.type))
    const following = kept[index + 1]
    return {
      ...node,
      ...(port && following ? { next: { [port]: following } } : {}),
      position: rowPosition(index + 1),
    }
  })
  const { next: _next, ...trigger } = flow.trigger
  return {
    ...flow,
    trigger: { ...trigger, ...(kept[0] ? { next: kept[0] } : {}), position: rowPosition(0) },
    nodes,
  }
}

/** Whether the step at `index` may move by `delta` (−1 or 1): it stays in the chain and a step
 * without ports stays last. */
export function canMove(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  chain: readonly string[],
  index: number,
  delta: number,
): boolean {
  const target = index + delta
  if (target < 0 || target >= chain.length) return false
  const portless = (id: string | undefined) =>
    forward(nodeTypeOf(catalogue, flow.nodes.find((n) => n.id === id)?.type ?? '')) === null
  const last = chain.length - 1
  if (portless(chain[index]) && index === last) return false
  if (portless(chain[target]) && target === last) return false
  return true
}

/** Moves the step at `index` by `delta` and links the chain again. */
export function moveStep(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  chain: readonly string[],
  index: number,
  delta: number,
): FlowDefinition {
  if (!canMove(flow, catalogue, chain, index, delta)) return flow
  const next = [...chain]
  const [id] = next.splice(index, 1)
  if (id === undefined) return flow
  next.splice(index + delta, 0, id)
  return withChain(flow, catalogue, next)
}

/** Removes a step and closes the gap. */
export function removeStep(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  chain: readonly string[],
  id: string,
): FlowDefinition {
  return withChain(
    { ...flow, nodes: flow.nodes.filter((n) => n.id !== id) },
    catalogue,
    chain.filter((c) => c !== id),
  )
}

/** Puts a step that was just added (the last of `flow.nodes`) at the end of the chain, or
 * before a closing step without ports. */
export function appendStep(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  chain: readonly string[],
  id: string,
): FlowDefinition {
  const last = chain.at(-1)
  const closing =
    last !== undefined &&
    forward(nodeTypeOf(catalogue, flow.nodes.find((n) => n.id === last)?.type ?? '')) === null
  const order = closing ? [...chain.slice(0, -1), id, last] : [...chain, id]
  return withChain(flow, catalogue, order)
}

/** The word a step starts with in the list: the trigger is "when", the first condition "if" and
 * the next ones "and", the first action "do" (shown as "then") and the next ones "and". */
export function chainWords(
  flow: FlowDefinition,
  catalogue: readonly NodeType[],
  chain: readonly string[],
): Map<string, 'when' | 'if' | 'do' | 'and'> {
  const words = new Map<string, 'when' | 'if' | 'do' | 'and'>([[TRIGGER_ID, 'when']])
  let previous: string | null = null
  for (const id of chain) {
    const role = nodeTypeOf(catalogue, flow.nodes.find((n) => n.id === id)?.type ?? '')?.role
    const kind = role === 'condition' ? 'if' : 'do'
    words.set(id, previous === kind ? 'and' : kind)
    previous = kind
  }
  return words
}
