import type { FlowIssue } from '@holtmail/api-client'
import { Button, cn } from '@holtmail/ui'
import { ArrowDownIcon, ArrowUpIcon } from 'lucide-react'
import type { ReactNode } from 'react'

import { appendStep, canMove, chainWords, moveStep, removeStep } from '../lib/rules-list.ts'
import { type PlanView, stepAt } from '../lib/rules-plan.ts'
import {
  addNode,
  docsOf,
  type FlowDefinition,
  type NodeType,
  nodeTypeOf,
  paramsOf,
  paramSummary,
  setConfirm,
  setParams,
  setTrigger,
  TRIGGER_ID,
} from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import type { FieldContext } from './fields.tsx'
import { Inspector } from './inspector.tsx'
import { Palette } from './palette.tsx'
import { outcomeText } from './plan-view.tsx'

const t = strings.rules
const locale = currentLocale()

/**
 * List mode for a linear flow: "When … if … then …", one row per step in chain order, the
 * selected step's settings open under it, and the palette below to add a step at the end.
 */
export function ListView({
  flow,
  chain,
  catalogue,
  role,
  selected,
  issues,
  context,
  plan,
  onSelect,
  onChange,
  children,
}: {
  flow: FlowDefinition
  chain: string[]
  catalogue: NodeType[]
  role: string
  selected: string | null
  issues: Map<string, FlowIssue[]>
  context: FieldContext
  plan: PlanView | null
  onSelect: (id: string | null) => void
  onChange: (change: (current: FlowDefinition) => FlowDefinition) => void
  children?: ReactNode
}) {
  const words = chainWords(flow, catalogue, chain)
  const rows = [TRIGGER_ID, ...chain]
  return (
    <div className="mx-auto grid w-full max-w-2xl content-start gap-6 p-4">
      <section aria-label={t.list.steps} className="grid gap-2">
        <ol className="grid divide-y divide-border border-y border-border">
          {rows.map((id, index) => {
            const isTrigger = id === TRIGGER_ID
            const typeId = isTrigger
              ? flow.trigger.type
              : (flow.nodes.find((n) => n.id === id)?.type ?? '')
            const type = nodeTypeOf(catalogue, typeId)
            const title = docsOf(type, locale, typeId).title
            const summary = paramSummary(paramsOf(flow, id))
            const open = selected === id
            const step = plan?.ran ? (isTrigger ? null : stepAt(plan, id)) : null
            const onPath = plan?.ran === true && (isTrigger || step !== undefined)
            const count = issues.get(id)?.length ?? 0
            const position = index - 1
            return (
              <li
                key={id}
                className={cn('grid border-l-2', onPath ? 'border-accent' : 'border-transparent')}
              >
                <div className="flex items-start gap-2 py-2 pl-2">
                  <button
                    type="button"
                    aria-expanded={open}
                    aria-label={t.list.edit(title)}
                    className="grid min-w-0 flex-1 gap-0.5 text-left focus-visible:outline-2 focus-visible:outline-focus"
                    onClick={() => onSelect(open ? null : id)}
                  >
                    <span className="flex flex-wrap items-baseline gap-x-2">
                      <span className="w-12 shrink-0 font-mono text-[0.625rem] tracking-wide text-fg-subtle uppercase">
                        {t.list.words[words.get(id) ?? 'do']}
                      </span>
                      <span className={cn('text-sm text-fg', !type && 'text-danger')}>{title}</span>
                      {step && (
                        <span className="text-xs text-fg-muted">{outcomeText(step.outcome)}</span>
                      )}
                    </span>
                    {summary && (
                      <span className="truncate pl-14 font-mono text-xs text-fg-muted">
                        {summary}
                      </span>
                    )}
                    {count > 0 && (
                      <span className="pl-14 text-xs text-danger">{t.editor.issues(count)}</span>
                    )}
                    {type?.role === 'condition' && (
                      <span className="pl-14 text-xs text-fg-subtle">{t.list.otherwise}</span>
                    )}
                  </button>
                  {!isTrigger && (
                    <span className="flex shrink-0">
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={t.list.moveUp}
                        disabled={!canMove(flow, catalogue, chain, position, -1)}
                        onClick={() => onChange((f) => moveStep(f, catalogue, chain, position, -1))}
                      >
                        <ArrowUpIcon />
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        aria-label={t.list.moveDown}
                        disabled={!canMove(flow, catalogue, chain, position, 1)}
                        onClick={() => onChange((f) => moveStep(f, catalogue, chain, position, 1))}
                      >
                        <ArrowDownIcon />
                      </Button>
                    </span>
                  )}
                </div>
                {open && (
                  <div className="border-t border-border bg-bg-subtle">
                    <Inspector
                      flow={flow}
                      catalogue={catalogue}
                      selected={id}
                      issues={issues.get(id) ?? []}
                      context={context}
                      onParams={(node, params) => onChange((f) => setParams(f, node, params))}
                      onTrigger={(next) => onChange((f) => setTrigger(f, next))}
                      onConfirm={(node, confirm) => onChange((f) => setConfirm(f, node, confirm))}
                      onRemove={(node) => {
                        onChange((f) => removeStep(f, catalogue, chain, node))
                        onSelect(null)
                      }}
                    />
                  </div>
                )}
              </li>
            )
          })}
        </ol>
        {chain.length === 0 && <p className="text-sm text-fg-muted">{t.list.empty}</p>}
      </section>
      <section aria-label={t.list.add} className="border border-border">
        <Palette
          catalogue={catalogue}
          trigger={flow.trigger.type}
          role={role}
          onAdd={(type) => {
            const added = addNode(flow, type, { x: 0, y: 0 })
            onChange(() => appendStep(added.flow, catalogue, chain, added.id))
            onSelect(added.id)
          }}
        />
      </section>
      {children}
    </div>
  )
}
