import {
  type Archive,
  archivesQuery,
  type Collection,
  collectionsQuery,
  type PimChange,
  pimChangesQuery,
  resolvePimChange,
  type PimResolution,
  sessionQuery,
  setWriteMode,
  type WriteMode,
  writeModeQuery,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { BookUserIcon, CalendarIcon } from 'lucide-react'
import { type ReactNode, useState } from 'react'

import { FormError } from '../auth/field.tsx'
import { ChangeDiff } from '../items/change-diff.tsx'
import { NativeSelect } from '../items/inputs.tsx'
import { LocalEditMark } from '../items/local-edit-mark.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import {
  asksToConfirm,
  changeActions,
  decisionCount,
  groupByCollection,
  isWriteMode,
  modeChoices,
  openCounts,
} from '../lib/edits.ts'
import { formatFullDate } from '../lib/format.ts'
import { manages } from '../lib/roles.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { Picker } from './aliases-page.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.edits
const s = t.settings

/** Settings → Contacts and calendars: the write mode of each address book and calendar, and the
 * open changes with their decisions. */
export function EditsPage() {
  const archives = useQuery(archivesQuery)
  const [chosen, setChosen] = useState<string>()
  const list = archives.data ?? []
  const archive = list.find((a) => a.id === chosen) ?? list[0]
  return (
    <SettingsPage
      title={s.nav}
      lead={s.lead}
      action={
        list.length > 1 && archive ? (
          <Picker
            label={s.archive}
            value={archive.id}
            choices={list.map((a) => ({ value: a.id, label: a.name }))}
            onChange={setChosen}
          />
        ) : undefined
      }
    >
      {archives.isPending ? (
        <Skeleton className="h-48" />
      ) : archive ? (
        <ArchiveEdits key={archive.id} archive={archive} />
      ) : (
        <p className="text-sm text-fg-muted">{s.noArchives}</p>
      )}
    </SettingsPage>
  )
}

function Section({ title, lead, children }: { title: string; lead: string; children: ReactNode }) {
  return (
    <section className="grid gap-4">
      <div className="grid gap-1">
        <h3 className="text-lg font-normal">{title}</h3>
        <p className="text-sm text-fg-muted">{lead}</p>
      </div>
      {children}
    </section>
  )
}

