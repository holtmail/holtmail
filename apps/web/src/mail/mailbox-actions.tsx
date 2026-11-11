import type { MailChange, MessageDetail } from '@holtmail/api-client'
import {
  Button,
  type ButtonProps,
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  Kbd,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@holtmail/ui'
import {
  ArchiveRestoreIcon,
  CircleAlertIcon,
  ClockIcon,
  FolderIcon,
  FolderInputIcon,
  MailIcon,
  MailOpenIcon,
  StarIcon,
  StarOffIcon,
  Trash2Icon,
} from 'lucide-react'
import { type ReactNode, useEffect, useRef, useState } from 'react'

import type { useMailbox } from '../hooks/use-mailbox.ts'
import { isFlagged, isUnread } from '../lib/flags.ts'
import { isMac } from '../lib/keymap.ts'
import {
  type ChangeLine,
  describeChange,
  type FolderGroup,
  heldLines,
  mayDismiss,
  mayRetry,
  messageActions,
  moveTargets,
  planDelete,
  planToggle,
  restoreTargets,
  shownFlags,
} from '../lib/mailbox.ts'
import { displayStroke } from '../lib/shortcuts.ts'
import { DeviceOrigin } from '../settings/device-origin.tsx'
import { strings } from '../strings.ts'
import { useMail } from './context.ts'
import { folderLabel } from './sidebar.tsx'

const t = strings.writing.mailbox

type Mailbox = ReturnType<typeof useMailbox>

function ActionButton({
  label,
  keys,
  children,
  ...props
}: { label: string; keys?: string; children: ReactNode } & Omit<ButtonProps, 'children'>) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={label} {...props}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>
        {label}
        {keys && <Kbd className="ml-2">{displayStroke(keys, isMac)[0]}</Kbd>}
      </TooltipContent>
    </Tooltip>
  )
}

/** Read, star, move, trash and restore buttons for the open message, each only when an account
 * lets the user make that change; also answers the keyboard shortcuts through the mail context. */
export function MailboxActions({ message, mailbox }: { message: MessageDetail; mailbox: Mailbox }) {
  const { mailbox: controller } = useMail()
  const [picker, setPicker] = useState<'move' | 'restore' | undefined>()
  const { folders, writable, accounts } = mailbox
  const actions = messageActions(message, folders, writable)
  const flags = shownFlags(message, folders)
  const unread = isUnread(flags)
  const flagged = isFlagged(flags)

  const run = (kind: 'read' | 'flagged' | 'trash') => {
    if (mailbox.busy) return
    const plan =
      kind === 'trash'
        ? planDelete(message, folders, writable)
        : planToggle(message, folders, writable, kind)
    if (plan.length > 0) mailbox.apply.mutate({ message, plan })
  }

  const latest = useRef({ actions, run })
  useEffect(() => {
    latest.current = { actions, run }
  })
  useEffect(() => {
    controller.current = {
      toggleRead: () => latest.current.actions.flags && latest.current.run('read'),
      toggleFlag: () => latest.current.actions.flags && latest.current.run('flagged'),
      move: () => latest.current.actions.move && setPicker('move'),
      trash: () => latest.current.actions.delete && latest.current.run('trash'),
    }
    return () => {
      controller.current = null
    }
  }, [controller])

  return (
    <>
      {actions.flags && (
        <>
          <ActionButton
            label={unread ? t.markRead : t.markUnread}
            keys="u"
            disabled={mailbox.busy}
            onClick={() => run('read')}
          >
            {unread ? <MailOpenIcon /> : <MailIcon />}
          </ActionButton>
          <ActionButton
            label={flagged ? t.unstar : t.star}
            keys="s"
            disabled={mailbox.busy}
            onClick={() => run('flagged')}
          >
            {flagged ? <StarOffIcon /> : <StarIcon />}
          </ActionButton>
        </>
      )}
      {actions.move && (
        <ActionButton
          label={t.move}
          keys="v"
          disabled={mailbox.busy}
          onClick={() => setPicker('move')}
        >
          <FolderInputIcon />
        </ActionButton>
      )}
      {actions.delete && (
        <ActionButton label={t.trash} keys="#" disabled={mailbox.busy} onClick={() => run('trash')}>
          <Trash2Icon />
        </ActionButton>
      )}
      {actions.restore && (
        <ActionButton
          label={t.restore}
          disabled={mailbox.busy}
          onClick={() => setPicker('restore')}
        >
          <ArchiveRestoreIcon />
        </ActionButton>
      )}
      <FolderPicker
        open={picker === 'move'}
        onOpenChange={(open) => setPicker(open ? 'move' : undefined)}
        title={t.moveTitle}
        description={t.moveLead}
        groups={moveTargets(message, folders, writable, accounts)}
        onPick={(folder) =>
          mailbox.apply.mutate({
            message,
            plan: [{ account: folder.account_id, op: { kind: 'move', folder: folder.id } }],
          })
        }
      />
      <FolderPicker
        open={picker === 'restore'}
        onOpenChange={(open) => setPicker(open ? 'restore' : undefined)}
        title={t.restoreTitle}
        description={t.restoreLead}
        groups={restoreTargets(folders, accounts, writable)}
        onPick={(folder) => mailbox.restore.mutate({ messageId: message.id, folder: folder.id })}
      />
    </>
  )
}

