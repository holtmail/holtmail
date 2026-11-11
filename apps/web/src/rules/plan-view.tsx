import { Badge } from '@holtmail/ui'
import type { ReactNode } from 'react'

import { actionSummary, type PlanStep, type PlanView, type StepOutcome } from '../lib/rules-plan.ts'
import { docsOf, type NodeType, nodeTypeOf } from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'

const t = strings.rules
const locale = currentLocale()

/** An outcome in words: the port a condition took, an action, a skip and why, or stop. */
export function outcomeText(outcome: StepOutcome): string {
  if (outcome.kind === 'branch') return t.inspector.portLabels[outcome.port] ?? outcome.port
  if (outcome.kind === 'planned') return t.test.planned
  if (outcome.kind === 'stopped') return t.test.stop
  const reason = t.skip[outcome.reason] ?? outcome.reason
  const extra = outcome.field
    ? ` (${outcome.field}${outcome.error ? `: ${outcome.error}` : ''})`
    : ''
  return t.test.skipped(`${reason}${extra}`)
}

const stepTitle = (catalogue: NodeType[], step: PlanStep) =>
  docsOf(nodeTypeOf(catalogue, step.type), locale, step.type).title

/**
 * A plan in words: why it did not run, or the path (each step with its outcome) and the
 * actions, each with `renderAction` for what became of it in a run.
 */
export function PlanSteps({
  plan,
  catalogue,
  renderAction,
}: {
  plan: PlanView
  catalogue: NodeType[]
  renderAction?: (index: number) => ReactNode
}) {
  if (!plan.ran) {
    return (
      <p className="text-sm text-fg-muted">
        {plan.ignored ? (t.test.ignored[plan.ignored] ?? plan.ignored) : t.test.notRun}
      </p>
    )
  }
  const titleOf = (node: string) => {
    const step = plan.path.find((s) => s.node === node)
    return step ? stepTitle(catalogue, step) : node
  }
  return (
    <div className="grid gap-3">
      <section className="grid gap-1">
        <h4 className="text-xs text-fg-subtle">{t.test.path}</h4>
        <ol className="grid border-l border-accent">
          {plan.path.map((step, index) => (
            <li
              key={`${step.node}-${index}`}
              className="flex flex-wrap items-baseline gap-x-2 py-0.5 pl-2 text-sm"
            >
              <span className="text-fg">{stepTitle(catalogue, step)}</span>
              <span
                className={
                  step.outcome.kind === 'skipped' ? 'text-xs text-warning' : 'text-xs text-fg-muted'
                }
              >
                {outcomeText(step.outcome)}
              </span>
            </li>
          ))}
        </ol>
      </section>
      <section className="grid gap-1">
        <h4 className="text-xs text-fg-subtle">{t.test.actions}</h4>
        {plan.actions.length === 0 ? (
          <p className="text-sm text-fg-muted">{t.test.noActions}</p>
        ) : (
          <ol className="grid divide-y divide-border border-y border-border">
            {plan.actions.map((action, index) => (
              <li key={`${action.node}-${index}`} className="grid gap-1 py-2 text-sm">
                <span className="flex flex-wrap items-center gap-2">
                  <span className="text-fg">{titleOf(action.node)}</span>
                  {action.needsConfirmation && <Badge variant="warning">{t.test.waits}</Badge>}
                </span>
                {actionSummary(action.action) && (
                  <span className="font-mono text-xs break-words text-fg-muted">
                    {actionSummary(action.action)}
                  </span>
                )}
                {renderAction?.(index)}
              </li>
            ))}
          </ol>
        )}
      </section>
      {plan.stopped && <p className="text-xs text-fg-muted">{t.test.stopped}</p>}
    </div>
  )
}