function ArchiveEdits({ archive }: { archive: Archive }) {
  const collections = useQuery(collectionsQuery(archive.id))
  const changes = useQuery(pimChangesQuery({ archive: archive.id }))
  if (collections.isError) {
    return <FormError message={`${s.loadFailed}: ${authErrorMessage(collections.error)}`} />
  }
  if (collections.isPending) return <Skeleton className="h-32" />
  const all = collections.data.filter((c) => c.removed_at == null)
  const counts = openCounts(changes.data ?? [])
  return (
    <div className="grid gap-12">
      <Section title={s.modesTitle} lead={s.modesLead}>
        {all.length === 0 ? (
          <p className="text-sm text-fg-muted">{s.none}</p>
        ) : (
          <ul className="border-t border-border">
            {all.map((collection) => (
              <ModeRow
                key={collection.id}
                collection={collection}
                open={counts.get(collection.id) ?? 0}
              />
            ))}
          </ul>
        )}
      </Section>
      <Section title={s.changesTitle} lead={s.changesLead}>
        {changes.isPending ? (
          <Skeleton className="h-24" />
        ) : changes.isError ? (
          <FormError message={authErrorMessage(changes.error)} />
        ) : changes.data.length === 0 ? (
          <p className="text-sm text-fg-muted">{s.noChanges}</p>
        ) : (
          <div className="grid gap-8">
            {groupByCollection(changes.data, collections.data).map((group) => (
              <section key={group.collectionId} className="grid gap-2">
                <h4 className="flex flex-wrap items-baseline gap-x-3 text-sm font-medium">
                  {group.collection ? collectionName(group.collection) : s.unknownCollection}
                  <span className="font-mono text-xs font-normal text-fg-subtle tabular-nums">
                    {s.open(group.changes.length)}
                  </span>
                  {decisionCount(group.changes) > 0 && (
                    <span className="text-xs font-normal text-warning">
                      {s.decide(decisionCount(group.changes))}
                    </span>
                  )}
                </h4>
                <ul className="border-t border-border">
                  {group.changes.map((change) => (
                    <ChangeRow
                      key={change.id}
                      change={change}
                      account={group.collection?.account_name ?? undefined}
                    />
                  ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </Section>
    </div>
  )
}

function collectionName(collection: Collection): string {
  return collection.account_name
    ? `${collection.name} · ${collection.account_name}`
    : collection.name
}

function useRefresh() {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['pim-changes'] }),
      queryClient.invalidateQueries({ queryKey: ['archives'] }),
      queryClient.invalidateQueries({ queryKey: ['collections'] }),
      queryClient.invalidateQueries({ queryKey: ['items'] }),
      queryClient.invalidateQueries({ queryKey: ['people'] }),
    ])
}

function ModeRow({ collection, open }: { collection: Collection; open: number }) {
  const user = useQuery(sessionQuery).data
  const canManage = manages(user?.role)
  const owned = Boolean(collection.account_id)
  const mode = useQuery({ ...writeModeQuery(collection.id), enabled: canManage && owned })
  const choices = modeChoices(collection, mode.data?.allowed_modes)
  const imported =
    canManage && mode.data?.allowed_modes.length === 1 && mode.data.allowed_modes[0] === 'read_only'
  const refresh = useRefresh()
  const [confirming, setConfirming] = useState(false)
  const change = useMutation({
    mutationFn: ({ to, confirm }: { to: WriteMode; confirm: boolean }) =>
      setWriteMode(collection.id, to, confirm),
    onSuccess: refresh,
  })
  const Icon = collection.kind === 'contacts' ? BookUserIcon : CalendarIcon
  const account = collection.account_name ?? t.mark.account
  const pick = (to: WriteMode) => {
    if (to === collection.write_mode) return
    if (asksToConfirm(collection.write_mode, to)) setConfirming(true)
    else change.mutate({ to, confirm: false })
  }
  return (
    <li className="grid gap-2 border-b border-border py-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <Icon aria-hidden className="size-4 shrink-0 text-fg-subtle" />
        <span className="min-w-0 flex-1">
          <span className="block truncate text-sm text-fg">{collection.name}</span>
          <span className="block truncate text-xs text-fg-subtle">
            {collection.account_name ?? s.imported}
            {open > 0 && ` · ${s.open(open)}`}
          </span>
        </span>
        {canManage && choices.length > 1 ? (
          <NativeSelect
            label={s.mode(collection.name)}
            hideLabel
            className="w-60"
            value={collection.write_mode}
            choices={choices.map((m) => ({ value: m, label: s.modes[m] ?? m }))}
            onChange={(value) => {
              if (isWriteMode(value)) pick(value)
            }}
          />
        ) : (
          <Badge variant="outline">{s.modes[collection.write_mode] ?? collection.write_mode}</Badge>
        )}
      </div>
      <p className="text-xs text-fg-subtle">
        {!owned || imported
          ? s.imported
          : (s.modeHints[collection.write_mode] ?? '') +
            (canManage ? '' : ` ${s.managersOnly}`) +
            (canManage &&
            mode.data &&
            !mode.data.may_write_through &&
            collection.write_mode !== 'write_through'
              ? ` ${s.cannotSend}`
              : '')}
      </p>
      <FormError message={change.isError ? authErrorMessage(change.error) : undefined} />
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={s.confirmTitle(account)}
        description={s.confirmBody(mode.data?.open_changes ?? open, account)}
        action={s.confirm}
        busyAction={t.dialog.saving}
        onConfirm={async () => {
          await setWriteMode(collection.id, 'write_through', true)
          await refresh()
        }}
      />
    </li>
  )
}

function ChangeRow({ change, account }: { change: PimChange; account?: string }) {
  const refresh = useRefresh()
  const [open, setOpen] = useState(change.state === 'conflict')
  const resolve = useMutation({
    mutationFn: (action: PimResolution) => resolvePimChange(change.id, action),
    onSuccess: refresh,
  })
  const actions = changeActions(change)
  return (
    <li className="grid gap-2 border-b border-border py-3">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="min-w-0 flex-1">
          {change.item_id ? (
            <Link
              to="/items/$itemId"
              params={{ itemId: change.item_id }}
              className="block truncate text-sm text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            >
              {change.title || strings.list.noSubject}
            </Link>
          ) : (
            <span className="block truncate text-sm text-fg">{s.newItem}</span>
          )}
          <span className="block text-xs text-fg-subtle">
            {t.ops[change.op] ?? change.op} · {t.classes[change.class] ?? change.class} ·{' '}
            <time dateTime={new Date(change.created_at).toISOString()} className="font-mono">
              {formatFullDate(change.created_at)}
            </time>
          </span>
        </span>
        <LocalEditMark edit={{ state: change.state, account_name: account, reason: null }} />
      </div>
      {change.reason && (
        <p className="text-sm text-fg-muted">{t.reasons[change.reason] ?? change.reason}</p>
      )}
      {change.last_error && (
        <p className="text-xs text-fg-subtle">
          {s.lastError(change.last_error)} · {s.tries(change.attempts)}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-2">
        {actions.map((action) => (
          <Button
            key={action}
            size="sm"
            variant={action === actions[0] && change.state !== 'held' ? 'secondary' : 'ghost'}
            disabled={resolve.isPending}
            onClick={() => resolve.mutate(action)}
          >
            {t.actions[action]}
          </Button>
        ))}
        <button
          type="button"
          aria-expanded={open}
          className="ml-auto text-xs text-fg-subtle underline-offset-2 hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
          onClick={() => setOpen((v) => !v)}
        >
          {open ? t.actions.hideDiff : t.actions.showDiff}
        </button>
      </div>
      <FormError
        message={
          resolve.isError ? `${s.resolveFailed}: ${authErrorMessage(resolve.error)}` : undefined
        }
      />
      {open && <ChangeDiff changeId={change.id} />}
    </li>
  )
}
