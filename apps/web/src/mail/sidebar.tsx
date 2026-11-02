import {
  accountsQuery,
  type Archive,
  archivesQuery,
  type Folder,
  foldersQuery,
} from '@holtmail/api-client'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
  Skeleton,
} from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import {
  ArchiveIcon,
  ChevronsUpDownIcon,
  FilePenIcon,
  FolderIcon,
  InboxIcon,
  type LucideIcon,
  MailIcon,
  SendIcon,
  ShieldAlertIcon,
  StarIcon,
  Trash2Icon,
} from 'lucide-react'
import { useMemo } from 'react'

import { groupFolders } from '../lib/folders.ts'
import { formatCount } from '../lib/format.ts'
import { strings } from '../strings.ts'
import { useMail } from './context.ts'

const roleIcons: Record<string, LucideIcon> = {
  inbox: InboxIcon,
  sent: SendIcon,
  drafts: FilePenIcon,
  trash: Trash2Icon,
  junk: ShieldAlertIcon,
  archive: ArchiveIcon,
  all: MailIcon,
  flagged: StarIcon,
  important: StarIcon,
}

export function folderIcon(folder: Pick<Folder, 'role'>): LucideIcon {
  return (folder.role && roleIcons[folder.role]) || FolderIcon
}

export function folderLabel(folder: Pick<Folder, 'role' | 'name' | 'path'>): string {
  if (folder.role === 'inbox' && folder.path.toUpperCase() === 'INBOX') {
    return strings.folders.roles.inbox ?? folder.name
  }
  return folder.name
}

const skeletonWidths = ['w-3/4', 'w-1/2', 'w-2/3', 'w-3/5', 'w-4/5', 'w-1/2']

const itemClass = cn(
  'group relative flex h-8 items-center gap-3 pr-3 text-sm text-fg-muted outline-none select-none',
  'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover hover:text-fg',
  'focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
  'aria-[current=page]:bg-bg-selected aria-[current=page]:text-fg',
  '[&_svg]:size-4 [&_svg]:shrink-0',
)

function Count({ value, unread = 0 }: { value: number; unread?: number }) {
  if (unread <= 0) {
    return (
      <span className="ml-auto font-mono text-xs text-fg-subtle tabular-nums">
        {formatCount(value)}
      </span>
    )
  }
  return (
    <span className="ml-auto flex items-baseline gap-2 font-mono text-xs tabular-nums">
      <span className="sr-only">
        {strings.folders.counts(formatCount(unread), formatCount(value))}
      </span>
      <span aria-hidden className="font-medium text-fg">
        {formatCount(unread)}
      </span>
      <span aria-hidden className="text-fg-subtle">
        {formatCount(value)}
      </span>
    </span>
  )
}

export function Sidebar({
  folderId,
  isAllMail,
  onNavigate,
}: {
  folderId?: string
  isAllMail: boolean
  onNavigate?: () => void
}) {
  const { archiveId } = useMail()
  const archives = useQuery(archivesQuery)
  const folders = useQuery(foldersQuery(archiveId))
  const accounts = useQuery(accountsQuery(archiveId))
  const archive = archives.data?.find((item) => item.id === archiveId)
  const groups = useMemo(
    () => groupFolders(folders.data ?? [], accounts.data ?? []),
    [folders.data, accounts.data],
  )

  return (
    <div className="flex h-full flex-col">
      <ArchiveSwitcher archive={archive} archives={archives.data ?? []} onNavigate={onNavigate} />
      <div className="min-h-0 flex-1 overflow-y-auto pb-4">
        <Link
          to="/a/$archiveId"
          params={{ archiveId }}
          aria-current={isAllMail ? 'page' : undefined}
          className={cn(itemClass, 'mt-2 pl-4')}
          onClick={onNavigate}
        >
          <MailIcon />
          <span className="truncate">{strings.archive.allMail}</span>
          {archive && <Count value={archive.stats.messages} />}
        </Link>

        {folders.isPending && (
          <div className="mt-4 grid gap-3 px-4" aria-label={strings.folders.loading}>
            {skeletonWidths.map((width, i) => (
              <Skeleton key={i} className={cn('h-4', width)} />
            ))}
          </div>
        )}
        {folders.isError && (
          <p className="mt-4 px-4 text-sm text-danger">{strings.folders.loadFailed}</p>
        )}

        {groups.map((group) => (
          <section key={group.accountId} aria-label={group.name} className="mt-4">
            <h2 className="flex h-8 items-center gap-2 px-4 text-xs text-fg-subtle">
              <span className="truncate">{group.name}</span>
              {group.address && group.address !== group.name && (
                <span className="truncate font-mono">{group.address}</span>
              )}
            </h2>
            <ul>
              {group.folders.map(({ folder, depth }) => {
                const Icon = folderIcon(folder)
                return (
                  <li key={folder.id}>
                    <Link
                      to="/a/$archiveId/f/$folderId"
                      params={{ archiveId, folderId: folder.id }}
                      aria-current={folder.id === folderId ? 'page' : undefined}
                      className={itemClass}
                      style={{ paddingLeft: `calc(var(--spacing) * ${4 + depth * 4})` }}
                      title={folder.path}
                      onClick={onNavigate}
                    >
                      <Icon />
                      <span className="truncate">{folderLabel(folder)}</span>
                      <Count value={folder.messages} unread={folder.unread} />
                    </Link>
                  </li>
                )
              })}
            </ul>
          </section>
        ))}
      </div>
    </div>
  )
}

function ArchiveSwitcher({
  archive,
  archives,
  onNavigate,
}: {
  archive: Archive | undefined
  archives: Archive[]
  onNavigate?: () => void
}) {
  const navigate = useNavigate()
  return (
    <div className="border-b border-border p-2">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            className="h-12 w-full justify-start gap-3 px-2 text-left"
            aria-label={strings.archive.switch}
          >
            <span
              aria-hidden
              className="flex size-8 shrink-0 items-center justify-center bg-accent text-sm font-semibold text-accent-fg"
            >
              {archive?.name.slice(0, 1).toUpperCase() ?? ''}
            </span>
            <span className="grid min-w-0 flex-1">
              {archive ? (
                <>
                  <span className="truncate text-sm font-medium">{archive.name}</span>
                  <span className="truncate text-xs text-fg-subtle">
                    {strings.archive.messages(formatCount(archive.stats.messages))}
                  </span>
                </>
              ) : (
                <Skeleton className="h-4 w-24" />
              )}
            </span>
            <ChevronsUpDownIcon className="text-fg-subtle" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent className="w-(--radix-dropdown-menu-trigger-width)">
          <DropdownMenuLabel>{strings.command.archives}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={archive?.id}
            onValueChange={(id) => {
              onNavigate?.()
              void navigate({ to: '/a/$archiveId', params: { archiveId: id } })
            }}
          >
            {archives.map((item) => (
              <DropdownMenuRadioItem key={item.id} value={item.id}>
                <span className="truncate">{item.name}</span>
                <span className="ml-auto pl-4 font-mono text-xs text-fg-subtle tabular-nums">
                  {formatCount(item.stats.messages)}
                </span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}
