import { imageProxyQuery, setImageProxy } from '@holtmail/api-client'
import { Button, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ShieldCheckIcon } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Field, FormError, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

export function NetworkPage() {
  const queryClient = useQueryClient()
  const proxy = useQuery(imageProxyQuery)
  const [value, setValue] = useState('')
  const mutation = useMutation({
    mutationFn: (next: string | null) => setImageProxy(next),
    onSuccess: (updated) => {
      queryClient.setQueryData(imageProxyQuery.queryKey, updated)
      setValue('')
    },
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate(value.trim())
  }
  const address = proxy.data?.address
  const fromConfig = proxy.data?.from_config ?? false

  return (
    <SettingsPage title={t.network} lead={t.networkLead}>
      <section aria-labelledby="image-proxy" className="grid max-w-xl gap-4">
        <div className="grid gap-1">
          <h3 id="image-proxy" className="text-lg font-normal">
            {t.imageProxy}
          </h3>
          <p className="text-sm text-fg-muted">{t.imageProxyLead}</p>
        </div>
        {proxy.isPending ? (
          <Skeleton className="h-12" />
        ) : (
          <p className="flex items-start gap-3 border border-border bg-bg-subtle px-4 py-3 text-sm text-fg">
            <ShieldCheckIcon
              aria-hidden
              className={
                address
                  ? 'mt-0.5 size-4 shrink-0 text-success'
                  : 'mt-0.5 size-4 shrink-0 text-fg-subtle'
              }
            />
            <span className="grid gap-1">
              <span className="font-mono text-xs break-all">
                {address ? t.imageProxyCurrent(address) : t.imageProxyNone}
              </span>
              {fromConfig && <span className="text-xs text-fg-muted">{t.imageProxyConfig}</span>}
            </span>
          </p>
        )}
        {!fromConfig && (
          <form className="grid gap-4" onSubmit={submit}>
            <Field
              label={t.imageProxyLabel}
              placeholder="socks5h://127.0.0.1:9050"
              autoComplete="off"
              autoCapitalize="none"
              spellCheck={false}
              required
              value={value}
              onChange={(event) => setValue(event.target.value)}
              hint={
                <>
                  {t.imageProxyHint} {t.imageProxySecret}
                </>
              }
            />
            <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
            <div className="flex flex-wrap gap-2">
              <SubmitButton
                busy={mutation.isPending && mutation.variables !== null}
                label={t.imageProxySave}
                busyLabel={t.imageProxyChecking}
              />
              {address && (
                <Button
                  variant="ghost"
                  disabled={mutation.isPending}
                  onClick={() => mutation.mutate(null)}
                >
                  {t.imageProxyRemove}
                </Button>
              )}
            </div>
          </form>
        )}
      </section>
    </SettingsPage>
  )
}
