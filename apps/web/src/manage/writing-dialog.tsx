import {
  type Account,
  type AccountCapabilities,
  capabilitiesQuery,
  type Capability,
  retentionQuery,
  setCapabilities,
  setRetention,
} from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Skeleton,
  Switch,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { TriangleAlertIcon } from 'lucide-react'
import { useEffect, useId, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { parseRetentionDays } from '../lib/mailbox.ts'
import { capabilitySwitch, toggleCapability } from '../lib/writing.ts'
import { strings } from '../strings.ts'
import { ConfirmDialog } from './confirm-dialog.tsx'

const t = strings.writing.capabilities
const r = strings.writing.mailbox.retention

const DRY_RUN_DELAY_MS = 400

/** The account's write capabilities, each with what it lets Holtmail do in plain words; every
 * switch is saved at once. */
export function WritingDialog({
  account,
  open,
  onOpenChange,
}: {
  account: Account
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const query = capabilitiesQuery(account.id)
  const capabilities = useQuery({ ...query, enabled: open })
  const save = useMutation({
    mutationFn: (next: Capability[]) => setCapabilities(account.id, next),
    onSuccess: (data) => queryClient.setQueryData(query.queryKey, data),
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: ['archives'] }),
        queryClient.invalidateQueries({ queryKey: ['accounts', account.id, 'retention'] }),
      ]),
  })
  const data = capabilities.data
  const switches = data?.switches

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t.title(account.name)}</DialogTitle>
          <DialogDescription>{t.lead}</DialogDescription>
        </DialogHeader>
        {switches && !switches.enabled && (
          <p className="border border-border bg-bg-subtle p-3 text-sm text-fg-muted">
            {t.switchesOff}
          </p>
        )}
        {switches?.enabled && !switches.send && (
          <p className="border border-border bg-bg-subtle p-3 text-sm text-fg-muted">{t.sendOff}</p>
        )}
        {capabilities.isPending ? (
          <Skeleton className="h-64" />
        ) : capabilities.isError || !data ? (
          <FormError message={t.loadFailed} />
        ) : (
          <ul className="grid max-h-[60vh] divide-y divide-border overflow-y-auto border-y border-border">
            {t.order.map((capability) => (
              <CapabilityRow
                key={capability}
                capability={capability}
                state={capabilitySwitch(data, capability)}
                busy={save.isPending}
                onChange={(on) => save.mutate(toggleCapability(data.capabilities, capability, on))}
              />
            ))}
          </ul>
        )}
        <FormError message={save.isError ? authErrorMessage(save.error) : undefined} />
        {data && account.kind !== 'import' && (
          <RetentionSection key={account.id} account={account} capabilities={data} />
        )}
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {t.close}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function CapabilityRow({
  capability,
  state,
  busy,
  onChange,
}: {
  capability: Capability
  state: ReturnType<typeof capabilitySwitch>
  busy: boolean
  onChange: (on: boolean) => void
}) {
  const id = useId()
  const item = t.items[capability]
  return (
    <li className="flex items-start gap-4 py-3">
      <div className="grid min-w-0 flex-1 gap-1">
        <label htmlFor={id} className="text-sm text-fg">
          {item.label}
        </label>
        <p id={`${id}-hint`} className="text-xs text-fg-muted">
          {item.consequence}
        </p>
        {state.on && !state.effective && <p className="text-xs text-warning">{t.inactive}</p>}
        {state.blocked && <p className="text-xs text-fg-subtle">{t.needsDelete}</p>}
      </div>
      <Switch
        id={id}
        aria-describedby={`${id}-hint`}
        checked={state.on}
        disabled={busy || state.blocked}
        onCheckedChange={onChange}
      />
    </li>
  )
}

/** "Delete from the server after N days" with the count a period would delete now and a
 * confirmation before a period is saved. */
