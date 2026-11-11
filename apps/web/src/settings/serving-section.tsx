import {
  type Archive,
  dropLocalFlags,
  localFlagsQuery,
  type LocalFlagged,
  type ServingArchive,
  servingQuery,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { buildState, type FlagWord, sortedFlagWords } from '../lib/devices.ts'
import { formatCount, formatFullDate } from '../lib/format.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'

const t = strings.devices.serving

const flagText = (word: FlagWord) =>
  word.system
    ? t.flags[word.system][word.present ? 'on' : 'off']
    : t.keyword(word.name, word.present)

function BuildProgress({ archive, name }: { archive: ServingArchive; name: string }) {
  const state = buildState(archive)
  if (!state.building) return <p className="text-sm text-fg-muted">{t.ready(archive.mailboxes)}</p>
  return (
    <div className="grid gap-2">
      <div
        role="progressbar"
        aria-label={t.progressLabel(name)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={state.percent}
        className="relative h-1 w-full overflow-hidden bg-bg-selected"
      >
        <span
          className="absolute inset-0 origin-left bg-accent transition-transform duration-moderate-01 ease-productive"
          style={{ transform: `scaleX(${state.percent / 100})` }}
        />
      </div>
      <p className="text-sm text-fg-muted">
        {t.building(state.percent, formatCount(archive.assigned), formatCount(archive.total))}
      </p>
    </div>
  )
}

function FlaggedRow({ archiveId, message }: { archiveId: string; message: LocalFlagged }) {
  const queryClient = useQueryClient()
  const clear = useMutation({
    mutationFn: () => dropLocalFlags(archiveId, message.message_id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ['devices'] }),
  })
  const subject = message.subject?.trim() || t.noSubject
  const from = message.from[0]
  return (
    <li className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border py-2">
      <div className="grid min-w-0 flex-1 gap-0.5">
        <span className="truncate text-sm text-fg">{subject}</span>
        <span className="truncate text-xs text-fg-subtle">
          {[from?.name || from?.email, formatFullDate(message.date)].filter(Boolean).join(' · ')}
        </span>
      </div>
      <div className="flex flex-wrap gap-1">
        {sortedFlagWords(message.flags).map((word) => (
          <Badge key={word.name} variant="outline">
            {flagText(word)}
          </Badge>
        ))}
      </div>
      <Button
        size="sm"
        variant="ghost"
        aria-label={t.clearLabel(subject)}
        disabled={clear.isPending}
        onClick={() => clear.mutate()}
      >
        {t.clear}
      </Button>
      {clear.isError && (
        <p role="alert" className="w-full text-xs text-danger">
          {t.clearFailed}
        </p>
      )}
    </li>
  )
}

function LocalFlagList({ archiveId }: { archiveId: string }) {
  const list = useQuery(localFlagsQuery(archiveId))
  if (list.isPending) return <Skeleton className="h-16 w-full" />
  if (list.isError)
    return (
      <p role="alert" className="text-sm text-danger">
        {t.listFailed}
      </p>
    )
  const { messages, total } = list.data
  return (
    <div className="grid gap-2">
      <ul className="grid border-t border-border">
        {messages.map((message) => (
          <FlaggedRow key={message.message_id} archiveId={archiveId} message={message} />
        ))}
      </ul>
      {messages.length < total && (
        <p className="text-xs text-fg-subtle">{t.shown(messages.length, total)}</p>
      )}
    </div>
  )
}

function ArchiveServing({
  archive,
  name,
  enabled,
}: {
  archive: ServingArchive
  name: string
  enabled: boolean
}) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [confirming, setConfirming] = useState(false)
  return (
    <li className="grid gap-3 border border-border px-4 py-4">
      <span className="truncate text-base text-fg">{name}</span>
      {archive.mailboxes > 0 && <BuildProgress archive={archive} name={name} />}
      {archive.local_flagged > 0 && (
        <div className="grid gap-3">
          <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
            <p className="min-w-0 flex-1 text-sm">{t.localCount(archive.local_flagged)}</p>
            <Button size="sm" variant="ghost" onClick={() => setOpen((now) => !now)}>
              {open ? t.hide : t.show}
            </Button>
            <Button size="sm" onClick={() => setConfirming(true)}>
              {t.clearAll}
            </Button>
          </div>
          {!enabled && <p className="text-xs text-fg-subtle">{t.localOff}</p>}
          {open && <LocalFlagList archiveId={archive.archive_id} />}
        </div>
      )}
      <ConfirmDialog
        open={confirming}
        onOpenChange={setConfirming}
        title={t.confirmTitle(name)}
        description={t.confirmLead}
        action={t.confirmAction}
        busyAction={t.confirmBusy}
        onConfirm={async () => {
          await dropLocalFlags(archive.archive_id)
          await queryClient.invalidateQueries({ queryKey: ['devices'] })
        }}
      />
    </li>
  )
}

export function ServingSection({
  archives,
  unknownArchive,
}: {
  archives: readonly Archive[]
  unknownArchive: string
}) {
  const serving = useQuery(servingQuery)
  if (serving.isError)
    return (
      <p role="alert" className="text-sm text-danger">
        {t.loadFailed}
      </p>
    )
  if (!serving.data || serving.data.archives.length === 0) return null
  const name = (id: string) => archives.find((archive) => archive.id === id)?.name ?? unknownArchive
  const anyLocal = serving.data.archives.some((archive) => archive.local_flagged > 0)
  return (
    <section className="grid gap-3" aria-labelledby="device-serving">
      <h3 id="device-serving" className="text-base font-normal">
        {t.heading}
      </h3>
      <p className="text-sm text-fg-muted">{anyLocal ? t.localLead : t.lead}</p>
      <ul className="grid gap-3">
        {serving.data.archives.map((archive) => (
          <ArchiveServing
            key={archive.archive_id}
            archive={archive}
            name={name(archive.archive_id)}
            enabled={serving.data.local_flags}
          />
        ))}
      </ul>
    </section>
  )
}
