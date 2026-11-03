import {
  type Archive,
  archivesQuery,
  createArchive,
  deleteArchive,
  renameArchive,
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
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Skeleton,
} from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import {
  ArrowUpRightIcon,
  EllipsisIcon,
  FileUpIcon,
  MailPlusIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useState } from 'react'

import { useLiveArchive } from '../hooks/use-live-archive.ts'
import { formatCount, formatSize } from '../lib/format.ts'
import type { AddMail } from '../lib/search-params.ts'
import { AccountList } from '../manage/account-list.tsx'
import { ConfirmDialog, TextDialog } from '../manage/confirm-dialog.tsx'
import { ConnectImapForm } from '../manage/connect-imap.tsx'
import { ImportFiles } from '../manage/import-files.tsx'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

export function ArchivesPage() {
  const archives = useQuery(archivesQuery)
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [creating, setCreating] = useState(false)

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
            <ConnectImapForm
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
          await renameArchive(archive.id, name)
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
