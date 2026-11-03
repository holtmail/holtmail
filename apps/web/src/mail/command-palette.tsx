import {
  accountsQuery,
  archivesQuery,
  foldersQuery,
  sessionQuery,
  syncAccount,
} from '@holtmail/api-client'
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandSeparator,
  CommandShortcut,
} from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import {
  ArchiveIcon,
  FileUpIcon,
  KeyboardIcon,
  MailIcon,
  MailPlusIcon,
  RefreshCwIcon,
  SearchIcon,
  SettingsIcon,
  SunMoonIcon,
} from 'lucide-react'
import { useMemo, useState } from 'react'

import { orderFolders } from '../lib/folders.ts'
import { isMac } from '../lib/keymap.ts'
import { manages } from '../lib/roles.ts'
import { displayStroke } from '../lib/shortcuts.ts'
import { strings } from '../strings.ts'
import { toggleTheme } from '../theme.ts'
import { useMail } from './context.ts'
import { folderIcon, folderLabel } from './sidebar.tsx'

export function CommandPalette({
  open,
  onOpenChange,
  onShowShortcuts,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  onShowShortcuts: () => void
}) {
  const { archiveId } = useMail()
  const navigate = useNavigate()
  const [search, setSearch] = useState('')
  const archives = useQuery(archivesQuery)
  const folders = useQuery(foldersQuery(archiveId))
  const ordered = useMemo(() => orderFolders(folders.data ?? []), [folders.data])
  const accounts = useQuery(accountsQuery(archiveId))
  const canManage = manages(useQuery(sessionQuery).data?.role)
  const queryClient = useQueryClient()
  const syncable = (accounts.data ?? []).filter((a) => a.kind !== 'import' && a.enabled)

  const syncAll = async () => {
    await Promise.allSettled(syncable.map((account) => syncAccount(account.id)))
    await queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'accounts'] })
  }

  const run = (action: () => void) => {
    onOpenChange(false)
    setSearch('')
    action()
  }

  const q = search.trim()

  return (
    <CommandDialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setSearch('')
        onOpenChange(next)
      }}
      title={strings.command.title}
      description={strings.command.description}
    >
      <CommandInput
        value={search}
        onValueChange={setSearch}
        placeholder={strings.command.placeholder}
      />
      <CommandList>
        <CommandEmpty>{strings.command.empty}</CommandEmpty>
        {q && (
          <CommandGroup forceMount>
            <CommandItem
              forceMount
              value={`search ${q}`}
              onSelect={() =>
                run(
                  () =>
                    void navigate({
                      to: '/a/$archiveId/search',
                      params: { archiveId },
                      search: { q },
                    }),
                )
              }
            >
              <SearchIcon />
              <span className="truncate">{strings.command.searchFor(q)}</span>
              <CommandShortcut>↵</CommandShortcut>
            </CommandItem>
          </CommandGroup>
        )}
        <CommandGroup heading={strings.command.folders}>
          <CommandItem
            value={`folder all ${strings.archive.allMail}`}
            onSelect={() =>
              run(() => void navigate({ to: '/a/$archiveId', params: { archiveId } }))
            }
          >
            <MailIcon />
            {strings.archive.allMail}
            <CommandShortcut>{displayStroke('g a', isMac).join(' ')}</CommandShortcut>
          </CommandItem>
          {ordered.map(({ folder }) => {
            const Icon = folderIcon(folder)
            return (
              <CommandItem
                key={folder.id}
                value={`folder ${folder.id} ${folder.path}`}
                keywords={[folderLabel(folder), folder.role ?? '']}
                onSelect={() =>
                  run(
                    () =>
                      void navigate({
                        to: '/a/$archiveId/f/$folderId',
                        params: { archiveId, folderId: folder.id },
                      }),
                  )
                }
              >
                <Icon />
                <span className="truncate">{folder.path}</span>
              </CommandItem>
            )
          })}
        </CommandGroup>
        {(archives.data?.length ?? 0) > 1 && (
          <CommandGroup heading={strings.command.archives}>
            {archives.data?.map((archive) => (
              <CommandItem
                key={archive.id}
                value={`archive ${archive.id} ${archive.name}`}
                onSelect={() =>
                  run(
                    () => void navigate({ to: '/a/$archiveId', params: { archiveId: archive.id } }),
                  )
                }
              >
                <ArchiveIcon />
                {archive.name}
              </CommandItem>
            ))}
          </CommandGroup>
        )}
        <CommandSeparator />
        <CommandGroup heading={strings.command.actions}>
          <CommandItem value="action theme" onSelect={() => run(toggleTheme)}>
            <SunMoonIcon />
            {strings.command.toggleTheme}
          </CommandItem>
          <CommandItem
            value="action settings"
            onSelect={() => run(() => void navigate({ to: '/settings' }))}
          >
            <SettingsIcon />
            {strings.command.settings}
          </CommandItem>
          {canManage && (
            <>
              <CommandItem
                value="action connect mailbox imap"
                onSelect={() =>
                  run(
                    () =>
                      void navigate({
                        to: '/settings/archives',
                        search: { archive: archiveId, add: 'connect' },
                      }),
                  )
                }
              >
                <MailPlusIcon />
                {strings.command.connect}
              </CommandItem>
              <CommandItem
                value="action import files mbox eml zip"
                onSelect={() =>
                  run(
                    () =>
                      void navigate({
                        to: '/settings/archives',
                        search: { archive: archiveId, add: 'import' },
                      }),
                  )
                }
              >
                <FileUpIcon />
                {strings.command.importFiles}
              </CommandItem>
              {syncable.length > 0 && (
                <CommandItem value="action sync now" onSelect={() => run(() => void syncAll())}>
                  <RefreshCwIcon />
                  {strings.command.syncAll}
                </CommandItem>
              )}
            </>
          )}
          <CommandItem value="action shortcuts" onSelect={() => run(onShowShortcuts)}>
            <KeyboardIcon />
            {strings.command.showShortcuts}
            <CommandShortcut>?</CommandShortcut>
          </CommandItem>
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  )
}
