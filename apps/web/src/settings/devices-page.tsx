import {
  type Archive,
  archivesQuery,
  type CreatedDevice,
  createDevice,
  type Device,
  type DeviceServer,
  deviceSetupQuery,
  devicesQuery,
  login,
  revokeDevice,
  sessionQuery,
  updateDevice,
} from '@holtmail/api-client'
import {
  Badge,
  Button,
  CopyButton,
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
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EllipsisIcon, PlusIcon } from 'lucide-react'
import { type FormEvent, type ReactNode, useId, useState } from 'react'

import { Field, FormError, PasswordField } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import {
  createDeviceRequest,
  deviceFormOf,
  type DeviceForm,
  deviceFormProblem,
  deviceProtocols,
  deviceScopes,
  formatDeviceDay,
  hasChanges,
  isDeviceProtocol,
  isDeviceScope,
  needsSignInAgain,
  newDeviceForm,
  serverRows,
  updateDeviceRequest,
  usesArchiveList,
  usesImplicitSeen,
  usesMailArchive,
  withItem,
  withProtocol,
} from '../lib/devices.ts'
import { formatFullDate } from '../lib/format.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { Picker } from './aliases-page.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.devices

const protocolLabel = (protocol: string) =>
  isDeviceProtocol(protocol) ? t.protocolNames[protocol].label : protocol

const scopeLabel = (scope: string) => (isDeviceScope(scope) ? t.scopeNames[scope].label : scope)

const browserHost = () => (typeof window === 'undefined' ? '' : window.location.hostname)

