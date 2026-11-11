import { type FlowIssue, flowIssues, testFlow } from '@holtmail/api-client'
import { Button, Switch } from '@holtmail/ui'
import { useMutation } from '@tanstack/react-query'
import { useId, useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { type PlanView, readPlan } from '../lib/rules-plan.ts'
import { changed, type FlowDefinition, type NodeType } from '../lib/rules.ts'
import { strings } from '../strings.ts'
import { MessagePicker, type PreviewMessage } from './fields.tsx'
import { PlanSteps } from './plan-view.tsx'

const t = strings.rules.test

/** What the last test planned, for the flow it was made with. */
export interface TestResult {
  flow: FlowDefinition
  plan: PlanView
  error: string | null
}

/** Whether a test still describes the flow in the editor. */
export const testIsCurrent = (result: TestResult | null, flow: FlowDefinition) =>
  result !== null && !changed(result.flow, flow)

/** Test with a message: a dry run of the editor's flow (unsaved changes included) for one
 * message; nothing is carried out or logged. */
export function TestPanel({
  flowId,
  archiveId,
  flow,
  catalogue,
  message,
  onMessage,
  result,
  onResult,
  onIssues,
}: {
  flowId: string
  archiveId: string
  flow: FlowDefinition
  catalogue: NodeType[]
  message: PreviewMessage | null
  onMessage: (message: PreviewMessage | null) => void
  result: TestResult | null
  onResult: (result: TestResult | null) => void
  onIssues: (issues: FlowIssue[]) => void
}) {
  const liveId = useId()
  const [live, setLive] = useState(true)
  const run = useMutation({
    mutationFn: (tested: FlowDefinition) =>
      testFlow(flowId, { flow: tested, message: message?.id ?? null, live }),
    onSuccess: (answer, tested) => {
      onIssues([])
      onResult({ flow: tested, plan: readPlan(answer.plan), error: answer.error ?? null })
    },
    onError: (error) => {
      const found = flowIssues(error)
      if (found) onIssues(found)
      onResult(null)
    },
  })
  const current = testIsCurrent(result, flow)
  return (
    <section aria-label={t.title} className="grid content-start gap-3 p-3">
      <header className="grid gap-1">
        <h2 className="text-base text-fg">{t.title}</h2>
        <p className="text-xs text-fg-subtle">{t.lead}</p>
      </header>
      <MessagePicker
        archiveId={archiveId}
        value={message}
        onChange={onMessage}
        label={t.message}
        hint={t.noMessage}
      />
      <div className="grid gap-1">
        <div className="flex items-center gap-3">
          <Switch id={liveId} checked={live} onCheckedChange={setLive} />
          <label htmlFor={liveId} className="text-sm text-fg">
            {t.live}
          </label>
        </div>
        <p className="text-xs text-fg-subtle">{t.liveLead}</p>
      </div>
      <Button
        variant="primary"
        className="justify-self-start"
        disabled={run.isPending}
        aria-busy={run.isPending}
        onClick={() => run.mutate(flow)}
      >
        {run.isPending ? t.running : t.run}
      </Button>
      <div aria-live="polite" className="grid gap-3">
        {run.isError && (
          <p role="alert" className="text-sm text-danger">
            {flowIssues(run.error)
              ? strings.rules.editor.issues(flowIssues(run.error)?.length ?? 0)
              : authErrorMessage(run.error)}
          </p>
        )}
        {result && !current && <p className="text-xs text-warning">{t.stale}</p>}
        {result && current && result.error && (
          <p className="text-sm text-danger">
            {t.failed} {result.error}
          </p>
        )}
        {result && current && !result.error && (
          <PlanSteps plan={result.plan} catalogue={catalogue} />
        )}
      </div>
    </section>
  )
}
