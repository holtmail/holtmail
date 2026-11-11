import { paramSummary, TRIGGER_ID } from './rules.ts'

/** What happened at one node of a plan's path. */
export type StepOutcome =
  | { kind: 'branch'; port: string }
  | { kind: 'planned'; action: number }
  | { kind: 'skipped'; reason: string; field?: string; error?: string; until?: string }
  | { kind: 'stopped' }

export interface PlanStep {
  node: string
  type: string
  outcome: StepOutcome
}

interface PlanAction {
  node: string
  capability: string | null
  needsConfirmation: boolean
  action: Record<string, unknown>
}

/** A `holtmail-rules` plan as a dry run or a run of the log carries it. */
export interface PlanView {
  ran: boolean
  ignored: string | null
  path: PlanStep[]
  actions: PlanAction[]
  stopped: boolean
}

/** What a carried-out (or held, or test-mode) action became, as the runs log keeps it. */
export interface ActionResultView {
  node: string
  type: string
  result: string
  detail: string | null
  outbox: string | null
  draft: string | null
}

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === 'object' && value !== null && !Array.isArray(value)

const text = (value: unknown): string | null => (typeof value === 'string' ? value : null)

function readOutcome(raw: Record<string, unknown>): StepOutcome | null {
  switch (raw.outcome) {
    case 'branch':
      return { kind: 'branch', port: text(raw.port) ?? 'no' }
    case 'planned':
      return { kind: 'planned', action: typeof raw.action === 'number' ? raw.action : -1 }
    case 'stopped':
      return { kind: 'stopped' }
    case 'skipped': {
      const reason = isRecord(raw.reason) ? raw.reason : {}
      const out: StepOutcome = { kind: 'skipped', reason: text(reason.code) ?? 'unknown' }
      const field = text(reason.field)
      const error = text(reason.error)
      const until = text(reason.until)
      if (field) out.field = field
      if (error) out.error = error
      if (until) out.until = until
      return out
    }
    default:
      return null
  }
}

/** Reads a plan; anything that is not one reads as a plan that did not run. Steps with an
 * unknown outcome are left out. */
export function readPlan(value: unknown): PlanView {
  const plan = isRecord(value) ? value : {}
  const path: PlanStep[] = []
  for (const raw of Array.isArray(plan.path) ? plan.path : []) {
    if (!isRecord(raw) || typeof raw.node !== 'string') continue
    const outcome = readOutcome(raw)
    if (outcome) path.push({ node: raw.node, type: text(raw.type) ?? '', outcome })
  }
  const actions: PlanAction[] = []
  for (const raw of Array.isArray(plan.actions) ? plan.actions : []) {
    if (!isRecord(raw)) continue
    actions.push({
      node: text(raw.node) ?? '',
      capability: text(raw.capability),
      needsConfirmation: raw.needs_confirmation === true,
      action: isRecord(raw.action) ? raw.action : {},
    })
  }
  return {
    ran: plan.ran === true,
    ignored: text(plan.ignored),
    path,
    actions,
    stopped: plan.stopped === true,
  }
}

/** Reads a run's action results; entries that are not objects are left out. */
export function readResults(value: unknown): ActionResultView[] {
  if (!Array.isArray(value)) return []
  return value.filter(isRecord).map((raw) => ({
    node: text(raw.node) ?? '',
    type: text(raw.type) ?? '',
    result: text(raw.result) ?? 'unknown',
    detail: text(raw.detail),
    outbox: text(raw.outbox),
    draft: text(raw.draft),
  }))
}

/** The port a step left by: the branch's port, `next` after an action, none after `stop`. */
function portOf(outcome: StepOutcome): string | null {
  if (outcome.kind === 'branch') return outcome.port
  if (outcome.kind === 'stopped') return null
  return 'next'
}

/** The nodes and canvas edges (`<source>:<port>`, as `toGraph` names them) a plan went
 * through; empty when the flow did not run. */
export function highlightOf(plan: PlanView): { nodes: Set<string>; edges: Set<string> } {
  const nodes = new Set<string>()
  const edges = new Set<string>()
  if (!plan.ran) return { nodes, edges }
  nodes.add(TRIGGER_ID)
  plan.path.forEach((step, index) => {
    nodes.add(step.node)
    if (index === 0) edges.add(`${TRIGGER_ID}:next`)
    const following = plan.path[index + 1]
    const port = portOf(step.outcome)
    if (following && port) edges.add(`${step.node}:${port}`)
  })
  return { nodes, edges }
}

/** The step of the path at a node, for marking it in the list. */
export function stepAt(plan: PlanView, node: string): PlanStep | undefined {
  return plan.path.find((s) => s.node === node)
}

/** A one-line summary of a planned action: its recipients, label, folder or first text, without
 * the message id. */
export function actionSummary(action: Record<string, unknown>): string {
  const { type: _type, message: _message, auto_submitted: _auto, ...rest } = action
  return paramSummary(rest, 80)
}

/** The places of the actions that wait for a person in a run. */
export function pendingActions(results: readonly ActionResultView[]): number[] {
  return results.flatMap((r, index) => (r.result === 'pending' ? [index] : []))
}
