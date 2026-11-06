import { archivesQuery, smtpQuery } from '@holtmail/api-client'
import { Badge, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'

import { formatCount, formatFullDate } from '../lib/format.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[10rem_1fr] gap-3 border-b border-border py-2 text-sm last:border-b-0 max-sm:grid-cols-1 max-sm:gap-1">
      <dt className="text-fg-muted">{label}</dt>
      <dd className="text-fg">{children}</dd>
    </div>
  )
}

export function SmtpPage() {
  const smtp = useQuery(smtpQuery)
  const archives = useQuery(archivesQuery)
  const archiveName = (id: string) => archives.data?.find((a) => a.id === id)?.name ?? id
  const data = smtp.data

  return (
    <SettingsPage title={t.smtp} lead={t.smtpLead}>
      {smtp.isPending ? (
        <Skeleton className="h-40" />
      ) : !data?.enabled ? (
        <p className="max-w-xl border border-border bg-bg-subtle px-4 py-3 text-sm text-fg-muted">
          {t.smtpOff}
        </p>
      ) : (
        <>
          <dl className="max-w-2xl border-t border-border">
            <Row label={t.smtpListen}>
              <span className="font-mono text-xs">{data.listen}</span>
              {data.hostname && <span className="text-fg-muted"> · {data.hostname}</span>}
            </Row>
            <Row label={t.smtpSecurity}>
              <span className="flex flex-wrap gap-2">
                {data.starttls && <Badge variant="outline">{t.smtpStarttls}</Badge>}
                {data.require_tls && <Badge variant="outline">{t.smtpRequireTls}</Badge>}
                {data.auth && <Badge variant="outline">{t.smtpAuth}</Badge>}
                {!data.starttls && !data.auth && <Badge variant="warning">{t.smtpOpen}</Badge>}
              </span>
            </Row>
            <Row label={t.smtpAllow}>
              <span className="font-mono text-xs">{data.allow.join(', ')}</span>
            </Row>
            {data.max_size_mb != null && <Row label="">{t.smtpMaxSize(data.max_size_mb)}</Row>}
          </dl>
          <section aria-labelledby="smtp-routes" className="grid max-w-2xl gap-3">
            <h3 id="smtp-routes" className="text-lg font-normal">
              {t.smtpRoutes}
            </h3>
            <ul className="border border-border">
              {data.routes.map((route) => (
                <li
                  key={`${route.recipient}-${route.archive_id}-${route.folder}`}
                  className="grid gap-1 border-b border-border px-4 py-2 text-sm last:border-b-0"
                >
                  <span className="font-mono text-xs text-fg">{route.recipient}</span>
                  <span className="text-fg-muted">
                    {archiveName(route.archive_id)} / {route.folder}
                    {route.journal && ` · ${t.smtpJournal}`}
                  </span>
                </li>
              ))}
            </ul>
          </section>
          <section aria-labelledby="smtp-counts" className="grid max-w-2xl gap-3">
            <h3 id="smtp-counts" className="text-lg font-normal">
              {t.smtpCounts}
            </h3>
            <ul className="grid gap-1 text-sm text-fg-muted">
              <li>
                <span className="text-fg tabular-nums">{formatCount(data.received)}</span>{' '}
                {t.smtpReceived}
              </li>
              <li>
                <span className="text-fg tabular-nums">{formatCount(data.deleted)}</span>{' '}
                {t.smtpDeleted}
              </li>
              <li>
                <span className="text-fg tabular-nums">{formatCount(data.refused_recipients)}</span>{' '}
                {t.smtpRefused}
              </li>
              <li>
                <span className="text-fg tabular-nums">{formatCount(data.failed)}</span>{' '}
                {t.smtpFailed}
              </li>
              {data.last_received_at != null && (
                <li>
                  {t.smtpLast}: {formatFullDate(data.last_received_at)}
                </li>
              )}
            </ul>
          </section>
        </>
      )}
    </SettingsPage>
  )
}
