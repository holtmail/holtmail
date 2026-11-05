import {
  type Archive,
  archivesQuery,
  cancelTask,
  createArchive,
  deleteArchive,
  type RemoteImageJob,
  setSyncSchedule,
  startRemoteImages,
  syncScheduleQuery,
  tasksQuery,
  updateArchive,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Skeleton,
  Switch,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import {
  ArrowUpRightIcon,
  ChevronDownIcon,
  ClockIcon,
  DownloadIcon,
  EllipsisIcon,
  FileUpIcon,
  ImageIcon,
  MailPlusIcon,
  PencilIcon,
  PlusIcon,
  RotateCwIcon,
  SquareIcon,
  Trash2Icon,
  TriangleAlertIcon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useEffect, useRef, useState } from 'react'

import { useLiveArchive } from '../hooks/use-live-archive.ts'
import { useOAuthReturn } from '../hooks/use-oauth-return.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { formatCount, formatSize } from '../lib/format.ts'
import { formatInterval, intervalChoices } from '../lib/schedule.ts'
import type { AddMail } from '../lib/search-params.ts'
import { remoteImagesTask } from '../lib/tasks.ts'
import { ExportSubmenu } from '../mail/export-menu.tsx'
import { AccountList } from '../manage/account-list.tsx'
import { ConfirmDialog, TextDialog } from '../manage/confirm-dialog.tsx'
import { ConnectMailbox } from '../manage/connect-mailbox.tsx'
import { ImportFiles } from '../manage/import-files.tsx'
import { OAuthReturnNote } from '../manage/oauth-sign-in.tsx'
import { strings } from '../strings.ts'
import { TaskProgress } from '../tasks/task-progress.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

export function ArchivesPage() {
  const archives = useQuery(archivesQuery)
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [creating, setCreating] = useState(false)
  const [signedIn, dismissSignedIn] = useOAuthReturn()

  return (
    <SettingsPage
      title={t.archives}
      lead={t.archivesLead}
      action={
        <Button variant="secondary" onClick={() => setCreating(true)}>
          <PlusIcon />
          {t.newArchive}
        </Button>
      }
    >
      {signedIn && <OAuthReturnNote result={signedIn} onDismiss={dismissSignedIn} />}
      <SyncScheduleCard />
      {archives.isPending && <Skeleton className="h-48" />}
      <div className="grid gap-12">
        <AnimatePresence initial={false}>
          {archives.data?.map((archive) => (
            <m.div
              key={archive.id}
              layout
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
              exit={{ opacity: 0, transition: transition.productiveExit.moderate01 }}
            >
              <ArchiveSection archive={archive} />
            </m.div>
          ))}
        </AnimatePresence>
      </div>
      <TextDialog
        open={creating}
        onOpenChange={setCreating}
        title={t.newArchiveTitle}
        description={strings.welcome.archiveHint}
        label={strings.welcome.archiveName}
        onSave={async (name) => {
          const archive = await createArchive(name)
          await queryClient.invalidateQueries({ queryKey: ['archives'] })
          await navigate({
            to: '/settings/archives',
            search: { archive: archive.id, add: 'connect' },
          })
        }}
      />
    </SettingsPage>
  )
}

function ArchiveSection({ archive }: { archive: Archive }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const search = useSearch({ from: '/settings/archives' })
  const [dialog, setDialog] = useState<'rename' | 'delete' | null>(null)
  const adding: AddMail | undefined = search.archive === archive.id ? search.add : undefined
  useLiveArchive(archive.id)

  const setAdding = (add: AddMail | undefined) =>
    void navigate({
      to: '/settings/archives',
      search: add ? { archive: archive.id, add } : {},
      replace: true,
    })

  const refresh = () => queryClient.invalidateQueries({ queryKey: ['archives'] })

  return (
    <section aria-labelledby={`archive-${archive.id}`} className="grid gap-6">
      <div className="flex items-center gap-4">
        <span
          aria-hidden
          className="flex size-10 shrink-0 items-center justify-center bg-accent text-base font-semibold text-accent-fg"
        >
          {archive.name.slice(0, 1).toUpperCase()}
        </span>
        <div className="grid min-w-0 flex-1">
          <h3 id={`archive-${archive.id}`} className="truncate text-lg font-normal">
            {archive.name}
          </h3>
          <span className="font-mono text-xs text-fg-subtle tabular-nums">
            {t.stats(formatCount(archive.stats.messages), formatSize(archive.stats.bytes))}
          </span>
        </div>
        <Button variant="ghost" size="icon" asChild aria-label={strings.welcome.open}>
          <Link to="/a/$archiveId" params={{ archiveId: archive.id }} title={strings.welcome.open}>
            <ArrowUpRightIcon />
          </Link>
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" aria-label={t.archiveActions(archive.name)}>
              <EllipsisIcon />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-56">
            <DropdownMenuItem onSelect={() => setDialog('rename')}>
              <PencilIcon />
              {t.renameArchive}
            </DropdownMenuItem>
            {archive.stats.messages > 0 && <ExportSubmenu archiveId={archive.id} />}
            <DropdownMenuSeparator />
            <DropdownMenuItem className="text-danger" onSelect={() => setDialog('delete')}>
              <Trash2Icon />
              {t.deleteArchive}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>

      <div className="grid gap-3">
        <h4 className="text-xs text-fg-subtle">{t.sources}</h4>
        <AccountList archiveId={archive.id} manage />
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" onClick={() => setAdding('connect')}>
            <MailPlusIcon />
            {strings.connect.title}
          </Button>
          <Button variant="ghost" onClick={() => setAdding('import')}>
            <FileUpIcon />
            {strings.importFiles.title}
          </Button>
        </div>
      </div>

      <SaveImages archive={archive} />

      <Dialog open={adding !== undefined} onOpenChange={(open) => !open && setAdding(undefined)}>
        <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto">
          <DialogHeader>
            <DialogTitle>
              {adding === 'import' ? strings.importFiles.title : strings.connect.title}
            </DialogTitle>
            <DialogDescription>
              {adding === 'import' ? strings.importFiles.lead : strings.connect.lead}
            </DialogDescription>
          </DialogHeader>
          {adding === 'import' ? (
            <ImportFiles archiveId={archive.id} />
          ) : (
            <ConnectMailbox
              archiveId={archive.id}
              onConnected={() => setAdding(undefined)}
              onCancel={() => setAdding(undefined)}
            />
          )}
        </DialogContent>
      </Dialog>
      <TextDialog
        open={dialog === 'rename'}
        onOpenChange={(open) => setDialog(open ? 'rename' : null)}
        title={t.renameArchive}
        label={strings.welcome.archiveName}
        initial={archive.name}
        onSave={async (name) => {
          await updateArchive(archive.id, { name })
          await refresh()
        }}
      />
      <ConfirmDialog
        open={dialog === 'delete'}
        onOpenChange={(open) => setDialog(open ? 'delete' : null)}
        title={t.deleteTitle(archive.name)}
        description={t.deleteLead}
        typeToConfirm={archive.name}
        confirmLabel={t.deleteConfirm(archive.name)}
        action={t.delete}
        busyAction={t.deleting}
        onConfirm={async () => {
          await deleteArchive(archive.id)
          queryClient.removeQueries({ queryKey: ['archives', archive.id] })
          await refresh()
        }}
      />
    </section>
  )
}

function SaveImages({ archive }: { archive: Archive }) {
  const queryClient = useQueryClient()
  const id = `save-images-${archive.id}`
  const mutation = useMutation({
    mutationFn: (save: boolean) => updateArchive(archive.id, { save_remote_images: save }),
    onSuccess: (updated) =>
      queryClient.setQueryData(archivesQuery.queryKey, (list) =>
        list?.map((item) => (item.id === updated.id ? updated : item)),
      ),
  })
  const checked = mutation.isPending ? mutation.variables : archive.save_remote_images
  return (
    <div className="grid gap-4 border-t border-border pt-6">
      <div className="flex items-start gap-4">
        <div className="grid flex-1 gap-1">
          <label htmlFor={id} className="text-sm text-fg">
            {t.saveImages}
          </label>
          <p className="text-xs text-fg-muted">{t.saveImagesHint}</p>
          <AnimatePresence initial={false}>
            {checked && (
              <m.p
                key="warning"
                className="flex items-start gap-2 overflow-hidden text-xs text-fg-muted"
                initial={{ opacity: 0, height: 0 }}
                animate={{
                  opacity: 1,
                  height: 'auto',
                  transition: transition.expressive.moderate02,
                }}
                exit={{ opacity: 0, height: 0, transition: transition.productiveExit.fast02 }}
              >
                <TriangleAlertIcon aria-hidden className="mt-px size-3.5 shrink-0 text-warning" />
                {t.saveImagesWarning}
              </m.p>
            )}
          </AnimatePresence>
          {mutation.isError && (
            <p role="alert" className="text-xs text-danger">
              {authErrorMessage(mutation.error)}
            </p>
          )}
        </div>
        <Switch
          id={id}
          checked={checked}
          disabled={mutation.isPending}
          onCheckedChange={(save) => mutation.mutate(save)}
        />
      </div>
      {archive.save_remote_images && <SavedImages archive={archive} />}
    </div>
  )
}

function SavedImages({ archive }: { archive: Archive }) {
  const queryClient = useQueryClient()
  const tasks = useQuery(tasksQuery)
  const task = remoteImagesTask(tasks.data, archive.id)
  const wasRunning = useRef(Boolean(task))
  const { images, bytes, failed, older_pending: older } = archive.remote_images
  const refresh = () => queryClient.invalidateQueries({ queryKey: ['archives'] })

  useEffect(() => {
    if (wasRunning.current && !task) void queryClient.invalidateQueries({ queryKey: ['archives'] })
    wasRunning.current = Boolean(task)
  }, [task, queryClient])

  const start = useMutation({
    mutationFn: (job: RemoteImageJob) => startRemoteImages(archive.id, job),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['tasks'] })
      await refresh()
    },
  })
  const cancel = useMutation({
    mutationFn: (taskId: string) => cancelTask(taskId),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['tasks'] }),
  })

  return (
    <div className="grid gap-3 border border-border bg-bg-subtle px-4 py-3 text-sm">
      <p className="flex flex-wrap items-center gap-x-3 gap-y-1 font-mono text-xs text-fg-muted tabular-nums">
        <ImageIcon aria-hidden className="size-4 text-fg-subtle" />
        {t.savedImages(formatCount(images), formatSize(bytes))}
        {failed > 0 && <span className="text-warning">{t.failedImages(formatCount(failed))}</span>}
      </p>
      <AnimatePresence initial={false} mode="popLayout">
        {task ? (
          <m.div
            key="task"
            className="grid gap-2"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1, transition: transition.productive.moderate01 }}
            exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
          >
            <TaskProgress task={task} />
            <div className="flex flex-wrap items-center gap-2">
              <span className="flex-1 truncate text-xs text-fg-muted tabular-nums">
                {task.summary}
              </span>
              <Button variant="ghost" size="sm" asChild>
                <Link to="/settings/tasks">{t.openTasks}</Link>
              </Button>
              {task.cancellable && (
                <Button
                  variant="ghost"
                  size="sm"
                  disabled={cancel.isPending}
                  onClick={() => cancel.mutate(task.id)}
                >
                  <SquareIcon />
                  {t.cancel}
                </Button>
              )}
            </div>
          </m.div>
        ) : (
          (older > 0 || failed > 0) && (
            <m.div
              key="actions"
              className="grid gap-2"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1, transition: transition.productive.moderate01 }}
              exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
            >
              {older > 0 && (
                <p className="text-xs text-fg-muted">{t.olderPending(formatCount(older))}</p>
              )}
              <div className="flex flex-wrap gap-2">
                {older > 0 && (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={start.isPending}
                    onClick={() => start.mutate('older')}
                  >
                    <DownloadIcon />
                    {t.saveOlder}
                  </Button>
                )}
                {failed > 0 && (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={start.isPending}
                    onClick={() => start.mutate('failed')}
                  >
                    <RotateCwIcon />
                    {t.retryFailed}
                  </Button>
                )}
              </div>
            </m.div>
          )
        )}
      </AnimatePresence>
      {(start.isError || cancel.isError) && (
        <p role="alert" className="text-xs text-danger">
          {authErrorMessage(start.error ?? cancel.error)}
        </p>
      )}
    </div>
  )
}

