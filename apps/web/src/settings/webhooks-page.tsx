import {
  createWebhook,
  deleteWebhook,
  rotateWebhookSecret,
  testWebhook,
  updateWebhook,
  type Webhook,
  type WebhookDelivery,
  webhookDeliveriesQuery,
  webhooksQuery,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton, Switch } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type FormEvent, useState } from 'react'

import { Field, FormError, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

function EventChoice({
  events,
  chosen,
  onChange,
}: {
  events: readonly string[]
  chosen: readonly string[]
  onChange: (next: string[]) => void
}) {
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-1 text-sm text-fg">{t.webhookEvents}</legend>
      {events.map((event) => (
        <label key={event} className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={chosen.includes(event)}
            onChange={(e) =>
              onChange(e.target.checked ? [...chosen, event] : chosen.filter((x) => x !== event))
            }
          />
          <span>{t.webhookEventNames[event] ?? event}</span>
          <span className="font-mono text-xs text-fg-subtle">{event}</span>
        </label>
      ))}
    </fieldset>
  )
}

function Secret({ secret, onDone }: { secret: string; onDone: () => void }) {
  return (
    <div className="grid gap-2 border border-accent bg-bg-subtle px-4 py-3">
      <p className="text-sm font-medium">{t.webhookSecret}</p>
      <p className="text-sm text-fg-muted">{t.webhookSecretLead}</p>
      <p className="font-mono text-xs break-all select-all">{secret}</p>
      <div>
        <Button size="sm" onClick={onDone}>
          {t.webhookDone}
        </Button>
      </div>
    </div>
  )
}

function deliveryState(delivery: WebhookDelivery) {
  if (delivery.delivered_at != null)
    return { variant: 'success' as const, label: t.webhookDelivered }
  if (delivery.next_at != null) return { variant: 'warning' as const, label: t.webhookWaiting }
  return { variant: 'danger' as const, label: t.webhookGaveUp }
}

function Deliveries({ id }: { id: string }) {
  const deliveries = useQuery({ ...webhookDeliveriesQuery(id), refetchInterval: 10_000 })
  if (deliveries.isPending) return <Skeleton className="h-16" />
  const items = deliveries.data ?? []
  if (items.length === 0) return <p className="text-sm text-fg-muted">{t.webhookNoDeliveries}</p>
  return (
    <ul className="max-h-64 overflow-y-auto border border-border text-sm">
      {items.map((delivery) => {
        const state = deliveryState(delivery)
        return (
          <li
            key={delivery.id}
            className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-3 py-1.5 last:border-b-0"
          >
            <Badge variant={state.variant}>{state.label}</Badge>
            <span className="font-mono text-xs">{delivery.event}</span>
            <span className="text-xs text-fg-muted">{formatFullDate(delivery.created_at)}</span>
            <span className="ml-auto text-xs text-fg-subtle">
              {t.webhookAttempts(delivery.attempts)}
              {delivery.last_status != null && ` · ${delivery.last_status}`}
              {delivery.last_error && ` · ${delivery.last_error}`}
            </span>
          </li>
        )
      })}
    </ul>
  )
}

