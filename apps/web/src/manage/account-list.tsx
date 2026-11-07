import {
  type Account,
  accountsQuery,
  deleteAccount,
  syncAccount,
  syncScheduleQuery,
  updateAccount,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
  Skeleton,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArchiveIcon,
  ClockIcon,
  ContactRoundIcon,
  EllipsisIcon,
  FileArchiveIcon,
  ServerCogIcon,
  KeyRoundIcon,
  LogInIcon,
  MailIcon,
  PauseIcon,
  PencilIcon,
  PlayIcon,
  RadioTowerIcon,
  RefreshCwIcon,
  ShieldCheckIcon,
  Trash2Icon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { useOAuthProviders } from '../hooks/use-oauth-providers.ts'
import { type AccountTone, accountState } from '../lib/accounts.ts'
import { contactsState, contactsSupport } from '../lib/contacts-calendars.ts'
import { isOAuthProvider, oauthProviderNames, switchableProvider } from '../lib/oauth.ts'
import {
  formatInterval,
  formatMinAge,
  formatNextSync,
  intervalChoices,
  minAgeOptions,
} from '../lib/schedule.ts'
import {
  gmailLike,
  type WatchBadge,
  type WatchTone,
  watchBadge,
  watchMode,
  watchRequest,
} from '../lib/watch.ts'
import { strings } from '../strings.ts'
import { ConfirmDialog, TextDialog } from './confirm-dialog.tsx'
import { ContactsCalendarsDialog } from './contacts-calendars.tsx'
import { EditConnectionDialog } from './edit-connection.tsx'
import { OAuthSignInDialog } from './oauth-sign-in.tsx'
import { TrustedServersDialog } from './trusted-servers.tsx'

const t = strings.accounts

const toneClass: Record<AccountTone, string> = {
  running: 'bg-accent',
  error: 'bg-danger',
  paused: 'bg-fg-subtle',
  done: 'bg-success',
  waiting: 'bg-border-strong',
}

const watchToneClass: Record<WatchTone, string> = {
  live: 'text-success',
  warn: 'text-warning',
  muted: 'text-fg-subtle',
}

function WatchMark({ badge }: { badge: WatchBadge | undefined }) {
  if (!badge) return null
  return (
    <span
      title={badge.title}
      className={cn('hidden shrink-0 sm:inline', watchToneClass[badge.tone])}
    >
      · {badge.label}
    </span>
  )
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
  const [dialog, setDialog] = useState<
    | 'rename'
    | 'password'
    | 'edit'
    | 'remove'
    | 'sign-in'
    | 'contacts'
    | 'consent'
    | 'watch-folders'
    | 'trusted'
    | null
  >(null)
  const schedule = useQuery({ ...syncScheduleQuery, enabled: manage })
  const state = accountState(account, now)
  const contacts = contactsState(account, now)
  const support = contactsSupport(account)
  const imported = account.kind === 'import'
  const Icon = imported ? FileArchiveIcon : MailIcon
  const oauth = isOAuthProvider(account.oauth_provider) ? account.oauth_provider : undefined
  const offered = useOAuthProviders()
  const switchTo = switchableProvider(account, offered)
  const signInWith = oauth ?? switchTo
  const token = account.jmap?.auth === 'token'
  const trustable = !imported && Array.isArray(account.trusted_authserv_ids)
  const suggested =
    account.trusted_authserv_ids?.length === 0 ? account.suggested_authserv_id : undefined

  const refresh = () => queryClient.invalidateQueries({ queryKey: ['archives'] })
  const sync = useMutation({ mutationFn: () => syncAccount(account.id), onSettled: refresh })
  const toggle = useMutation({
    mutationFn: () => updateAccount(account.id, { enabled: !account.enabled }),
    onSettled: refresh,
  })
  const every = useMutation({
    mutationFn: (minutes: number | null) =>
      updateAccount(account.id, { sync_interval_minutes: minutes }),
    onSettled: refresh,
  })
  const minAge = useMutation({
    mutationFn: (days: number | null) => updateAccount(account.id, { min_age_days: days }),
    onSettled: refresh,
  })
  const watch = useMutation({
    mutationFn: (mode: 'off' | 'inbox') =>
      updateAccount(account.id, { watch: watchRequest(mode, '') }),
    onSettled: refresh,
  })
  const contactsEvery = useMutation({
    mutationFn: (minutes: number | null) =>
      updateAccount(account.id, { contacts_interval_minutes: minutes }),
    onSettled: refresh,
  })
  const mailWatch = account.enabled ? watchBadge(account.status.watch, 'mail') : undefined
  const contactsWatch = account.enabled
    ? watchBadge(account.status.contacts_watch, 'contacts')
    : undefined
  const mailEvery = account.sync_interval_minutes ?? schedule.data?.minutes ?? 15
  const next =
    !imported && account.enabled && !account.status.running && account.status.next_sync_at
      ? formatNextSync(account.status.next_sync_at, now)
      : undefined

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
          {oauth && (
            <span className="hidden shrink-0 text-xs text-fg-subtle sm:inline">
              {strings.oauth.provider(oauthProviderNames[oauth])}
            </span>
          )}
          {account.kind === 'jmap' && (
            <span className="hidden shrink-0 text-xs text-fg-subtle sm:inline">{t.jmap}</span>
          )}
          {account.kind === 'graph' && (
            <span className="hidden shrink-0 text-xs text-fg-subtle sm:inline">{t.graph}</span>
          )}
        </span>
        <span className="flex min-w-0 items-center gap-2 text-xs">
          <StatusDot tone={state.tone} />
          <span
            className={cn('truncate', state.tone === 'error' ? 'text-danger' : 'text-fg-muted')}
          >
            {state.label}
          </span>
          <WatchMark badge={mailWatch} />
          {next && <span className="hidden shrink-0 text-fg-subtle sm:inline">· {next}</span>}
        </span>
        {contacts && (
          <span className="flex min-w-0 items-center gap-2 text-xs">
            <StatusDot tone={contacts.tone} />
            {manage && account.contacts_calendars?.needs_consent ? (
              <button
                type="button"
                className="truncate text-danger underline decoration-dotted underline-offset-2"
                onClick={() => setDialog('consent')}
              >
                {contacts.label}
              </button>
            ) : (
              <span
                className={cn(
                  'truncate',
                  contacts.tone === 'error' ? 'text-danger' : 'text-fg-muted',
                )}
              >
                {contacts.label}
              </span>
            )}
            <WatchMark badge={contactsWatch} />
          </span>
        )}
        {manage && trustable && suggested && (
          <button
            type="button"
            className="w-fit truncate text-left text-xs text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus"
            onClick={() => setDialog('trusted')}
          >
            {strings.trustedServers.suggestionHint(suggested)}
          </button>
        )}
      </div>
      {manage && (
        <>
          {account.needs_sign_in && oauth ? (
            <Button variant="secondary" onClick={() => setDialog('sign-in')}>
              <LogInIcon />
              {strings.oauth.signInAgain}
            </Button>
          ) : (
            !imported && (
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
            )
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
                  {oauth ? (
                    <DropdownMenuItem onSelect={() => setDialog('sign-in')}>
                      <LogInIcon />
                      {strings.oauth.signInAgain}
                    </DropdownMenuItem>
                  ) : (
                    <DropdownMenuItem onSelect={() => setDialog('password')}>
                      <KeyRoundIcon />
                      {token ? t.token : t.password}
                    </DropdownMenuItem>
                  )}
                  {switchTo && (
                    <DropdownMenuItem onSelect={() => setDialog('sign-in')}>
                      <LogInIcon />
                      {strings.oauth.switchTo(oauthProviderNames[switchTo])}
                    </DropdownMenuItem>
                  )}
                  {(account.connection || account.jmap) && (
                    <DropdownMenuItem onSelect={() => setDialog('edit')}>
                      <ServerCogIcon />
                      {t.edit}
                    </DropdownMenuItem>
                  )}
                  {trustable && (
                    <DropdownMenuItem onSelect={() => setDialog('trusted')}>
                      <ShieldCheckIcon />
                      {strings.trustedServers.menu}
                    </DropdownMenuItem>
                  )}
                  {support !== 'none' && (
                    <DropdownMenuItem onSelect={() => setDialog('contacts')}>
                      <ContactRoundIcon />
                      {strings.contacts.menu}
                    </DropdownMenuItem>
                  )}
                  <DropdownMenuSub>
                    <DropdownMenuSubTrigger>
                      <ClockIcon />
                      {t.schedule}
                    </DropdownMenuSubTrigger>
                    <DropdownMenuSubContent className="w-64">
                      <DropdownMenuRadioGroup
                        value={
                          account.sync_interval_minutes == null
                            ? 'server'
                            : String(account.sync_interval_minutes)
                        }
                        onValueChange={(value) =>
                          every.mutate(value === 'server' ? null : Number(value))
                        }
                      >
                        <DropdownMenuRadioItem value="server">
                          {t.likeServer(formatInterval(schedule.data?.minutes ?? 15))}
                        </DropdownMenuRadioItem>
                        <DropdownMenuSeparator />
                        {intervalChoices.map((minutes) => (
                          <DropdownMenuRadioItem key={minutes} value={String(minutes)}>
                            {formatInterval(minutes)}
                          </DropdownMenuRadioItem>
                        ))}
                      </DropdownMenuRadioGroup>
                    </DropdownMenuSubContent>
                  </DropdownMenuSub>
                  <DropdownMenuSub>
                    <DropdownMenuSubTrigger>
                      <RadioTowerIcon />
                      {t.watch}
                    </DropdownMenuSubTrigger>
                    <DropdownMenuSubContent className="w-64">
                      <DropdownMenuRadioGroup
                        value={watchMode(account)}
                        onValueChange={(value) => {
                          if (value === 'folders') setDialog('watch-folders')
                          else if (value === 'off' || value === 'inbox') watch.mutate(value)
                        }}
                      >
                        <DropdownMenuRadioItem value="inbox">
                          {gmailLike(account) ? t.watchAllMail : t.watchInbox}
                        </DropdownMenuRadioItem>
                        {account.kind === 'imap' && (
                          <DropdownMenuRadioItem value="folders">
                            {t.watchFolders}
                          </DropdownMenuRadioItem>
                        )}
                        <DropdownMenuRadioItem value="off">{t.watchOff}</DropdownMenuRadioItem>
                      </DropdownMenuRadioGroup>
                      <DropdownMenuSeparator />
                      <DropdownMenuLabel className="h-auto py-2 leading-4">
                        {t.watchHint}
                      </DropdownMenuLabel>
                    </DropdownMenuSubContent>
                  </DropdownMenuSub>
                  {account.contacts_calendars?.enabled && (
                    <DropdownMenuSub>
                      <DropdownMenuSubTrigger>
                        <ContactRoundIcon />
                        {t.contactsSchedule}
                      </DropdownMenuSubTrigger>
                      <DropdownMenuSubContent className="w-64">
                        <DropdownMenuRadioGroup
                          value={
                            account.contacts_calendars.interval_minutes == null
                              ? 'mail'
                              : String(account.contacts_calendars.interval_minutes)
                          }
                          onValueChange={(value) =>
                            contactsEvery.mutate(value === 'mail' ? null : Number(value))
                          }
                        >
                          <DropdownMenuRadioItem value="mail">
                            {t.likeMail(formatInterval(mailEvery))}
                          </DropdownMenuRadioItem>
                          <DropdownMenuSeparator />
                          {[1, ...intervalChoices].map((minutes) => (
                            <DropdownMenuRadioItem key={minutes} value={String(minutes)}>
                              {formatInterval(minutes)}
                            </DropdownMenuRadioItem>
                          ))}
                        </DropdownMenuRadioGroup>
                      </DropdownMenuSubContent>
                    </DropdownMenuSub>
                  )}
                  <DropdownMenuSub>
                    <DropdownMenuSubTrigger>
                      <ArchiveIcon />
                      {t.minAge}
                    </DropdownMenuSubTrigger>
                    <DropdownMenuSubContent className="w-64">
                      <DropdownMenuRadioGroup
                        value={String(account.min_age_days ?? 0)}
                        onValueChange={(value) => minAge.mutate(Number(value) || null)}
                      >
                        <DropdownMenuRadioItem value="0">
                          {formatMinAge(null)}
                        </DropdownMenuRadioItem>
                        <DropdownMenuSeparator />
                        {minAgeOptions(account.min_age_days).map((days) => (
                          <DropdownMenuRadioItem key={days} value={String(days)}>
                            {formatMinAge(days)}
                          </DropdownMenuRadioItem>
                        ))}
                      </DropdownMenuRadioGroup>
                      <DropdownMenuSeparator />
                      <DropdownMenuLabel className="h-auto py-2 leading-4">
                        {t.minAgeHint}
                      </DropdownMenuLabel>
                    </DropdownMenuSubContent>
                  </DropdownMenuSub>
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
        title={token ? t.tokenTitle : t.passwordTitle}
        description={token ? t.tokenLead : t.passwordLead}
        label={token ? strings.jmap.token : strings.connect.password}
        password
        onSave={async (password) => {
          await updateAccount(account.id, { password })
          await refresh()
        }}
      />
      <TextDialog
        open={dialog === 'watch-folders'}
        onOpenChange={(open) => setDialog(open ? 'watch-folders' : null)}
        title={t.watchFoldersTitle}
        description={t.watchFoldersLead}
        label={t.watchFoldersLabel}
        initial={(account.watch?.folders?.length ? account.watch.folders : ['INBOX']).join(', ')}
        onSave={async (folders) => {
          await updateAccount(account.id, { watch: watchRequest('folders', folders) })
          await refresh()
        }}
      />
      <EditConnectionDialog
        account={account}
        open={dialog === 'edit'}
        onOpenChange={(open) => setDialog(open ? 'edit' : null)}
      />
      {trustable && (
        <TrustedServersDialog
          account={account}
          open={dialog === 'trusted'}
          onOpenChange={(open) => setDialog(open ? 'trusted' : null)}
        />
      )}
      <ContactsCalendarsDialog
        account={account}
        open={dialog === 'contacts'}
        onOpenChange={(open) => setDialog(open ? 'contacts' : null)}
        onSignIn={() => setDialog('consent')}
      />
      {oauth && (
        <OAuthSignInDialog
          account={account}
          provider={oauth}
          reason="consent"
          open={dialog === 'consent'}
          onOpenChange={(open) => setDialog(open ? 'consent' : null)}
        />
      )}
      {signInWith && (
        <OAuthSignInDialog
          account={account}
          provider={signInWith}
          reason={oauth ? 'again' : 'switch'}
          open={dialog === 'sign-in'}
          onOpenChange={(open) => setDialog(open ? 'sign-in' : null)}
        />
      )}
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