function SyncScheduleCard() {
  const queryClient = useQueryClient()
  const schedule = useQuery(syncScheduleQuery)
  const mutation = useMutation({
    mutationFn: (minutes: number) => setSyncSchedule(minutes),
    onSuccess: async (updated) => {
      queryClient.setQueryData(syncScheduleQuery.queryKey, updated)
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
    },
  })
  const minutes = mutation.isPending ? mutation.variables : schedule.data?.minutes
  const fromConfig = schedule.data?.from_config ?? false
  const choices: number[] = [...intervalChoices]
  if (minutes !== undefined && !choices.includes(minutes)) choices.push(minutes)
  return (
    <section
      aria-labelledby="sync-schedule"
      className="flex flex-wrap items-start gap-4 border border-border bg-bg-subtle px-4 py-3"
    >
      <ClockIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-fg-subtle" />
      <div className="grid min-w-0 flex-1 gap-1">
        <h3 id="sync-schedule" className="text-sm text-fg">
          {strings.schedule.title}
        </h3>
        <p className="text-xs text-fg-muted">
          {fromConfig ? strings.schedule.fromConfig : strings.schedule.lead}
        </p>
        {mutation.isError && (
          <p role="alert" className="text-xs text-danger">
            {authErrorMessage(mutation.error)}
          </p>
        )}
      </div>
      {schedule.isPending ? (
        <Skeleton className="h-8 w-48" />
      ) : (
        <DropdownMenu>
          <DropdownMenuTrigger asChild disabled={fromConfig || mutation.isPending}>
            <Button variant="secondary" aria-label={strings.schedule.title}>
              {minutes === undefined ? '' : formatInterval(minutes)}
              <ChevronDownIcon />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-64">
            <DropdownMenuRadioGroup
              value={String(minutes)}
              onValueChange={(value) => mutation.mutate(Number(value))}
            >
              {choices.map((choice) => (
                <DropdownMenuRadioItem key={choice} value={String(choice)}>
                  {formatInterval(choice)}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
    </section>
  )
}
