import { ledgerQuery, verifyLedger } from '@holtmail/api-client'
import { Badge, Button, CopyButton, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

export function IntegrityPage() {
  const queryClient = useQueryClient()
  const ledger = useQuery(ledgerQuery)
  const [anchor, setAnchor] = useState('')
  const verify = useMutation({
    mutationFn: (blobs: boolean) => verifyLedger({ blobs, anchor: anchor.trim() || null }),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tasks'] })
      void queryClient.invalidateQueries({ queryKey: ['ledger'] })
    },
  })
  const head = ledger.data?.head
  const last = ledger.data?.last_verification
  const written = head ? `${head.seq}:${head.hash}` : ''

  return (
    <SettingsPage title={t.integrity} lead={t.integrityLead}>
      <section aria-labelledby="ledger-head" className="grid max-w-xl gap-3">
        <h3 id="ledger-head" className="text-lg font-normal">
          {t.ledgerHead}
        </h3>
        {ledger.isPending ? (
          <Skeleton className="h-16" />
        ) : head ? (
          <div className="grid gap-2 border border-border bg-bg-subtle px-4 py-3">
            <p className="font-mono text-xs break-all text-fg">{written}</p>
            <div className="flex items-center justify-between gap-2 text-xs text-fg-muted">
              <span>{formatFullDate(head.at)}</span>
              <CopyButton text={written} label={t.copy} copiedLabel={t.copied} />
            </div>
          </div>
        ) : (
          <p className="text-sm text-fg-muted">{t.ledgerEmpty}</p>
        )}
      </section>

      <section aria-labelledby="ledger-check" className="grid max-w-xl gap-4">
        <h3 id="ledger-check" className="text-lg font-normal">
          {t.ledgerLast}
        </h3>
        {last ? (
          <div className="grid gap-2 text-sm">
            <p className="flex flex-wrap items-center gap-2">
              <Badge variant={last.ok ? 'success' : 'danger'}>
                {last.ok ? t.ledgerOk(last.entries) : t.ledgerProblems(last.problem_count)}
              </Badge>
              <span className="text-fg-muted">{formatFullDate(last.finished_at)}</span>
              {last.blobs_checked != null && (
                <span className="text-fg-muted">· {t.ledgerBlobs(last.blobs_checked)}</span>
              )}
            </p>
            {last.problems.length > 0 && (
              <ul className="max-h-48 overflow-y-auto border border-border font-mono text-xs">
                {last.problems.map((problem) => (
                  <li key={problem} className="border-b border-border px-3 py-1.5 last:border-b-0">
                    {problem}
                  </li>
                ))}
              </ul>
            )}
          </div>
        ) : (
          <p className="text-sm text-fg-muted">{t.ledgerNever}</p>
        )}
        <Field
          label={t.ledgerAnchor}
          placeholder="1234:9f86d081…"
          autoComplete="off"
          spellCheck={false}
          value={anchor}
          onChange={(event) => setAnchor(event.target.value)}
          hint={t.ledgerAnchorHint}
        />
        <FormError message={verify.isError ? authErrorMessage(verify.error) : undefined} />
        <div className="flex flex-wrap gap-2">
          <Button disabled={verify.isPending} onClick={() => verify.mutate(false)}>
            {t.ledgerVerify}
          </Button>
          <Button
            variant="ghost"
            disabled={verify.isPending}
            title={t.ledgerVerifyBytesHint}
            onClick={() => verify.mutate(true)}
          >
            {t.ledgerVerifyBytes}
          </Button>
        </div>
        {verify.isSuccess && (
          <p className="text-sm text-fg-muted">
            {t.ledgerStarted}{' '}
            <Link to="/settings/tasks" className="underline">
              {t.tasks}
            </Link>
          </p>
        )}
      </section>
    </SettingsPage>
  )
}
