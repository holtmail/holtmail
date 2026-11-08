import {
  type Account,
  accountsQuery,
  ApiError,
  type Archive,
  archivesQuery,
  capabilitiesQuery,
  checkTransport,
  createIdentity,
  createInstanceTransport,
  createTransport,
  deleteIdentity,
  deleteTransport,
  type Identity,
  identitiesQuery,
  type IdentitySuggestion,
  identitySuggestionsQuery,
  instanceTransportsQuery,
  type Person,
  sessionQuery,
  setDefaultIdentity,
  setIdentityUsers,
  setTransportGrants,
  type Transport,
  type TransportCheck,
  transportsQuery,
  updateIdentity,
  updateTransport,
  usersQuery,
} from '@holtmail/api-client'
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
  Skeleton,
  Switch,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import type { Editor } from '@tiptap/react'
import { EllipsisIcon, PlusIcon, SendIcon, ServerIcon } from 'lucide-react'
import { type FormEvent, type ReactNode, useId, useState } from 'react'

import { Field, FormError, PasswordField } from '../auth/field.tsx'
import { RichEditor } from '../compose/rich-editor.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatSize } from '../lib/format.ts'
import {
  appendState,
  emptyTransportForm,
  formServer,
  isTransportKind,
  type IdentityForm,
  identityForm,
  identityRequest,
  identityUpdate,
  newIdentityForm,
  type SentCopyChoice,
  type TransportForm,
  type TransportFormError,
  sentCopyAccount,
  transportFields,
  transportForm,
  transportKinds,
  transportRequest,
  transportUpdate,
} from '../lib/sending.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { Picker } from './aliases-page.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.writing.sending

/** Settings → Sending: identities with their suggestions, and transports of the archive and of
 * the instance. */
export function SendingPage() {
  const archives = useQuery(archivesQuery)
  const [chosen, setChosen] = useState<string>()
  const list = archives.data ?? []
  const archive = list.find((a) => a.id === chosen) ?? list[0]
  return (
    <SettingsPage
      title={t.nav}
      lead={t.lead}
      action={
        list.length > 1 && archive ? (
          <Picker
            label={t.archive}
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
        <ArchiveSending key={archive.id} archive={archive} archives={list} />
      ) : (
        <p className="text-sm text-fg-muted">{strings.categories.noArchives}</p>
      )}
    </SettingsPage>
  )
}

function ArchiveSending({ archive, archives }: { archive: Archive; archives: Archive[] }) {
  const transports = useQuery(transportsQuery(archive.id))
  const unavailable = transports.error instanceof ApiError && transports.error.status === 503
  if (transports.isError && !unavailable) {
    return <FormError message={t.transportsFailed(authErrorMessage(transports.error))} />
  }
  if (transports.isPending) return <Skeleton className="h-24" />
  return (
    <div className="grid gap-12">
      {unavailable && <p className="text-sm text-fg-muted">{t.unavailable}</p>}
      <Identities archive={archive} transports={transports.data ?? []} />
      <Transports archive={archive} transports={transports.data ?? []} />
      <SharedTransports archives={archives} />
    </div>
  )
}

function Section({
  title,
  lead,
  action,
  children,
}: {
  title: string
  lead: string
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="grid gap-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div className="grid gap-1">
          <h3 className="text-lg font-normal">{title}</h3>
          <p className="text-sm text-fg-muted">{lead}</p>
        </div>
        {action}
      </div>
      {children}
    </section>
  )
}

function useRefresh(archiveId: string) {
  const queryClient = useQueryClient()
  return () =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'identities'] }),
      queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'transports'] }),
      queryClient.invalidateQueries({ queryKey: ['transports'] }),
    ])
}

