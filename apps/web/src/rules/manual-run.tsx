import { runFlow } from '@holtmail/api-client'
import { Button, Input } from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useId, useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { MessagePicker, type PreviewMessage } from './fields.tsx'

const t = strings.rules.manual

/** Runs a saved `manual` flow by hand, on a search or on one message; the runs land in the log
 * below. */
export function ManualRun({
  flowId,
  archiveId,
  dirty,
}: {
  flowId: string
  archiveId: string
  dirty: boolean
}) {
  const queryClient = useQueryClient()
  const queryId = useId()
  const [query, setQuery] = useState('')
  const [message, setMessage] = useState<PreviewMessage | null>(null)
  const run = useMutation({
    mutationFn: (request: { messages: string[] } | { query: string }) => runFlow(flowId, request),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['flows', flowId, 'runs'] }),
  })
  const busy = run.isPending
  return (
    <section aria-labelledby={`${queryId}-title`} className="grid gap-3 border border-border p-3">
      <header className="grid gap-1">
        <h3 id={`${queryId}-title`} className="text-sm text-fg">
          {t.title}
        </h3>
        <p className="text-xs text-fg-subtle">{t.lead}</p>
      </header>
      {dirty && <p className="text-xs text-warning">{t.saveFirst}</p>}
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={(event) => {
          event.preventDefault()
          if (query.trim()) run.mutate({ query: query.trim() })
        }}
      >
        <div className="grid min-w-0 flex-1 gap-1">
          <label htmlFor={queryId} className="text-xs text-fg-muted">
            {t.query}
          </label>
          <Input
            id={queryId}
            type="search"
            value={query}
            spellCheck={false}
            className="font-mono"
            onChange={(event) => setQuery(event.target.value)}
          />
        </div>
        <Button
          type="submit"
          variant="secondary"
          disabled={dirty || busy || !query.trim()}
          aria-busy={busy}
        >
          {busy ? t.running : t.runQuery}
        </Button>
      </form>
      <div className="grid gap-2">
        <MessagePicker
          archiveId={archiveId}
          value={message}
          onChange={setMessage}
          label={t.message}
        />
        <Button
          variant="secondary"
          className="justify-self-start"
          disabled={dirty || busy || !message}
          aria-busy={busy}
          onClick={() => {
            if (message) run.mutate({ messages: [message.id] })
          }}
        >
          {busy ? t.running : t.runMessage}
        </Button>
      </div>
      <div aria-live="polite">
        {run.isSuccess && (
          <p className="text-xs text-fg-muted">
            {run.data.length > 0 ? t.done(run.data.length) : t.none}
          </p>
        )}
        {run.isError && (
          <p role="alert" className="text-xs text-danger">
            {t.failed} {authErrorMessage(run.error)}
          </p>
        )}
      </div>
    </section>
  )
}