function WebhookCard({ hook, events }: { hook: Webhook; events: readonly string[] }) {
  const queryClient = useQueryClient()
  const refresh = () => queryClient.invalidateQueries({ queryKey: ['webhooks'] })
  const [secret, setSecret] = useState<string>()
  const [removing, setRemoving] = useState(false)
  const update = useMutation({
    mutationFn: (change: { enabled?: boolean; events?: string[] }) =>
      updateWebhook(hook.id, change),
    onSettled: refresh,
  })
  const rotate = useMutation({
    mutationFn: () => rotateWebhookSecret(hook.id),
    onSuccess: (answer) => setSecret(answer.secret),
  })
  const test = useMutation({ mutationFn: () => testWebhook(hook.id), onSettled: refresh })
  const error = update.error ?? rotate.error ?? test.error

  return (
    <li className="grid gap-4 border border-border px-4 py-4">
      <div className="flex flex-wrap items-center gap-3">
        <span className="min-w-0 flex-1 font-mono text-xs break-all">{hook.url}</span>
        <label className="flex items-center gap-2 text-sm">
          <Switch
            checked={update.isPending ? (update.variables.enabled ?? hook.enabled) : hook.enabled}
            onCheckedChange={(enabled) => update.mutate({ enabled })}
          />
          {t.webhookOn}
        </label>
      </div>
      {!hook.url.startsWith('https://') && (
        <p className="text-xs text-warning">{t.webhookUrlHint}</p>
      )}
      <EventChoice
        events={events}
        chosen={hook.events}
        onChange={(next) => update.mutate({ events: next })}
      />
      {secret && <Secret secret={secret} onDone={() => setSecret(undefined)} />}
      <FormError message={error ? authErrorMessage(error) : undefined} />
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="ghost" disabled={test.isPending} onClick={() => test.mutate()}>
          {test.isSuccess ? t.webhookTested : t.webhookTest}
        </Button>
        <Button
          size="sm"
          variant="ghost"
          disabled={rotate.isPending}
          onClick={() => rotate.mutate()}
        >
          {t.webhookRotate}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setRemoving(true)}>
          {t.webhookRemove}
        </Button>
      </div>
      <div className="grid gap-2">
        <p className="text-xs text-fg-subtle">{t.webhookDeliveries}</p>
        <Deliveries id={hook.id} />
      </div>
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.webhookRemove}
        description={t.webhookRemoveConfirm(hook.url)}
        action={t.webhookRemove}
        busyAction={t.webhookRemove}
        onConfirm={async () => {
          await deleteWebhook(hook.id)
          await refresh()
        }}
      />
    </li>
  )
}

function AddWebhook({ events }: { events: readonly string[] }) {
  const queryClient = useQueryClient()
  const [url, setUrl] = useState('')
  const [chosen, setChosen] = useState<string[]>([...events])
  const [secret, setSecret] = useState<string>()
  const create = useMutation({
    mutationFn: () => createWebhook({ url: url.trim(), events: chosen }),
    onSuccess: (created) => {
      setSecret(created.secret)
      setUrl('')
      void queryClient.invalidateQueries({ queryKey: ['webhooks'] })
    },
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    create.mutate()
  }
  return (
    <section aria-labelledby="webhook-add" className="grid max-w-2xl gap-4">
      <h3 id="webhook-add" className="text-lg font-normal">
        {t.webhookAdd}
      </h3>
      {secret && <Secret secret={secret} onDone={() => setSecret(undefined)} />}
      <form className="grid gap-4" onSubmit={submit}>
        <Field
          label={t.webhookUrl}
          placeholder="https://hooks.example.com/holtmail"
          autoComplete="off"
          spellCheck={false}
          required
          value={url}
          onChange={(event) => setUrl(event.target.value)}
          hint={t.webhookUrlHint}
        />
        <EventChoice events={events} chosen={chosen} onChange={setChosen} />
        <FormError message={create.isError ? authErrorMessage(create.error) : undefined} />
        <div>
          <SubmitButton busy={create.isPending} label={t.webhookAdd} busyLabel={t.webhookAdd} />
        </div>
      </form>
    </section>
  )
}

export function WebhooksPage() {
  const webhooks = useQuery(webhooksQuery)
  const events = webhooks.data?.events ?? []
  return (
    <SettingsPage title={t.webhooks} lead={t.webhooksLead}>
      {webhooks.isPending ? (
        <Skeleton className="h-32" />
      ) : (
        <>
          {webhooks.data?.webhooks.length ? (
            <ul className="grid max-w-2xl gap-4">
              {webhooks.data.webhooks.map((hook) => (
                <WebhookCard key={hook.id} hook={hook} events={events} />
              ))}
            </ul>
          ) : (
            <p className="text-sm text-fg-muted">{t.webhooksEmpty}</p>
          )}
          <AddWebhook events={events} />
        </>
      )}
    </SettingsPage>
  )
}
