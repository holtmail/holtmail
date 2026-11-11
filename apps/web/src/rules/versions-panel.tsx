import { type FlowVersion, flowVersionsQuery } from '@holtmail/api-client'
import { Badge, Button, cn, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { diffLines, diffText } from '../lib/rules-json.ts'
import { type FlowDefinition, readFlow } from '../lib/rules.ts'
import { strings } from '../strings.ts'

const t = strings.rules.versions

function Diff({ before, after }: { before: FlowDefinition | null; after: FlowDefinition | null }) {
  const lines = diffLines(diffText(before), diffText(after))
  if (!lines.some((l) => l.kind === 'added' || l.kind === 'removed')) {
    return <p className="text-sm text-fg-muted">{t.same}</p>
  }
  return (
    <pre className="overflow-x-auto border border-border font-mono text-xs leading-5">
      {lines.map((line, index) =>
        line.kind === 'gap' ? (
          <div key={index} className="bg-bg-subtle px-2 text-fg-subtle">
            {t.gap(line.count)}
          </div>
        ) : (
          <div
            key={index}
            className={cn(
              'px-2 whitespace-pre',
              line.kind === 'added' && 'border-l-2 border-success text-fg',
              line.kind === 'removed' && 'border-l-2 border-danger text-fg-muted line-through',
              line.kind === 'same' && 'text-fg-subtle',
            )}
          >
            <span aria-hidden="true" className="inline-block w-3 select-none">
              {line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ' '}
            </span>
            <span className="sr-only">
              {line.kind === 'added' ? '+' : line.kind === 'removed' ? '−' : ''}
            </span>
            {line.text}
          </div>
        ),
      )}
    </pre>
  )
}

/** Every saved version, newest first, each with its diff from the version before and a way
 * to open it in the editor. */
export function VersionsPanel({
  flowId,
  current,
  onRestore,
}: {
  flowId: string
  current: number
  onRestore: (flow: FlowDefinition) => void
}) {
  const versions = useQuery(flowVersionsQuery(flowId))
  const [chosen, setChosen] = useState<number | null>(null)
  const list: FlowVersion[] = versions.data ?? []
  const shown = list.find((v) => v.version === chosen) ?? list[0]
  const previous = shown ? list.find((v) => v.version < shown.version) : undefined
  const shownFlow = shown ? readFlow(shown.flow) : null
  return (
    <section aria-label={t.title} className="mx-auto grid w-full max-w-5xl content-start gap-3 p-4">
      <h2 className="text-base text-fg">{t.title}</h2>
      {versions.isPending && <Skeleton className="h-24" />}
      {versions.isError && (
        <p role="alert" className="text-sm text-danger">
          {t.failed} {authErrorMessage(versions.error)}
        </p>
      )}
      {shown && (
        <div className="grid gap-4 md:grid-cols-[14rem_1fr]">
          <ul className="grid content-start divide-y divide-border border-y border-border">
            {list.map((v) => (
              <li key={v.version}>
                <button
                  type="button"
                  aria-pressed={v.version === shown.version}
                  className={cn(
                    'grid w-full gap-0.5 px-2 py-2 text-left hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus',
                    v.version === shown.version && 'bg-bg-selected',
                  )}
                  onClick={() => setChosen(v.version)}
                >
                  <span className="flex items-center gap-2 text-sm text-fg">
                    {strings.rules.page.version(v.version)}
                    {v.version === current && <Badge variant="outline">{t.current}</Badge>}
                  </span>
                  <span className="text-xs text-fg-subtle">
                    {t.saved(formatFullDate(v.saved_at))}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          <div className="grid min-w-0 content-start gap-3">
            <h3 className="text-sm text-fg">
              {previous ? t.compare(previous.version, shown.version) : t.first}
            </h3>
            {previous ? (
              <Diff before={readFlow(previous.flow)} after={shownFlow} />
            ) : (
              <Diff before={null} after={shownFlow} />
            )}
            {shownFlow && (
              <div className="grid gap-1">
                <Button
                  variant="secondary"
                  className="justify-self-start"
                  onClick={() => onRestore(shownFlow)}
                >
                  {t.restore}
                </Button>
                <p className="text-xs text-fg-subtle">{t.restoreLead}</p>
              </div>
            )}
          </div>
        </div>
      )}
    </section>
  )
}