function RetentionSection({
  account,
  capabilities,
}: {
  account: Account
  capabilities: AccountCapabilities
}) {
  const queryClient = useQueryClient()
  const stored = useQuery(retentionQuery(account.id))
  const [text, setText] = useState<string | undefined>()
  const [confirming, setConfirming] = useState(false)
  const value = text ?? (stored.data?.days != null ? String(stored.data.days) : '')
  const days = parseRetentionDays(value)
  const typed = typeof days === 'number' ? days : undefined
  const [settled, setSettled] = useState<number | undefined>(typed)
  useEffect(() => {
    const timer = window.setTimeout(() => setSettled(typed), DRY_RUN_DELAY_MS)
    return () => window.clearTimeout(timer)
  }, [typed])
  const dry = useQuery({
    ...retentionQuery(account.id, settled),
    enabled: settled !== undefined && settled === typed,
  })
  const canDelete = capabilities.active.includes('mail.delete')
  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: ['accounts', account.id, 'retention'] })
  const turnOff = useMutation({
    mutationFn: () => setRetention(account.id, null),
    onSuccess: () => setText(''),
    onSettled: refresh,
  })
  const current = stored.data?.days ?? null
  const checked = typed
  const counted = checked !== undefined && dry.data?.checked_days === checked ? dry.data : undefined
  const dueText =
    counted && checked !== undefined
      ? `${r.due(counted.due, counted.more, checked)} ${counted.expunge ? r.forGood : r.toTrash}`
      : ''

  return (
    <section aria-labelledby={`${account.id}-retention`} className="grid gap-3">
      <div className="grid gap-1">
        <h3 id={`${account.id}-retention`} className="text-sm text-fg">
          {r.title}
        </h3>
        <p className="text-xs text-fg-muted">{r.lead}</p>
        <p className="text-xs text-fg-subtle">
          {stored.isPending
            ? r.loading
            : stored.isError
              ? ''
              : current === null
                ? r.off
                : r.current(current)}
        </p>
        {current !== null && stored.data && !stored.data.active && (
          <p className="text-xs text-warning">{r.inactive}</p>
        )}
      </div>
      <p className="flex items-start gap-3 border-l-2 border-warning bg-bg-subtle px-3 py-2 text-xs text-fg">
        <TriangleAlertIcon aria-hidden className="size-4 shrink-0 text-warning" />
        {r.warning}
      </p>
      <div className="flex flex-wrap items-end gap-2">
        <Field
          label={r.days}
          inputMode="numeric"
          className="w-40"
          value={value}
          disabled={!stored.isSuccess}
          onChange={(event) => setText(event.target.value)}
          aria-invalid={days === undefined}
        />
        <Button
          variant="danger"
          disabled={
            !stored.isSuccess ||
            typeof days !== 'number' ||
            days === current ||
            !canDelete ||
            !counted
          }
          onClick={() => setConfirming(true)}
        >
          {r.save}
        </Button>
        {current !== null && (
          <Button variant="ghost" disabled={turnOff.isPending} onClick={() => turnOff.mutate()}>
            {r.turnOff}
          </Button>
        )}
      </div>
      <p aria-live="polite" className="text-xs text-fg-muted">
        {days === undefined
          ? r.invalid
          : !canDelete
            ? r.needsDelete
            : typeof days === 'number' && !counted && !dry.isError
              ? r.counting
              : dueText}
      </p>
      <FormError message={stored.isError ? authErrorMessage(stored.error) : undefined} />
      <FormError
        message={dry.isError && !counted && canDelete ? authErrorMessage(dry.error) : undefined}
      />
      <FormError message={turnOff.isError ? authErrorMessage(turnOff.error) : undefined} />
      {typeof days === 'number' && (
        <ConfirmDialog
          open={confirming}
          onOpenChange={setConfirming}
          title={r.confirmTitle(days)}
          description={r.confirmLead(dueText)}
          action={r.confirm}
          busyAction={r.confirmBusy}
          onConfirm={async () => {
            const saved = await setRetention(account.id, days)
            queryClient.setQueryData(retentionQuery(account.id).queryKey, saved)
            setText(undefined)
            await refresh()
          }}
        />
      )}
    </section>
  )
}