function FolderPicker({
  open,
  onOpenChange,
  title,
  description,
  groups,
  onPick,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: string
  groups: FolderGroup[]
  onPick: (folder: FolderGroup['folders'][number]) => void
}) {
  return (
    <CommandDialog open={open} onOpenChange={onOpenChange} title={title} description={description}>
      <CommandInput placeholder={t.findFolder} />
      <CommandList>
        <CommandEmpty>{t.noFolders}</CommandEmpty>
        {groups.map((group) => (
          <CommandGroup key={group.accountId} heading={group.name || title}>
            {group.folders.map((folder) => (
              <CommandItem
                key={folder.id}
                value={`${group.accountId} ${folder.id} ${folder.path}`}
                keywords={[folderLabel(folder), folder.role ?? '', group.name]}
                onSelect={() => {
                  onOpenChange(false)
                  onPick(folder)
                }}
              >
                <FolderIcon />
                <span className="truncate">{folder.path}</span>
              </CommandItem>
            ))}
          </CommandGroup>
        ))}
      </CommandList>
    </CommandDialog>
  )
}

function lineText(line: ChangeLine): string {
  return t.changes[line.kind](line.folder ?? '')
}

export function errorText(code: string | null | undefined): string {
  return (code && t.errors[code]) || t.unknownError
}

/** The held mark under the subject: changes not yet on the server, failed ones with Retry and
 * Dismiss, and the last request that could not be recorded. */
export function HeldMark({
  message,
  mailbox,
  managesInstance,
}: {
  message: MessageDetail
  mailbox: Mailbox
  managesInstance: boolean
}) {
  const held = heldLines(message.pending, mailbox.folders)
  const failed: MailChange[] = message.failed
  if (held.length === 0 && failed.length === 0 && !mailbox.error) return null
  return (
    <div className="mt-3 grid gap-2" aria-live="polite">
      {held.length > 0 && (
        <p
          className="flex items-start gap-2 border-l-2 border-border-strong bg-bg-subtle px-3 py-2 text-sm text-fg-muted"
          title={t.heldHint}
        >
          <ClockIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
          <span>
            <span className="text-fg">{t.held}</span>
            {': '}
            {held.map(lineText).join(', ')}
          </span>
        </p>
      )}
      {failed.map((change) => {
        const line = describeChange(change, mailbox.folders)
        const retry = mayRetry(change, mailbox.folders, mailbox.writable)
        const dismiss = mayDismiss(change, mailbox.writable, managesInstance)
        return (
          <div
            key={change.id}
            role="alert"
            className="flex flex-wrap items-center gap-x-3 gap-y-1 border-l-2 border-danger bg-bg-subtle px-3 py-2 text-sm"
          >
            <CircleAlertIcon aria-hidden className="size-4 shrink-0 text-danger" />
            <span className="min-w-0 flex-1 text-fg">
              {t.failedTitle}: {lineText(line)} — {errorText(line.error)}
              {change.device_id != null && (
                <span className="block text-xs text-fg-muted">
                  <DeviceOrigin deviceId={change.device_id} />
                </span>
              )}
            </span>
            {(retry || dismiss) && (
              <span className="flex gap-1">
                {retry && (
                  <Button
                    variant="secondary"
                    size="sm"
                    disabled={mailbox.busy}
                    onClick={() =>
                      mailbox.retry.mutate({ changeId: change.id, messageId: message.id })
                    }
                  >
                    {t.retry}
                  </Button>
                )}
                {dismiss && (
                  <Button
                    variant="ghost"
                    size="sm"
                    disabled={mailbox.busy}
                    onClick={() =>
                      mailbox.dismiss.mutate({ changeId: change.id, messageId: message.id })
                    }
                  >
                    {t.dismiss}
                  </Button>
                )}
              </span>
            )}
          </div>
        )
      })}
      {mailbox.error && (
        <p role="alert" className="text-sm text-danger">
          {t.actionFailed(mailbox.error)}
        </p>
      )}
    </div>
  )
}