function Check({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string
  hint?: string
  checked: boolean
  onChange: (checked: boolean) => void
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

function Group({ legend, children }: { legend: string; children: ReactNode }) {
  return (
    <fieldset className="grid gap-3">
      <legend className="pb-3 text-xs text-fg-muted">{legend}</legend>
      {children}
    </fieldset>
  )
}

function Servers({
  servers,
  email,
  host,
}: {
  servers: readonly DeviceServer[]
  email: string | undefined
  host: string
}) {
  const rows = serverRows(servers, host)
  if (rows.length === 0) return null
  return (
    <div className="grid gap-2">
      <table className="w-full border-collapse text-sm">
        <thead>
          <tr className="border-b border-border text-left text-xs text-fg-muted">
            <th className="py-2 pr-4 font-normal">{t.servers.protocol}</th>
            <th className="py-2 pr-4 font-normal">{t.servers.host}</th>
            <th className="py-2 pr-4 font-normal">{t.servers.port}</th>
            <th className="py-2 font-normal">{t.servers.security}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr
              key={`${row.protocol}-${row.port}-${row.security}`}
              className="border-b border-border"
            >
              <td className="py-2 pr-4">{t.servers.names[row.protocol] ?? row.protocol}</td>
              <td className="py-2 pr-4 font-mono text-xs break-all">{row.host}</td>
              <td className="py-2 pr-4 font-mono text-xs">{row.port}</td>
              <td className="py-2 text-xs">
                {row.security === 'tls' ? t.servers.tls : t.servers.starttls}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {email && (
        <p className="text-xs text-fg-muted">
          {t.servers.user}: <span className="font-mono">{email}</span>
        </p>
      )}
    </div>
  )
}

function SignInAgain({
  open,
  email,
  onOpenChange,
  onDone,
}: {
  open: boolean
  email: string
  onOpenChange: (open: boolean) => void
  onDone: () => void
}) {
  const [password, setPassword] = useState('')
  const signIn = useMutation({
    mutationFn: () => login({ email, password }),
    onSuccess: () => {
      setPassword('')
      onOpenChange(false)
      onDone()
    },
  })
  const close = (next: boolean) => {
    if (!next) {
      setPassword('')
      signIn.reset()
    }
    onOpenChange(next)
  }
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (password) signIn.mutate()
  }
  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.signIn.title}</DialogTitle>
            <DialogDescription>{t.signIn.lead}</DialogDescription>
          </DialogHeader>
          <PasswordField
            label={t.signIn.password}
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            autoComplete="current-password"
            autoFocus
          />
          <FormError message={signIn.isError ? authErrorMessage(signIn.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => close(false)}>
              {t.signIn.cancel}
            </Button>
            <Button type="submit" disabled={!password || signIn.isPending}>
              {signIn.isPending ? t.signIn.busy : t.signIn.action}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function DeviceDialog({
  device,
  archives,
  servers,
  email,
  onClose,
  onCreated,
}: {
  device?: Device
  archives: readonly Archive[]
  servers: readonly DeviceServer[]
  email: string
  onClose: () => void
  onCreated: (created: CreatedDevice) => void
}) {
  const queryClient = useQueryClient()
  const [form, setForm] = useState<DeviceForm>(() =>
    device ? deviceFormOf(device, archives) : newDeviceForm(archives),
  )
  const [confirming, setConfirming] = useState(false)
  const set = (patch: Partial<DeviceForm>) => setForm((current) => ({ ...current, ...patch }))
  const save = useMutation({
    gcTime: 0,
    mutationFn: async (): Promise<CreatedDevice | null> => {
      if (!device) return createDevice(createDeviceRequest(form))
      const request = updateDeviceRequest(device, form, archives)
      if (hasChanges(request)) await updateDevice(device.id, request)
      return null
    },
    onSuccess: async (created) => {
      await queryClient.invalidateQueries({ queryKey: devicesQuery.queryKey })
      if (created) onCreated(created)
      else onClose()
    },
    onError: (error) => {
      if (needsSignInAgain(error)) setConfirming(true)
    },
  })
  const problem = deviceFormProblem(form)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (!problem) save.mutate()
  }
  const archiveChoices = archives.map((archive) => ({ value: archive.id, label: archive.name }))
  const listening = new Set(servers.map((server) => server.protocol))
  const f = t.form
  const error = save.isError && !needsSignInAgain(save.error) ? save.error : undefined
  return (
    <Dialog open onOpenChange={(next) => !next && onClose()}>
      <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto">
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{device ? f.editTitle(device.name) : f.newTitle}</DialogTitle>
            <DialogDescription>{f.lead}</DialogDescription>
          </DialogHeader>
          <Field
            label={f.name}
            value={form.name}
            placeholder={f.namePlaceholder}
            maxLength={100}
            onChange={(event) => set({ name: event.target.value })}
            autoFocus
          />
          <Group legend={f.protocols}>
            {deviceProtocols.map((protocol) => {
              const names = t.protocolNames[protocol]
              const off = (protocol === 'imap' || protocol === 'smtp') && !listening.has(protocol)
              return (
                <Check
                  key={protocol}
                  label={names.label}
                  hint={off ? `${names.hint} ${f.protocolOff}` : names.hint}
                  checked={form.protocols.includes(protocol)}
                  onChange={(on) => setForm((current) => withProtocol(current, protocol, on))}
                />
              )
            })}
          </Group>
          {usesMailArchive(form.protocols) && (
            <div className="grid gap-2">
              <span className="text-xs text-fg-muted">{f.mailArchive}</span>
              <Picker
                label={f.mailArchive}
                value={form.mailArchive}
                choices={archiveChoices}
                onChange={(mailArchive) => set({ mailArchive })}
              />
              <span className="text-xs text-fg-subtle">{f.mailArchiveHint}</span>
            </div>
          )}
          {usesArchiveList(form.protocols) && (
            <Group legend={f.archives}>
              <Check
                label={f.allArchives}
                hint={f.allArchivesHint}
                checked={form.allArchives}
                onChange={(allArchives) => set({ allArchives })}
              />
              {!form.allArchives &&
                archives.map((archive) => (
                  <Check
                    key={archive.id}
                    label={archive.name}
                    checked={form.archives.includes(archive.id)}
                    onChange={(on) => set({ archives: withItem(form.archives, archive.id, on) })}
                  />
                ))}
            </Group>
          )}
          <Group legend={f.scopes}>
            {deviceScopes.map((scope) => (
              <Check
                key={scope}
                label={t.scopeNames[scope].label}
                hint={t.scopeNames[scope].hint}
                checked={form.scopes.includes(scope)}
                onChange={(on) => set({ scopes: withItem(form.scopes, scope, on) })}
              />
            ))}
          </Group>
          {usesImplicitSeen(form.protocols) && (
            <Check
              label={f.implicitSeen}
              hint={f.implicitSeenHint}
              checked={form.implicitSeen}
              onChange={(implicitSeen) => set({ implicitSeen })}
            />
          )}
          {problem && <p className="text-xs text-fg-subtle">{f.problems[problem]}</p>}
          <FormError message={error ? authErrorMessage(error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {f.cancel}
            </Button>
            <Button type="submit" disabled={problem !== null || save.isPending}>
              {device
                ? save.isPending
                  ? f.saving
                  : f.save
                : save.isPending
                  ? f.creating
                  : f.create}
            </Button>
          </DialogFooter>
        </form>
        <SignInAgain
          open={confirming}
          email={email}
          onOpenChange={setConfirming}
          onDone={() => save.mutate()}
        />
      </DialogContent>
    </Dialog>
  )
}

function SecretDialog({
  created,
  servers,
  email,
  onDone,
}: {
  created: CreatedDevice
  servers: readonly DeviceServer[]
  email: string
  onDone: () => void
}) {
  const rows = serverRows(servers, browserHost(), created.device.protocols)
  return (
    <Dialog open>
      <DialogContent
        showClose={false}
        className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto"
      >
        <DialogHeader>
          <DialogTitle>{t.secret.title(created.device.name)}</DialogTitle>
          <DialogDescription>{t.secret.lead}</DialogDescription>
        </DialogHeader>
        <div className="flex flex-wrap items-center gap-3 border border-border px-4 py-3">
          <code className="min-w-0 flex-1 font-mono text-sm break-all select-all">
            {created.secret}
          </code>
          <CopyButton text={created.secret} label={t.secret.copy} copiedLabel={t.secret.copied} />
        </div>
        <Servers servers={rows} email={email} host={browserHost()} />
        <DialogFooter>
          <Button onClick={onDone}>{t.secret.done}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function Row({
  device,
  archives,
  onEdit,
}: {
  device: Device
  archives: readonly Archive[]
  onEdit: () => void
}) {
  const queryClient = useQueryClient()
  const [revoking, setRevoking] = useState(false)
  const revoked = device.revoked_at != null
  const archiveName = (id: string) =>
    archives.find((archive) => archive.id === id)?.name ?? t.row.unknownArchive
  const used =
    device.last_used_day == null
      ? t.row.neverUsed
      : t.row.lastUsed(
          formatDeviceDay(device.last_used_day),
          device.last_protocol ? protocolLabel(device.last_protocol) : '',
        )
  const where = [
    device.mail_archive ? t.row.mailArchive(archiveName(device.mail_archive)) : null,
    usesArchiveList(device.protocols)
      ? device.archives == null
        ? t.row.allArchives
        : t.row.someArchives(device.archives.map(archiveName).join(', '))
      : null,
  ].filter((line) => line !== null)
  return (
    <li className="grid gap-2 border border-border px-4 py-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="min-w-0 flex-1 truncate text-base text-fg">{device.name}</span>
        {device.protocols.map((protocol) => (
          <Badge key={protocol} variant="outline">
            {protocolLabel(protocol)}
          </Badge>
        ))}
        {!revoked && (
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button size="icon" variant="ghost" aria-label={t.row.actions(device.name)}>
                <EllipsisIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={onEdit}>{t.row.edit}</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => setRevoking(true)}>{t.row.revoke}</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </div>
      <p className="text-xs text-fg-muted">{device.scopes.map(scopeLabel).join(' · ')}</p>
      {where.length > 0 && <p className="text-xs text-fg-muted">{where.join(' · ')}</p>}
      <p className="text-xs text-fg-subtle">
        {revoked && device.revoked_at != null
          ? t.row.revokedOn(formatFullDate(device.revoked_at))
          : `${used} · ${t.row.made(formatFullDate(device.created_at))}`}
      </p>
      <ConfirmDialog
        open={revoking}
        onOpenChange={setRevoking}
        title={t.revoke.title(device.name)}
        description={t.revoke.lead}
        action={t.revoke.action}
        busyAction={t.revoke.busy}
        onConfirm={async () => {
          await revokeDevice(device.id)
          await queryClient.invalidateQueries({ queryKey: devicesQuery.queryKey })
        }}
      />
    </li>
  )
}

type Editing = { device?: Device }

export function DevicesPage() {
  const devices = useQuery(devicesQuery)
  const setup = useQuery(deviceSetupQuery)
  const archives = useQuery(archivesQuery).data ?? []
  const email = useQuery(sessionQuery).data?.email ?? ''
  const [editing, setEditing] = useState<Editing | null>(null)
  const [created, setCreated] = useState<CreatedDevice | null>(null)
  const servers = setup.data?.servers ?? []
  const list = devices.data ?? []
  const active = list.filter((device) => device.revoked_at == null)
  const revoked = list.filter((device) => device.revoked_at != null)
  const canCreate = setup.data?.can_create === true
  return (
    <SettingsPage
      title={t.nav}
      lead={t.lead}
      action={
        canCreate && (
          <Button onClick={() => setEditing({})}>
            <PlusIcon />
            {t.add}
          </Button>
        )
      }
    >
      {setup.data && !canCreate && <p className="text-sm text-fg-muted">{t.noCreate}</p>}
      <section className="grid gap-3" aria-labelledby="device-servers">
        <h3 id="device-servers" className="text-base font-normal">
          {t.servers.heading}
        </h3>
        {setup.data && (
          <p className="text-sm text-fg-muted">
            {servers.length > 0 ? t.servers.lead : t.servers.none}
          </p>
        )}
        <Servers servers={servers} email={email} host={browserHost()} />
      </section>
      <section className="grid gap-3">
        {devices.isPending && <Skeleton className="h-24 w-full" />}
        {devices.isError && (
          <p role="alert" className="text-sm text-danger">
            {t.loadFailed}
          </p>
        )}
        {devices.data && active.length === 0 && <p className="text-sm text-fg-muted">{t.empty}</p>}
        {active.length > 0 && (
          <ul className="grid gap-3">
            {active.map((device) => (
              <Row
                key={device.id}
                device={device}
                archives={archives}
                onEdit={() => setEditing({ device })}
              />
            ))}
          </ul>
        )}
      </section>
      {revoked.length > 0 && (
        <section className="grid gap-3" aria-labelledby="device-revoked">
          <h3 id="device-revoked" className="text-base font-normal">
            {t.revokedHeading}
          </h3>
          <ul className="grid gap-3">
            {revoked.map((device) => (
              <Row key={device.id} device={device} archives={archives} onEdit={() => undefined} />
            ))}
          </ul>
        </section>
      )}
      {editing && (
        <DeviceDialog
          key={editing.device?.id ?? 'new'}
          device={editing.device}
          archives={archives}
          servers={servers}
          email={email}
          onClose={() => setEditing(null)}
          onCreated={(next) => {
            setEditing(null)
            setCreated(next)
          }}
        />
      )}
      {created && (
        <SecretDialog
          created={created}
          servers={servers}
          email={email}
          onDone={() => setCreated(null)}
        />
      )}
    </SettingsPage>
  )
}
