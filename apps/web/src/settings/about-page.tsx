import { insightsDataQuery } from '@holtmail/api-client'
import { Badge, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'

import { attributions, dataFileRows, updateLines } from '../lib/about.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.about

function Detail({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <span>
      <span className="text-fg-subtle">{label}: </span>
      {children}
    </span>
  )
}

export function AboutPage() {
  const data = useQuery(insightsDataQuery())

  return (
    <SettingsPage title={strings.settings.about} lead={strings.settings.aboutLead}>
      {data.isPending ? (
        <Skeleton className="h-48" />
      ) : data.isError ? (
        <p role="alert" className="text-sm text-danger">
          {data.error.message}
        </p>
      ) : (
        <>
          <dl className="max-w-2xl border-y border-border">
            <div className="grid grid-cols-[10rem_1fr] gap-3 py-2 text-sm max-sm:grid-cols-1 max-sm:gap-1">
              <dt className="text-fg-muted">{t.version}</dt>
              <dd className="font-mono text-xs leading-5 text-fg">{data.data.holtmail_version}</dd>
            </div>
          </dl>
          <section aria-labelledby="about-data" className="grid max-w-2xl gap-3">
            <div className="grid gap-1">
              <h3 id="about-data" className="text-lg font-normal">
                {t.dataHeading}
              </h3>
              <p className="text-sm text-fg-muted">{t.dataLead}</p>
            </div>
            {data.data.files.length === 0 ? (
              <p className="border border-border bg-bg-subtle px-4 py-3 text-sm text-fg-muted">
                {t.noData}
              </p>
            ) : (
              <ul className="grid divide-y divide-border border-y border-border">
                {dataFileRows(data.data.files).map((row) => (
                  <li key={row.key} className="grid gap-1 py-3">
                    <span className="flex min-w-0 flex-wrap items-baseline gap-2">
                      <span className="text-sm text-fg">{row.name}</span>
                      <Badge variant="outline">{row.origin}</Badge>
                    </span>
                    <span className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-fg-muted">
                      <Detail label={t.fileVersion}>
                        <span className="font-mono">{row.version}</span>
                      </Detail>
                      <Detail label={t.rows}>
                        <span className="tabular-nums">{row.rows}</span>
                      </Detail>
                      <Detail label={t.license}>{row.license}</Detail>
                      <Detail label={t.file}>
                        <span className="font-mono">{row.file}</span>
                      </Detail>
                    </span>
                  </li>
                ))}
              </ul>
            )}
            {attributions(data.data.files).map((line) => (
              <p key={line} className="text-sm text-fg">
                {line}
              </p>
            ))}
            <div className="grid gap-1 text-sm text-fg-muted">
              {updateLines(data.data.updates).map((line) => (
                <p key={line}>{line}</p>
              ))}
            </div>
          </section>
        </>
      )}
    </SettingsPage>
  )
}
