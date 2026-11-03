import {
  type Account,
  accountsQuery,
  deleteAccount,
  syncAccount,
  updateAccount,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Skeleton,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  EllipsisIcon,
  FileArchiveIcon,
  KeyRoundIcon,
  MailIcon,
  PauseIcon,
  PencilIcon,
  PlayIcon,
  RefreshCwIcon,
  Trash2Icon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { type AccountTone, accountState } from '../lib/accounts.ts'
import { strings } from '../strings.ts'
import { ConfirmDialog, TextDialog } from './confirm-dialog.tsx'

const t = strings.accounts

const toneClass: Record<AccountTone, string> = {
  running: 'bg-accent',
  error: 'bg-danger',
  paused: 'bg-fg-subtle',
  done: 'bg-success',
  waiting: 'bg-border-strong',
}

export function StatusDot({ tone }: { tone: AccountTone }) {
  return (
    <span aria-hidden className="relative flex size-2 shrink-0">
      {tone === 'running' && (
        <m.span
          className="absolute inset-0 bg-accent"
          animate={{ scale: [1, 2.4], opacity: [0.6, 0] }}
          transition={{ duration: 1.4, repeat: Infinity, ease: 'easeOut' }}
        />
      )}
      <span className={cn('relative size-2', toneClass[tone])} />
    </span>
  )
}

export function AccountList({ archiveId, manage }: { archiveId: string; manage: boolean }) {
  const accounts = useQuery(accountsQuery(archiveId))
  if (accounts.isPending) {
    return (
      <div className="grid gap-2">
        <Skeleton className="h-14" />
        <Skeleton className="h-14" />
      </div>
    )
  }
  const list = accounts.data ?? []
  if (list.length === 0) return <p className="py-3 text-sm text-fg-subtle">{t.empty}</p>
  return (
    <ul className="grid divide-y divide-border border-y border-border">
      <AnimatePresence initial={false}>
        {list.map((account) => (
          <m.li
            key={account.id}
            layout
            initial={{ opacity: 0 }}
            animate={{ opacity: 1, transition: transition.productive.moderate01 }}
            exit={{ opacity: 0, height: 0, transition: transition.productiveExit.moderate01 }}
          >
            <AccountRow account={account} archiveId={archiveId} manage={manage} />
          </m.li>
        ))}
      </AnimatePresence>
    </ul>
  )
}

function AccountRow({
  account,
  archiveId,
  manage,
}: {
  account: Account
  archiveId: string
  manage: boolean
}) {
  const now = useNow(30_000)
  const queryClient = useQueryClient()
  const [dialog, setDialog] = useState<'rename' | 'password' | 'remove' | null>(null)
  const state = accountState(account, now)
  const imported = account.kind === 'import'
  const Icon = imported ? FileArchiveIcon : MailIcon

  const refresh = () => queryClient.invalidateQueries({ queryKey: ['archives'] })
  const sync = useMutation({ mutationFn: () => syncAccount(account.id), onSettled: refresh })
  const toggle = useMutation({
    mutationFn: () => updateAccount(account.id, { enabled: !account.enabled }),
    onSettled: refresh,
  })

  return (
    <div className="flex min-h-14 items-center gap-4 py-3">
      <span
        aria-hidden
        className="flex size-8 shrink-0 items-center justify-center bg-bg-subtle text-fg-muted [&_svg]:size-4"
      >
        <Icon />
      </span>
      <div className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <span className="truncate text-sm text-fg">{account.name}</span>
          {account.address && account.address !== account.name && (
            <span className="truncate font-mono text-xs text-fg-subtle">{account.address}</span>
          )}
        </span>
        <span className="flex min-w-0 items-center gap-2 text-xs">
          <StatusDot tone={state.tone} />
          <span
            className={cn('truncate', state.tone === 'error' ? 'text-danger' : 'text-fg-muted')}
          >
            {state.label}
          </span>
        </span>
      </div>
      {manage && (
        <>
          {!imported && (
            <Button
              variant="ghost"
              size="icon"
              aria-label={t.syncNow}
              title={t.syncNow}
              disabled={account.status.running || sync.isPending}
              onClick={() => sync.mutate()}
            >
              <RefreshCwIcon className={cn(account.status.running && 'animate-spin')} />
            </Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={t.actions(account.name)}>
                <EllipsisIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-56">
              <DropdownMenuItem onSelect={() => setDialog('rename')}>
                <PencilIcon />
                {t.rename}
              </DropdownMenuItem>
              {!imported && (
                <>
                  <DropdownMenuItem onSelect={() => toggle.mutate()}>
                    {account.enabled ? <PauseIcon /> : <PlayIcon />}
                    {account.enabled ? t.pause : t.resume}
                  </DropdownMenuItem>
                  <DropdownMenuItem onSelect={() => setDialog('password')}>
                    <KeyRoundIcon />
                    {t.password}
                  </DropdownMenuItem>
                </>
              )}
              <DropdownMenuSeparator />
              <DropdownMenuItem
                className="text-danger"
                disabled={account.status.running}
                onSelect={() => setDialog('remove')}
              >
                <Trash2Icon />
                {t.remove}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </>
      )}

      <TextDialog
        open={dialog === 'rename'}
        onOpenChange={(open) => setDialog(open ? 'rename' : null)}
        title={t.renameTitle}
        label={strings.connect.name}
        initial={account.name}
        onSave={async (name) => {
          await updateAccount(account.id, { name })
          await refresh()
        }}
      />
      <TextDialog
        open={dialog === 'password'}
        onOpenChange={(open) => setDialog(open ? 'password' : null)}
        title={t.passwordTitle}
        description={t.passwordLead}
        label={strings.connect.password}
        password
        onSave={async (password) => {
          await updateAccount(account.id, { password })
          await refresh()
        }}
      />
      <ConfirmDialog
        open={dialog === 'remove'}
        onOpenChange={(open) => setDialog(open ? 'remove' : null)}
        title={t.removeTitle(account.name)}
        description={t.removeLead}
        action={t.remove}
        busyAction={strings.settings.deleting}
        onConfirm={async () => {
          await deleteAccount(account.id)
          await queryClient.invalidateQueries({ queryKey: ['archives', archiveId] })
          await refresh()
        }}
      />
    </div>
  )
}
