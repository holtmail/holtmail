import {
  confirmFlowAction,
  dismissFlowAction,
  type FlowRun,
  flowRunsInfiniteQuery,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton } from '@holtmail/ui'
import { useInfiniteQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ChevronDownIcon, ChevronRightIcon } from 'lucide-react'
import { type ReactNode, useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { pendingActions, type PlanView, readPlan, readResults } from '../lib/rules-plan.ts'
import { docsOf, type NodeType, nodeTypeOf } from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import { PlanSteps } from './plan-view.tsx'

const t = strings.rules.runs
const locale = currentLocale()

const statusVariant = (status: string) =>
  status === 'failed' ? 'danger' : status === 'interrupted' ? 'warning' : 'outline'

const resultVariant = (result: string) =>
  result === 'failed'
    ? 'danger'
    : result === 'pending' || result === 'draft' || result === 'confirming'
      ? 'warning'
      : result === 'done' || result === 'confirmed'
        ? 'success'
        : 'outline'

function RunDetails({
  run,
  archiveId,
  catalogue,
  onShowPath,
}: {
  run: FlowRun
  archiveId: string
  catalogue: NodeType[]
  onShowPath: (plan: PlanView, version: number) => void
}) {
  const queryClient = useQueryClient()
  const plan = run.plan ? readPlan(run.plan) : null
  const results = readResults(run.results)
  const decide = useMutation({
    mutationFn: ({ index, confirm }: { index: number; confirm: boolean }) =>
      confirm ? confirmFlowAction(run.id, index) : dismissFlowAction(run.id, index),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['flows', run.flow, 'runs'] }),
  })
  return (
    <div className="grid gap-3 pb-3 pl-6">
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
        <dt className="text-fg-subtle">{strings.rules.inspector.trigger}</dt>
        <dd className="text-fg-muted">
          {docsOf(nodeTypeOf(catalogue, run.trigger), locale, run.trigger).title}
        </dd>
        <dt className="text-fg-subtle">{strings.rules.inspector.id}</dt>
        <dd className="font-mono break-all text-fg-muted">{run.event}</dd>
      </dl>
      {run.error && <p className="text-sm text-danger">{run.error}</p>}
      <div className="flex flex-wrap gap-2">
        {run.message && (
          <Button variant="secondary" size="sm" asChild>
            <Link to="/a/$archiveId" params={{ archiveId }} search={{ m: run.message }}>
              {t.message}
            </Link>
          </Button>
        )}
        {plan?.ran && (
          <Button variant="secondary" size="sm" onClick={() => onShowPath(plan, run.version)}>
            {t.showPath}
          </Button>
        )}
      </div>
      {plan ? (
        <PlanSteps
          plan={plan}
          catalogue={catalogue}
          renderAction={(index) => {
            const result = results[index]
            if (!result) return null
            return (
              <span className="flex flex-wrap items-center gap-2">
                <Badge variant={resultVariant(result.result)}>
                  {t.results[result.result] ?? result.result}
                </Badge>
                {result.detail && <span className="text-xs text-fg-muted">{result.detail}</span>}
                {result.result === 'pending' && (
                  <>
                    <Button
                      size="sm"
                      variant="primary"
                      disabled={decide.isPending}
                      onClick={() => decide.mutate({ index, confirm: true })}
                    >
                      {t.confirm}
                    </Button>
                    <Button
                      size="sm"
                      variant="ghost"
                      disabled={decide.isPending}
                      onClick={() => decide.mutate({ index, confirm: false })}
                    >
                      {t.dismiss}
                    </Button>
                  </>
                )}
              </span>
            )
          }}
        />
      ) : (
        <p className="text-sm text-fg-muted">{t.noPlan}</p>
      )}
      {decide.isError && (
        <p role="alert" className="text-sm text-danger">
          {t.decideFailed} {authErrorMessage(decide.error)}
        </p>
      )}
    </div>
  )
}

/** The runs log of a flow, newest first, with held actions to confirm or dismiss. */
export function RunsPanel({
  flowId,
  archiveId,
  catalogue,
  onShowPath,
  children,
}: {
  flowId: string
  archiveId: string
  catalogue: NodeType[]
  onShowPath: (plan: PlanView, version: number) => void
  children?: ReactNode
}) {
  const runs = useInfiniteQuery(flowRunsInfiniteQuery(flowId))
  const [open, setOpen] = useState<string | null>(null)
  const all = runs.data?.pages.flat() ?? []
  return (
    <section aria-label={t.title} className="mx-auto grid w-full max-w-3xl content-start gap-3 p-4">
      <header className="grid gap-1">
        <h2 className="text-base text-fg">{t.title}</h2>
        <p className="text-xs text-fg-subtle">{t.lead}</p>
      </header>
      {children}
      {runs.isPending && <Skeleton className="h-24" />}
      {runs.isError && (
        <p role="alert" className="text-sm text-danger">
          {t.failed} {authErrorMessage(runs.error)}
        </p>
      )}
      {runs.isSuccess && all.length === 0 && <p className="text-sm text-fg-muted">{t.empty}</p>}
      {all.length > 0 && (
        <ul className="grid divide-y divide-border border-y border-border">
          {all.map((run) => {
            const expanded = open === run.id
            const pending = pendingActions(readResults(run.results)).length > 0
            return (
              <li key={run.id} className="grid">
                <button
                  type="button"
                  aria-expanded={expanded}
                  aria-label={t.details(formatFullDate(run.started_at))}
                  className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2 text-left text-sm hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus"
                  onClick={() => setOpen(expanded ? null : run.id)}
                >
                  {expanded ? (
                    <ChevronDownIcon className="size-4 text-fg-subtle" />
                  ) : (
                    <ChevronRightIcon className="size-4 text-fg-subtle" />
                  )}
                  <span className="text-fg tabular-nums">{formatFullDate(run.started_at)}</span>
                  <Badge variant={statusVariant(run.status)}>
                    {t.statuses[run.status] ?? run.status}
                  </Badge>
                  {run.test && <Badge variant="outline">{t.test}</Badge>}
                  {pending && <Badge variant="warning">{t.results.pending}</Badge>}
                  <span className="text-xs text-fg-subtle">{t.version(run.version)}</span>
                </button>
                {expanded && (
                  <RunDetails
                    run={run}
                    archiveId={archiveId}
                    catalogue={catalogue}
                    onShowPath={onShowPath}
                  />
                )}
              </li>
            )
          })}
        </ul>
      )}
      {runs.hasNextPage && (
        <Button
          variant="secondary"
          className="justify-self-start"
          disabled={runs.isFetchingNextPage}
          onClick={() => void runs.fetchNextPage()}
        >
          {runs.isFetchingNextPage ? t.loading : t.more}
        </Button>
      )}
    </section>
  )
}
