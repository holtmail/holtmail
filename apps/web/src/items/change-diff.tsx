import { pimChangeDiffQuery } from '@holtmail/api-client'
import { cn, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'

import { fieldLabel } from '../lib/events.ts'
import { strings } from '../strings.ts'

const t = strings.edits

function Values({ values, changed }: { values: string[]; changed: boolean }) {
  if (values.length === 0) return <span className="text-fg-subtle">{t.diff.empty}</span>
  return (
    <ul className={cn('grid gap-0.5 wrap-break-word', changed ? 'text-fg' : 'text-fg-muted')}>
      {values.map((value, index) => (
        <li key={`${value}-${index}`} className="whitespace-pre-wrap">
          {value}
        </li>
      ))}
    </ul>
  )
}

/** A change next to the version it was made on and the account's latest version, field by
 * field, with the change's own text on demand. */
export function ChangeDiff({ changeId }: { changeId: string }) {
  const diff = useQuery(pimChangeDiffQuery(changeId))
  const [raw, setRaw] = useState(false)
  if (diff.isPending) return <Skeleton className="h-16" />
  if (diff.isError) {
    return (
      <p role="alert" className="text-sm text-danger">
        {t.timeline.diffFailed}: {diff.error.message}
      </p>
    )
  }
  const data = diff.data
  return (
    <div className="grid gap-2 text-sm">
      {data.upstream_deleted && <p className="text-warning">{t.diff.gone}</p>}
      {data.mine == null && <p className="text-fg-muted">{t.diff.deletes}</p>}
      {data.fields.length === 0 ? (
        <p className="text-fg-subtle">{t.diff.none}</p>
      ) : (
        <div className="overflow-x-auto border border-border">
          <table className="w-full min-w-xl border-collapse text-left">
            <caption className="sr-only">{t.diff.caption}</caption>
            <thead className="text-xs text-fg-subtle">
              <tr className="border-b border-border">
                <th scope="col" className="w-36 px-2 py-1 font-normal">
                  {t.diff.field}
                </th>
                <th scope="col" className="px-2 py-1 font-normal">
                  {t.diff.base}
                </th>
                <th scope="col" className="px-2 py-1 font-normal">
                  {t.diff.mine}
                </th>
                <th scope="col" className="px-2 py-1 font-normal">
                  {t.diff.theirs}
                </th>
              </tr>
            </thead>
            <tbody>
              {data.fields.map((field) => (
                <tr
                  key={field.field}
                  className={cn(
                    'border-b border-border align-top last:border-b-0',
                    field.conflict && 'bg-bg-subtle',
                  )}
                >
                  <th scope="row" className="px-2 py-1.5 font-normal text-fg-subtle">
                    {fieldLabel(field.field)}
                    {field.conflict && (
                      <span className="mt-0.5 block text-xs text-warning">{t.diff.conflict}</span>
                    )}
                  </th>
                  <td className="px-2 py-1.5">
                    <Values values={field.base} changed={false} />
                  </td>
                  <td className="px-2 py-1.5">
                    <Values values={field.mine} changed={field.mine_changed} />
                  </td>
                  <td className="px-2 py-1.5">
                    <Values values={field.theirs} changed={field.theirs_changed} />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {data.mine != null && (
        <div className="grid gap-1">
          <button
            type="button"
            aria-expanded={raw}
            className="justify-self-start text-xs text-fg-subtle underline-offset-2 hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            onClick={() => setRaw((v) => !v)}
          >
            {t.timeline.raw}
          </button>
          {raw && (
            <pre className="max-h-64 overflow-auto border border-border bg-bg-subtle p-2 font-mono text-xs whitespace-pre-wrap">
              {data.mine}
            </pre>
          )}
        </div>
      )}
    </div>
  )
}