function Identities({ archive, transports }: { archive: Archive; transports: Transport[] }) {
  const identities = useQuery(identitiesQuery(archive.id))
  const suggestions = useQuery(identitySuggestionsQuery(archive.id))
  const me = useQuery(sessionQuery).data
  const [editing, setEditing] = useState<
    { identity?: Identity; suggestion?: IdentitySuggestion } | undefined
  >()

  return (
    <Section
      title={t.identities}
      lead={t.identitiesLead}
      action={
        <Button
          variant="secondary"
          disabled={transports.length === 0 || !identities.isSuccess}
          onClick={() => setEditing({})}
        >
          <PlusIcon />
          {t.addIdentity}
        </Button>
      }
    >
      {identities.isPending ? (
        <Skeleton className="h-24" />
      ) : identities.isError ? (
        <FormError message={t.identitiesFailed(authErrorMessage(identities.error))} />
      ) : identities.data.length > 0 ? (
        <ul className="grid divide-y divide-border border-y border-border">
          {identities.data.map((identity) => (
            <IdentityRow
              key={identity.id}
              identity={identity}
              archiveId={archive.id}
              transports={transports}
              mayUse={me !== undefined && identity.users.includes(me.id)}
              onEdit={() => setEditing({ identity })}
            />
          ))}
        </ul>
      ) : (
        <p className="text-sm text-fg-muted">{t.noIdentities}</p>
      )}
      {transports.length === 0 && <p className="text-sm text-fg-subtle">{t.noTransports}</p>}
      {suggestions.isError && (
        <FormError message={t.suggestionsFailed(authErrorMessage(suggestions.error))} />
      )}
      {identities.isSuccess && suggestions.data && suggestions.data.length > 0 && (
        <div className="grid gap-2">
          <h4 className="text-sm text-fg">{t.suggestions}</h4>
          <p className="text-xs text-fg-muted">{t.suggestionsLead}</p>
          <ul className="grid divide-y divide-border border-y border-border">
            {suggestions.data.map((s) => (
              <li key={s.address} className="flex flex-wrap items-center gap-3 py-2">
                <span className="min-w-0 flex-1 truncate font-mono text-sm">{s.address}</span>
                <span className="text-xs text-fg-subtle">
                  {t.suggestionSource[s.source] ?? s.source}
                  {!s.transport && ` · ${t.noTransportFor}`}
                </span>
                <Button
                  variant="secondary"
                  size="sm"
                  disabled={transports.length === 0}
                  onClick={() => setEditing({ suggestion: s })}
                >
                  {t.setUp}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {editing && (
        <IdentityDialog
          archiveId={archive.id}
          transports={transports}
          identity={editing.identity}
          suggestion={editing.suggestion}
          onClose={() => setEditing(undefined)}
        />
      )}
    </Section>
  )
}

function IdentityRow({
  identity,
  archiveId,
  transports,
  mayUse,
  onEdit,
}: {
  identity: Identity
  archiveId: string
  transports: Transport[]
  mayUse: boolean
  onEdit: () => void
}) {
  const refresh = useRefresh(archiveId)
  const [removing, setRemoving] = useState(false)
  const makeDefault = useMutation({
    mutationFn: () => setDefaultIdentity(archiveId, identity.id),
    onSettled: refresh,
  })
  const transport = transports.find((tr) => tr.id === identity.transport)
  return (
    <li className="flex items-center gap-3 py-3">
      <SendIcon aria-hidden className="size-4 shrink-0 text-fg-subtle" />
      <div className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm">{identity.name || identity.address}</span>
          {identity.default && <Badge variant="accent">{t.isDefault}</Badge>}
          {!identity.active && <Badge variant="outline">{t.inactive}</Badge>}
        </span>
        <span className="truncate font-mono text-xs text-fg-subtle">{identity.address}</span>
        <span className="truncate text-xs text-fg-subtle">
          {transport?.name ?? identity.transport}
        </span>
        {identity.rewritten_from && (
          <span role="status" className="text-xs text-warning">
            {t.rewrittenFrom(identity.rewritten_from.seen, identity.address)}
          </span>
        )}
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" aria-label={identity.address}>
            <EllipsisIcon />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-56">
          <DropdownMenuItem onSelect={onEdit}>{t.edit}</DropdownMenuItem>
          {mayUse && identity.active && !identity.default && (
            <DropdownMenuItem onSelect={() => makeDefault.mutate()}>
              {t.makeDefault}
            </DropdownMenuItem>
          )}
          <DropdownMenuItem className="text-danger" onSelect={() => setRemoving(true)}>
            {t.remove}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.removeIdentityTitle(identity.address)}
        description={t.removeIdentityLead}
        action={t.remove}
        busyAction={strings.settings.deleting}
        onConfirm={async () => {
          await deleteIdentity(identity.id)
          await refresh()
        }}
      />
    </li>
  )
}

function Check({
  label,
  checked,
  onChange,
  hint,
}: {
  label: string
  checked: boolean
  onChange: (checked: boolean) => void
  hint?: string
}) {
  const id = useId()
  return (
    <div className="flex items-start gap-3">
      <input
        id={id}
        type="checkbox"
        className="mt-0.5 size-4 accent-accent"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      <div className="grid gap-0.5">
        <label htmlFor={id} className="text-sm">
          {label}
        </label>
        {hint && <span className="text-xs text-fg-subtle">{hint}</span>}
      </div>
    </div>
  )
}

function IdentityDialog({
  archiveId,
  transports,
  identity,
  suggestion,
  onClose,
}: {
  archiveId: string
  transports: Transport[]
  identity?: Identity
  suggestion?: IdentitySuggestion
  onClose: () => void
}) {
  const me = useQuery(sessionQuery).data
  const people = useQuery(usersQuery).data ?? []
  const refresh = useRefresh(archiveId)
  const [form, setForm] = useState<IdentityForm>(() =>
    identity ? identityForm(identity) : newIdentityForm(transports, me?.id, suggestion),
  )
  const set = <K extends keyof IdentityForm>(key: K, value: IdentityForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }))
  const save = useMutation({
    mutationFn: async () => {
      const saved = identity
        ? await updateIdentity(identity.id, identityUpdate(form))
        : await createIdentity(archiveId, identityRequest(form))
      const users = form.users.toSorted()
      if (JSON.stringify(saved.users.toSorted()) !== JSON.stringify(users)) {
        await setIdentityUsers(saved.id, users)
      }
    },
    onSuccess: async () => {
      await refresh()
      onClose()
    },
  })
  const submit = (event: FormEvent) => {
    event.preventDefault()
    save.mutate()
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <form className="grid max-h-[80vh] gap-5 overflow-y-auto" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{identity ? t.editIdentity : t.newIdentity}</DialogTitle>
            <DialogDescription>{t.identitiesLead}</DialogDescription>
          </DialogHeader>
          <Field
            label={t.address}
            type="email"
            required
            value={form.address}
            onChange={(e) => set('address', e.target.value)}
          />
          <Field label={t.name} value={form.name} onChange={(e) => set('name', e.target.value)} />
          <Field
            label={t.replyTo}
            type="email"
            hint={t.replyToHint}
            value={form.replyTo}
            onChange={(e) => set('replyTo', e.target.value)}
          />
          <div className="grid gap-2">
            <span className="text-xs text-fg-muted">{t.transport}</span>
            <Picker
              label={t.transport}
              value={form.transport}
              choices={transports.map((tr) => ({ value: tr.id, label: tr.name }))}
              onChange={(value) => set('transport', value)}
            />
          </div>
          <SentCopy
            archiveId={archiveId}
            form={form}
            transports={transports}
            onChoice={(choice) => set('sentCopy', choice)}
            onFolder={(folder) => set('sentFolder', folder)}
          />
          <Check label={t.bccSelf} checked={form.bccSelf} onChange={(v) => set('bccSelf', v)} />
          <Check
            label={t.plusAddresses}
            checked={form.plusAddresses}
            onChange={(v) => set('plusAddresses', v)}
          />
          <div className="grid gap-2">
            <span className="text-xs text-fg-muted">{t.signature}</span>
            <div className="border border-border p-2">
              <RichEditor
                content={form.signatureHtml}
                label={t.signature}
                onChange={(html) => set('signatureHtml', html)}
                onReady={noopEditor}
                onSubmit={() => save.mutate()}
              />
            </div>
          </div>
          <div className="grid gap-2">
            <label htmlFor="identity-signature-text" className="text-xs text-fg-muted">
              {t.signatureText}
            </label>
            <textarea
              id="identity-signature-text"
              className="min-h-20 border border-border bg-bg p-2 font-mono text-sm"
              value={form.signatureText}
              onChange={(e) => set('signatureText', e.target.value)}
            />
            <span className="text-xs text-fg-subtle">{t.signatureTextHint}</span>
          </div>
          <UsersChecklist
            people={people}
            selected={form.users}
            onChange={(users) => set('users', users)}
          />
          <div className="flex items-start justify-between gap-4">
            <div className="grid gap-0.5">
              <span className="text-sm">{t.active}</span>
              <span className="text-xs text-fg-subtle">{t.activeHint}</span>
            </div>
            <Switch
              aria-label={t.active}
              checked={form.active}
              onCheckedChange={(v) => set('active', v)}
            />
          </div>
          <FormError message={save.isError ? authErrorMessage(save.error) : undefined} />
          <DialogFooter>
            <Button variant="ghost" onClick={onClose}>
              {t.cancel}
            </Button>
            <Button type="submit" variant="primary" disabled={save.isPending}>
              {save.isPending ? t.saving : t.save}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

const sentChoices: SentCopyChoice[] = ['provider', 'append', 'none']

function SentCopy({
  archiveId,
  form,
  transports,
  onChoice,
  onFolder,
}: {
  archiveId: string
  form: IdentityForm
  transports: Transport[]
  onChoice: (choice: SentCopyChoice) => void
  onFolder: (folder: string) => void
}) {
  const account = sentCopyAccount(form, transports)
  const capabilities = useQuery({
    ...capabilitiesQuery(account ?? ''),
    enabled: account != null,
    retry: false,
  })
  const accounts = useQuery(accountsQuery(archiveId))
  const accountName = accounts.data?.find((a) => a.id === account)?.name ?? ''
  const append = appendState(account, capabilities.isSuccess ? capabilities.data : undefined)
  const appendOpen = append === 'available' || append === 'unknown'
  const baseId = useId()
  return (
    <fieldset className="grid gap-2">
      <legend className="pb-2 text-xs text-fg-muted">{t.sentCopy}</legend>
      {sentChoices.map((choice) => {
        const disabled = choice === 'append' && !appendOpen && form.sentCopy !== 'append'
        const id = `${baseId}-${choice}`
        return (
          <div key={choice} className="flex items-start gap-3">
            <input
              id={id}
              type="radio"
              name="sent-copy"
              className="mt-0.5 size-4 accent-accent"
              aria-describedby={`${id}-hint`}
              checked={form.sentCopy === choice}
              disabled={disabled}
              onChange={() => onChoice(choice)}
            />
            <div className="grid gap-0.5">
              <label htmlFor={id} className={disabled ? 'text-sm text-fg-subtle' : 'text-sm'}>
                {t.sentCopyChoices[choice]}
              </label>
              <span id={`${id}-hint`} className="text-xs text-fg-subtle">
                {choice === 'append' && !appendOpen
                  ? t.appendStates[append](accountName)
                  : t.sentCopyHints[choice]}
              </span>
            </div>
          </div>
        )
      })}
      {form.sentCopy === 'append' && (
        <>
          <Field
            label={t.sentFolder}
            placeholder="Sent"
            value={form.sentFolder}
            onChange={(e) => onFolder(e.target.value)}
          />
          <span role="note" className="text-xs text-fg-subtle">
            {t.appendNote}
          </span>
        </>
      )}
    </fieldset>
  )
}

const noopEditor = (_editor: Editor | null) => undefined

function UsersChecklist({
  people,
  selected,
  onChange,
}: {
  people: Person[]
  selected: string[]
  onChange: (users: string[]) => void
}) {
  if (people.length === 0) return null
  return (
    <fieldset className="grid gap-2">
      <legend className="pb-1 text-xs text-fg-muted">{t.users}</legend>
      <div className="grid gap-px border border-border bg-border">
        {people.map(({ user }) => {
          const checked = selected.includes(user.id)
          return (
            <label key={user.id} className="flex h-10 items-center gap-3 bg-bg px-3 text-sm">
              <input
                type="checkbox"
                className="size-4 accent-accent"
                checked={checked}
                onChange={() =>
                  onChange(
                    checked ? selected.filter((id) => id !== user.id) : [...selected, user.id],
                  )
                }
              />
              <span className="truncate">{user.name}</span>
              <span className="truncate font-mono text-xs text-fg-subtle">{user.email}</span>
            </label>
          )
        })}
      </div>
      <span className="text-xs text-fg-subtle">{t.usersHint}</span>
    </fieldset>
  )
}

function Transports({ archive, transports }: { archive: Archive; transports: Transport[] }) {
  const accounts = useQuery(accountsQuery(archive.id))
  const [editing, setEditing] = useState<{ transport?: Transport } | undefined>()
  return (
    <Section
      title={t.transports}
      lead={t.transportsLead}
      action={
        <Button variant="secondary" onClick={() => setEditing({})}>
          <PlusIcon />
          {t.addTransport}
        </Button>
      }
    >
      {transports.length === 0 ? (
        <p className="text-sm text-fg-muted">{t.noTransportsYet}</p>
      ) : (
        <ul className="grid divide-y divide-border border-y border-border">
          {transports.map((transport) => (
            <TransportRow
              key={transport.id}
              transport={transport}
              archiveId={archive.id}
              shared={transport.archive == null}
              onEdit={() => setEditing({ transport })}
            />
          ))}
        </ul>
      )}
      {editing && (
        <TransportDialog
          archiveId={archive.id}
          accounts={accounts.data ?? []}
          transport={editing.transport}
          onClose={() => setEditing(undefined)}
        />
      )}
    </Section>
  )
}

function TransportRow({
  transport,
  archiveId,
  shared,
  onEdit,
  extra,
}: {
  transport: Transport
  archiveId: string
  shared: boolean
  onEdit?: () => void
  extra?: ReactNode
}) {
  const refresh = useRefresh(archiveId)
  const [removing, setRemoving] = useState(false)
  const [result, setResult] = useState<TransportCheck>()
  const check = useMutation({
    mutationFn: () => checkTransport(transport.id),
    onSuccess: setResult,
  })
  const where =
    transport.kind === 'account'
      ? t.kinds.account
      : transport.kind === 'jmap' || transport.kind === 'graph'
        ? (transport.url ?? t.kinds[transport.kind])
        : `${transport.host ?? ''}${transport.port ? `:${transport.port}` : ''}`
  return (
    <li className="grid gap-2 py-3">
      <div className="flex items-center gap-3">
        <ServerIcon aria-hidden className="size-4 shrink-0 text-fg-subtle" />
        <div className="grid min-w-0 flex-1 gap-0.5">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate text-sm">{transport.name}</span>
            {shared && <Badge variant="outline">{t.sharedTransports}</Badge>}
            {!transport.enabled && <Badge variant="outline">{t.disabled}</Badge>}
          </span>
          <span className="truncate font-mono text-xs text-fg-subtle">{where}</span>
          {transport.allowed_from.length > 0 && (
            <span className="truncate text-xs text-fg-subtle">
              {t.allowedFrom}: {transport.allowed_from.join(', ')}
            </span>
          )}
        </div>
        <Button
          variant="secondary"
          size="sm"
          disabled={check.isPending}
          onClick={() => check.mutate()}
        >
          {check.isPending ? t.checking : t.check}
        </Button>
        {onEdit && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" size="icon" aria-label={transport.name}>
                <EllipsisIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-48">
              <DropdownMenuItem onSelect={onEdit}>{t.edit}</DropdownMenuItem>
              <DropdownMenuItem className="text-danger" onSelect={() => setRemoving(true)}>
                {t.remove}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      {extra}
      {(result || check.isError) && (
        <p role="status" className={result?.ok ? 'text-xs text-success' : 'text-xs text-danger'}>
          {check.isError
            ? t.checkFailed(authErrorMessage(check.error))
            : result?.ok
              ? result.max_size
                ? t.checkOkSize(formatSize(result.max_size))
                : t.checkOk
              : t.checkFailed(result?.error ?? '')}
        </p>
      )}
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.removeTransportTitle(transport.name)}
        description={t.removeTransportLead}
        action={t.remove}
        busyAction={strings.settings.deleting}
        onConfirm={async () => {
          await deleteTransport(transport.id)
          await refresh()
        }}
      />
    </li>
  )
}

function formError(error: TransportFormError): string {
  switch (error.field) {
    case 'name':
      return t.required(t.transportName)
    case 'host':
      return t.required(t.host)
    case 'account':
      return t.required(t.account)
    case 'url':
      return t.required(t.url)
    case 'username':
      return t.required(t.username)
    case 'password':
      return t.required(t.secret)
    case 'secretAgain':
      return t.secretAgain
    case 'secretForAuth':
      return t.secretForAuth
    case 'allowedFrom':
      return t.patternInvalid(error.pattern)
    default:
      return t.numberInvalid
  }
}

function TransportDialog({
  archiveId,
  accounts,
  transport,
  shared = false,
  onClose,
}: {
  archiveId: string
  accounts: Account[]
  transport?: Transport
  shared?: boolean
  onClose: () => void
}) {
  const refresh = useRefresh(archiveId)
  const [form, setForm] = useState<TransportForm>(() =>
    transport ? transportForm(transport) : emptyTransportForm(),
  )
  const [problem, setProblem] = useState<string>()
  const set = <K extends keyof TransportForm>(key: K, value: TransportForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }))
  const servers = accounts.filter((a) => a.kind !== 'import')
  const accountKind = accounts.find((a) => a.id === form.account)?.kind
  const fields = transportFields(form, accountKind)
  const server = formServer(form, accountKind)
  const kinds = transportKinds.filter((k) => !shared || k !== 'account')
  const save = useMutation({
    mutationFn: async () => {
      if (transport) {
        const result = transportUpdate(form, transport, accountKind)
        if ('error' in result) throw new Error(formError(result.error))
        await updateTransport(transport.id, result.request)
        return
      }
      const result = transportRequest(form, accountKind)
      if ('error' in result) throw new Error(formError(result.error))
      if (shared) await createInstanceTransport({ ...result.request, account: null })
      else await createTransport(archiveId, result.request)
    },
    onSuccess: async () => {
      await refresh()
      onClose()
    },
    onError: (error) =>
      setProblem(
        error instanceof Error && !('status' in error) ? error.message : authErrorMessage(error),
      ),
  })
  const submit = (event: FormEvent) => {
    event.preventDefault()
    setProblem(undefined)
    save.mutate()
  }
  const isAccount = form.kind === 'account'
  const secretLabel =
    server === 'graph' || (fields.auth && form.auth === 'token') ? t.token : t.password

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <form className="grid max-h-[80vh] gap-5 overflow-y-auto" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>
              {transport ? t.editTransport : shared ? t.addShared : t.addTransport}
            </DialogTitle>
            <DialogDescription>{shared ? t.sharedLead : t.transportsLead}</DialogDescription>
          </DialogHeader>
          <Field
            label={t.transportName}
            required
            value={form.name}
            onChange={(e) => set('name', e.target.value)}
          />
          {!transport ? (
            <div className="grid gap-2">
              <span className="text-xs text-fg-muted">{t.kind}</span>
              <Picker
                label={t.kind}
                value={form.kind}
                choices={kinds.map((k) => ({ value: k, label: t.kinds[k] ?? k }))}
                onChange={(value) => isTransportKind(value) && set('kind', value)}
              />
              <span className="text-xs text-fg-subtle">{t.kindHints[form.kind]}</span>
            </div>
          ) : (
            <p className="text-xs text-fg-subtle">
              {t.kind}: {t.kinds[form.kind] ?? form.kind}
            </p>
          )}
          {isAccount && !transport && (
            <div className="grid gap-2">
              <span className="text-xs text-fg-muted">{t.account}</span>
              <Picker
                label={t.account}
                value={form.account}
                choices={servers.map((a) => ({ value: a.id, label: a.name }))}
                onChange={(value) => set('account', value)}
              />
            </div>
          )}
          {isAccount && server !== 'smtp' && (
            <p className="text-xs text-fg-subtle">{t.accountServer[server]}</p>
          )}
          {fields.server && (
            <>
              <div className="grid grid-cols-[1fr_6rem] gap-3">
                <Field
                  label={t.host}
                  hint={isAccount ? t.hostAccountHint : undefined}
                  required={!isAccount}
                  value={form.host}
                  onChange={(e) => set('host', e.target.value)}
                />
                <Field
                  label={t.port}
                  inputMode="numeric"
                  value={form.port}
                  onChange={(e) => set('port', e.target.value)}
                />
              </div>
              <div className="grid gap-2">
                <span className="text-xs text-fg-muted">{t.security}</span>
                <Picker
                  label={t.security}
                  value={form.security}
                  choices={(['starttls', 'tls', 'none'] as const).map((sec) => ({
                    value: sec,
                    label: t.securities[sec] ?? sec,
                  }))}
                  onChange={(value) =>
                    set(
                      'security',
                      value === 'tls' ? 'tls' : value === 'none' ? 'none' : 'starttls',
                    )
                  }
                />
              </div>
            </>
          )}
          {fields.url && (
            <Field
              label={t.url}
              type="url"
              inputMode="url"
              spellCheck={false}
              required={server === 'jmap'}
              placeholder={server === 'jmap' ? 'https://jmap.example.com' : t.graphDefault}
              hint={server === 'jmap' ? t.urlJmapHint : t.urlGraphHint}
              value={form.url}
              onChange={(e) => set('url', e.target.value)}
            />
          )}
          {fields.auth && (
            <div className="grid gap-2">
              <span className="text-xs text-fg-muted">{t.auth}</span>
              <Picker
                label={t.auth}
                value={form.auth}
                choices={(['password', 'token'] as const).map((a) => ({
                  value: a,
                  label: t.auths[a] ?? a,
                }))}
                onChange={(value) => set('auth', value === 'token' ? 'token' : 'password')}
              />
            </div>
          )}
          {fields.mailbox && (
            <Field
              label={t.mailbox}
              spellCheck={false}
              hint={t.mailboxHint}
              value={form.mailbox}
              onChange={(e) => set('mailbox', e.target.value)}
            />
          )}
          {fields.username && (
            <Field
              label={t.username}
              autoComplete="off"
              required={fields.auth}
              value={form.username}
              onChange={(e) => set('username', e.target.value)}
            />
          )}
          {fields.password && (
            <PasswordField
              label={secretLabel}
              autoComplete="new-password"
              required={!transport && !fields.server}
              hint={
                transport?.has_password
                  ? fields.url
                    ? t.secretKept
                    : t.passwordKept
                  : server === 'graph'
                    ? t.graphTokenHint
                    : undefined
              }
              value={form.password}
              onChange={(e) => set('password', e.target.value)}
            />
          )}
          <div className="grid gap-2">
            <label htmlFor="transport-allowed" className="text-xs text-fg-muted">
              {t.allowedFrom}
            </label>
            <textarea
              id="transport-allowed"
              spellCheck={false}
              className="min-h-20 border border-border bg-bg p-2 font-mono text-sm"
              value={form.allowedFrom}
              onChange={(e) => set('allowedFrom', e.target.value)}
            />
            <span className="text-xs text-fg-subtle">
              {t.allowedFromHint} {isAccount && t.allowedFromAccount}
            </span>
          </div>
          <fieldset className="grid gap-3">
            <legend className="pb-2 text-xs text-fg-muted">{t.limits}</legend>
            <div className="grid grid-cols-3 gap-3 max-sm:grid-cols-1">
              <Field
                label={t.maxSize}
                inputMode="numeric"
                placeholder={t.noLimit}
                value={form.maxSizeMb}
                onChange={(e) => set('maxSizeMb', e.target.value)}
              />
              <Field
                label={t.maxRecipients}
                inputMode="numeric"
                placeholder={t.noLimit}
                value={form.maxRecipients}
                onChange={(e) => set('maxRecipients', e.target.value)}
              />
              <Field
                label={t.perHour}
                inputMode="numeric"
                placeholder={t.noLimit}
                value={form.perHour}
                onChange={(e) => set('perHour', e.target.value)}
              />
            </div>
          </fieldset>
          <div className="flex items-center justify-between gap-4">
            <span className="text-sm">{t.enabled}</span>
            <Switch
              aria-label={t.enabled}
              checked={form.enabled}
              onCheckedChange={(v) => set('enabled', v)}
            />
          </div>
          <FormError message={problem} />
          <DialogFooter>
            <Button variant="ghost" onClick={onClose}>
              {t.cancel}
            </Button>
            <Button type="submit" variant="primary" disabled={save.isPending}>
              {save.isPending ? t.saving : t.save}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function SharedTransports({ archives }: { archives: Archive[] }) {
  const shared = useQuery(instanceTransportsQuery)
  const [editing, setEditing] = useState<{ transport?: Transport } | undefined>()
  const first = archives[0]?.id ?? ''
  if (shared.isError) return null
  return (
    <Section
      title={t.sharedTransports}
      lead={t.sharedLead}
      action={
        <Button variant="secondary" onClick={() => setEditing({})}>
          <PlusIcon />
          {t.addShared}
        </Button>
      }
    >
      {shared.data && shared.data.length > 0 ? (
        <ul className="grid divide-y divide-border border-y border-border">
          {shared.data.map((transport) => (
            <TransportRow
              key={transport.id}
              transport={transport}
              archiveId={first}
              shared={false}
              onEdit={() => setEditing({ transport })}
              extra={<Grants transport={transport} archives={archives} />}
            />
          ))}
        </ul>
      ) : (
        <p className="text-sm text-fg-muted">{t.noTransportsYet}</p>
      )}
      {editing && (
        <TransportDialog
          archiveId={first}
          accounts={[]}
          transport={editing.transport}
          shared
          onClose={() => setEditing(undefined)}
        />
      )}
    </Section>
  )
}

function Grants({ transport, archives }: { transport: Transport; archives: Archive[] }) {
  const queryClient = useQueryClient()
  const save = useMutation({
    mutationFn: (next: string[]) => setTransportGrants(transport.id, next),
    onSettled: () =>
      Promise.all([
        queryClient.invalidateQueries({ queryKey: ['transports'] }),
        queryClient.invalidateQueries({
          predicate: (q) => q.queryKey[0] === 'archives' && q.queryKey[2] === 'transports',
        }),
      ]),
  })
  return (
    <fieldset className="flex flex-wrap items-center gap-x-4 gap-y-1 pl-7 text-xs">
      <legend className="sr-only">{t.grantedTo}</legend>
      <span className="text-fg-subtle">{t.grantedTo}:</span>
      {archives.map((archive) => {
        const checked = transport.grants.includes(archive.id)
        return (
          <label key={archive.id} className="flex items-center gap-2">
            <input
              type="checkbox"
              className="size-3.5 accent-accent"
              checked={checked}
              disabled={save.isPending}
              onChange={() =>
                save.mutate(
                  checked
                    ? transport.grants.filter((id) => id !== archive.id)
                    : [...transport.grants, archive.id],
                )
              }
            />
            {archive.name}
          </label>
        )
      })}
    </fieldset>
  )
}
