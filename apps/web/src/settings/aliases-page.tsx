import {
  addAliasService,
  addManualAlias,
  type Alias,
  aliasesQuery,
  aliasOptionsQuery,
  aliasProvidersQuery,
  type AliasProvider,
  type AliasService,
  archivesQuery,
  createAlias,
  removeAlias,
  removeAliasService,
  syncAliasService,
  updateAlias,
  updateAliasService,
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
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Skeleton,
  Switch,
  ToggleGroup,
  ToggleGroupItem,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import {
  ChevronDownIcon,
  EllipsisIcon,
  PlusIcon,
  RefreshCwIcon,
  SearchIcon,
  ShieldAlertIcon,
} from 'lucide-react'
import { type FormEvent, type ReactNode, useMemo, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import {
  ALL,
  type AliasSource,
  aliasProviders,
  aliasSearch,
  filterAliases,
  isAliasSource,
  providerName,
} from '../lib/aliases.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate, formatListDate } from '../lib/format.ts'
import { ConfirmDialog, TextDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.aliases

function Picker({
  label,
  value,
  choices,
  onChange,
}: {
  label: string
  value: string
  choices: readonly { value: string; label: string }[]
  onChange: (value: string) => void
}) {
  const current = choices.find((choice) => choice.value === value)
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="secondary" aria-label={label} className="max-w-full">
          <span className="truncate">{current?.label ?? label}</span>
          <ChevronDownIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        <DropdownMenuRadioGroup value={value} onValueChange={onChange}>
          {choices.map((choice) => (
            <DropdownMenuRadioItem key={choice.value} value={choice.value}>
              {choice.label}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function useRefresh(archiveId: string) {
  const queryClient = useQueryClient()
  return () => queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'aliases'] })
}

function ServiceCard({
  service,
  providers,
  archiveId,
}: {
  service: AliasService
  providers: readonly AliasProvider[] | undefined
  archiveId: string
}) {
  const refresh = useRefresh(archiveId)
  const [keyOpen, setKeyOpen] = useState(false)
  const [removing, setRemoving] = useState(false)
  const [keep, setKeep] = useState(true)
  const sync = useMutation({ mutationFn: () => syncAliasService(service.id), onSettled: refresh })
  const provider = providers?.find((p) => p.id === service.provider)
  return (
    <li className="grid gap-3 border border-border px-4 py-4">
      <div className="flex flex-wrap items-baseline gap-x-3 gap-y-1">
        <span className="text-base text-fg">{service.name}</span>
        <Badge variant="outline">{providerName(providers, service.provider)}</Badge>
        <span className="min-w-0 flex-1 truncate text-right font-mono text-xs text-fg-subtle">
          {service.base_url}
        </span>
      </div>
      <p className="text-xs text-fg-muted">
        {service.synced_at != null ? t.synced(formatFullDate(service.synced_at)) : t.neverSynced}
      </p>
      {service.sync_error && (
        <p role="alert" className="text-xs text-danger">
          {service.sync_error}
        </p>
      )}
      <FormError message={sync.isError ? authErrorMessage(sync.error) : undefined} />
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="ghost" disabled={sync.isPending} onClick={() => sync.mutate()}>
          <RefreshCwIcon />
          {sync.isPending ? t.syncing : t.syncNow}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setKeyOpen(true)}>
          {t.newKey}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => setRemoving(true)}>
          {t.removeService}
        </Button>
      </div>
      <TextDialog
        open={keyOpen}
        onOpenChange={setKeyOpen}
        title={t.newKeyTitle(service.name)}
        description={provider?.key_help}
        label={t.key(provider?.key_label ?? '')}
        password
        onSave={async (key) => {
          await updateAliasService(service.id, { key })
          await refresh()
        }}
      />
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.removeService}
        description={
          <span className="grid gap-3">
            <span>{t.removeServiceConfirm(service.name)}</span>
            <label className="flex items-center gap-2 text-sm text-fg">
              <Switch checked={keep} onCheckedChange={setKeep} />
              {t.keepAliases}
            </label>
          </span>
        }
        action={t.removeService}
        busyAction={t.removeService}
        onConfirm={async () => {
          await removeAliasService(service.id, keep)
          await refresh()
        }}
      />
    </li>
  )
}

function ConnectService({
  archiveId,
  providers,
}: {
  archiveId: string
  providers: readonly AliasProvider[]
}) {
  const refresh = useRefresh(archiveId)
  const usable = providers.filter((p) => p.capabilities)
  const [provider, setProvider] = useState(usable[0]?.id ?? '')
  const [name, setName] = useState('')
  const [key, setKey] = useState('')
  const [url, setUrl] = useState('')
  const chosen = usable.find((p) => p.id === provider)
  const connect = useMutation({
    mutationFn: () =>
      addAliasService(archiveId, {
        provider,
        name: name.trim() || null,
        base_url: url.trim() || null,
        key: key.trim(),
      }),
    onSuccess: async () => {
      setKey('')
      setName('')
      setUrl('')
      await refresh()
    },
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (provider && key.trim()) connect.mutate()
  }
  if (usable.length === 0) return null
  return (
    <form className="grid max-w-xl gap-4" onSubmit={submit} aria-labelledby="alias-connect">
      <h4 id="alias-connect" className="text-base font-normal">
        {t.connect}
      </h4>
      <div className="grid gap-2">
        <span className="text-xs text-fg-muted">{t.provider}</span>
        <ToggleGroup
          type="single"
          value={provider}
          onValueChange={(value) => value && setProvider(value)}
          aria-label={t.provider}
          className="flex w-fit flex-wrap"
        >
          {usable.map((p) => (
            <ToggleGroupItem key={p.id} value={p.id}>
              {p.name}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>
      <PasswordField
        label={t.key(chosen?.key_label ?? '')}
        hint={chosen?.key_help}
        autoComplete="off"
        required
        value={key}
        onChange={(event) => setKey(event.target.value)}
      />
      <Field
        label={t.name}
        placeholder={chosen?.name ?? t.namePlaceholder}
        autoComplete="off"
        value={name}
        onChange={(event) => setName(event.target.value)}
      />
      <Field
        label={t.baseUrl}
        placeholder={chosen?.default_url ?? ''}
        hint={t.baseUrlHint}
        autoComplete="off"
        spellCheck={false}
        value={url}
        onChange={(event) => setUrl(event.target.value)}
      />
      <FormError message={connect.isError ? authErrorMessage(connect.error) : undefined} />
      <div>
        <SubmitButton busy={connect.isPending} label={t.connect} busyLabel={t.connecting} />
      </div>
    </form>
  )
}

function Fact({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 gap-2 text-xs">
      <dt className="w-28 shrink-0 text-fg-subtle">{label}</dt>
      <dd className="min-w-0 text-fg-muted wrap-break-word">{children}</dd>
    </div>
  )
}

function AliasRow({
  alias,
  archiveId,
  providers,
  service,
}: {
  alias: Alias
  archiveId: string
  providers: readonly AliasProvider[] | undefined
  service: AliasService | undefined
}) {
  const refresh = useRefresh(archiveId)
  const [editing, setEditing] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const caps = service?.capabilities
  const toggle = useMutation({
    mutationFn: (enabled: boolean) => updateAlias(archiveId, alias.address, { enabled }),
    onSettled: refresh,
  })
  const now = Date.now()
  const unsigned = alias.source !== 'mail' ? 0 : alias.messages - alias.signed
  const enabled = toggle.isPending ? toggle.variables : alias.enabled
  return (
    <li className="grid gap-2 border-b border-border px-1 py-3 last:border-b-0">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="min-w-0 font-mono text-sm break-all text-fg">{alias.address}</span>
        <Badge variant="outline">{providerName(providers, alias.provider)}</Badge>
        {alias.source !== 'service' && <Badge variant="neutral">{t.sources[alias.source]}</Badge>}
        {unsigned > 0 && (
          <Badge variant="warning" title={t.unverifiedHint}>
            <ShieldAlertIcon />
            {t.unverified(unsigned)}
          </Badge>
        )}
        <span className="flex-1" />
        {caps?.switch && enabled != null && (
          <label className="flex items-center gap-2 text-xs text-fg-muted">
            <Switch
              checked={enabled}
              onCheckedChange={(next) => toggle.mutate(next)}
              aria-label={t.forwarding}
            />
            {enabled ? t.forwarding : t.forwardingOff}
          </label>
        )}
        {!caps?.switch && alias.enabled === false && (
          <Badge variant="danger">{t.forwardingOff}</Badge>
        )}
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button size="icon" variant="ghost" aria-label={t.actions(alias.address)}>
              <EllipsisIcon />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem asChild>
              <Link
                to="/a/$archiveId/search"
                params={{ archiveId }}
                search={{ q: aliasSearch(alias.address) }}
              >
                {t.showMail}
              </Link>
            </DropdownMenuItem>
            {alias.source !== 'mail' && (
              <DropdownMenuItem onSelect={() => setEditing(true)}>{t.edit}</DropdownMenuItem>
            )}
            {alias.source !== 'mail' && (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuItem onSelect={() => setDeleting(true)}>{t.delete}</DropdownMenuItem>
              </>
            )}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {alias.note && <p className="text-sm text-fg-muted">{alias.note}</p>}
      <dl className="grid gap-0.5">
        <Fact label={strings.reader.alias}>
          {t.messages(alias.messages)}
          {alias.senders > 0 && ` · ${t.senders(alias.senders)}`}
          {alias.last_at != null && ` · ${t.lastMail(formatListDate(alias.last_at, now))}`}
          {alias.forwarded != null && ` · ${t.serviceCounts(alias.forwarded, alias.blocked ?? 0)}`}
        </Fact>
        {alias.for_domain && <Fact label={t.website}>{alias.for_domain}</Fact>}
        {alias.name && <Fact label={t.replyName}>{alias.name}</Fact>}
        {alias.mailboxes.length > 0 && (
          <Fact label={t.deliversTo}>
            <span className="font-mono">{alias.mailboxes.join(', ')}</span>
          </Fact>
        )}
        {alias.accounts.length > 0 && (
          <Fact label={t.archivedFrom}>
            {alias.accounts
              .map((account) => `${account.name} (${account.messages.toLocaleString()})`)
              .join(', ')}
          </Fact>
        )}
      </dl>
      <FormError message={toggle.isError ? authErrorMessage(toggle.error) : undefined} />
      {editing && (
        <EditAlias
          alias={alias}
          archiveId={archiveId}
          service={service}
          onClose={() => setEditing(false)}
        />
      )}
      <ConfirmDialog
        open={deleting}
        onOpenChange={setDeleting}
        title={t.deleteTitle(alias.address)}
        description={caps?.delete ? t.deleteAtService : t.deleteHere}
        action={t.deleteConfirm}
        busyAction={t.deleteConfirm}
        typeToConfirm={caps?.delete ? alias.address : undefined}
        onConfirm={async () => {
          await removeAlias(archiveId, alias.address, Boolean(caps?.delete))
          await refresh()
        }}
      />
    </li>
  )
}

function EditAlias({
  alias,
  archiveId,
  service,
  onClose,
}: {
  alias: Alias
  archiveId: string
  service: AliasService | undefined
  onClose: () => void
}) {
  const refresh = useRefresh(archiveId)
  const caps = service?.capabilities
  const manual = alias.source === 'manual'
  const options = useQuery({
    ...aliasOptionsQuery(service?.id ?? ''),
    enabled: Boolean(service && caps?.mailboxes),
  })
  const [note, setNote] = useState(alias.note ?? '')
  const [name, setName] = useState(alias.name ?? '')
  const [site, setSite] = useState(alias.for_domain ?? '')
  const [boxes, setBoxes] = useState<string[] | undefined>()
  const chosenBoxes =
    boxes ??
    (options.data?.mailboxes ?? [])
      .filter((box) => alias.mailboxes.includes(box.email))
      .map((box) => box.id)
  const save = useMutation({
    mutationFn: () =>
      updateAlias(archiveId, alias.address, {
        note: manual || caps?.note ? note.trim() : null,
        name: caps?.name ? name.trim() : null,
        for_domain: manual || caps?.for_domain ? site.trim() : null,
        mailbox_ids: caps?.mailboxes && boxes ? boxes : null,
      }),
    onSuccess: async () => {
      await refresh()
      onClose()
    },
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="grid gap-6"
          onSubmit={(event) => {
            event.preventDefault()
            save.mutate()
          }}
        >
          <DialogHeader>
            <DialogTitle>{t.editTitle(alias.address)}</DialogTitle>
          </DialogHeader>
          {(manual || caps?.note) && (
            <Field label={t.note} value={note} onChange={(e) => setNote(e.target.value)} />
          )}
          {caps?.name && (
            <Field label={t.replyName} value={name} onChange={(e) => setName(e.target.value)} />
          )}
          {(manual || caps?.for_domain) && (
            <Field label={t.website} value={site} onChange={(e) => setSite(e.target.value)} />
          )}
          {caps?.mailboxes && (
            <Mailboxes
              loading={options.isPending}
              mailboxes={options.data?.mailboxes ?? []}
              chosen={chosenBoxes}
              onChange={setBoxes}
            />
          )}
          <FormError message={save.isError ? authErrorMessage(save.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {strings.connect.cancel}
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

function Mailboxes({
  loading,
  mailboxes,
  chosen,
  onChange,
}: {
  loading: boolean
  mailboxes: readonly { id: string; email: string }[]
  chosen: readonly string[]
  onChange: (next: string[]) => void
}) {
  if (loading) return <Skeleton className="h-12" />
  if (mailboxes.length === 0) return null
  return (
    <fieldset className="grid gap-2">
      <legend className="mb-1 text-xs text-fg-muted">{t.mailboxes}</legend>
      {mailboxes.map((box) => (
        <label key={box.id} className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={chosen.includes(box.id)}
            onChange={(event) =>
              onChange(
                event.target.checked ? [...chosen, box.id] : chosen.filter((id) => id !== box.id),
              )
            }
          />
          <span className="font-mono text-xs">{box.email}</span>
        </label>
      ))}
    </fieldset>
  )
}

function CreateAlias({
  archiveId,
  services,
  onClose,
}: {
  archiveId: string
  services: readonly AliasService[]
  onClose: () => void
}) {
  const refresh = useRefresh(archiveId)
  const [serviceId, setServiceId] = useState(services[0]?.id ?? '')
  const service = services.find((s) => s.id === serviceId)
  const caps = service?.capabilities
  const options = useQuery({ ...aliasOptionsQuery(serviceId), enabled: Boolean(serviceId) })
  const [mode, setMode] = useState<'random' | 'custom'>(caps?.create_random ? 'random' : 'custom')
  const [prefix, setPrefix] = useState('')
  const [domain, setDomain] = useState<string>()
  const [note, setNote] = useState('')
  const [name, setName] = useState('')
  const [site, setSite] = useState('')
  const [boxes, setBoxes] = useState<string[]>([])
  const custom = mode === 'custom' && Boolean(caps?.create_custom)
  const domains = options.data?.domains ?? []
  const create = useMutation({
    mutationFn: () =>
      createAlias(serviceId, {
        prefix: custom ? prefix.trim() : null,
        domain: custom ? (domain ?? domains[0]?.id ?? null) : null,
        note: note.trim() || null,
        name: caps?.name ? name.trim() || null : null,
        for_domain: caps?.for_domain ? site.trim() || null : null,
        mailbox_ids: caps?.mailboxes ? boxes : [],
      }),
    onSuccess: async () => {
      await refresh()
      onClose()
    },
  })
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="grid gap-6"
          onSubmit={(event) => {
            event.preventDefault()
            if (!custom || prefix.trim()) create.mutate()
          }}
        >
          <DialogHeader>
            <DialogTitle>{t.createTitle}</DialogTitle>
            {options.data && !options.data.can_create && (
              <DialogDescription>{t.cannotCreate}</DialogDescription>
            )}
          </DialogHeader>
          {services.length > 1 && (
            <div className="grid gap-2">
              <span className="text-xs text-fg-muted">{t.createAt}</span>
              <Picker
                label={t.createAt}
                value={serviceId}
                choices={services.map((s) => ({ value: s.id, label: s.name }))}
                onChange={(value) => {
                  setServiceId(value)
                  setDomain(undefined)
                  setBoxes([])
                }}
              />
            </div>
          )}
          {caps?.create_random && caps.create_custom && (
            <ToggleGroup
              type="single"
              value={mode}
              onValueChange={(value) =>
                (value === 'random' || value === 'custom') && setMode(value)
              }
              className="w-fit"
            >
              <ToggleGroupItem value="random">{t.random}</ToggleGroupItem>
              <ToggleGroupItem value="custom">{t.custom}</ToggleGroupItem>
            </ToggleGroup>
          )}
          {custom && (
            <div className="grid gap-4 sm:grid-cols-[1fr_auto] sm:items-end">
              <Field
                label={t.prefix}
                placeholder={options.data?.prefix_hint ?? ''}
                autoComplete="off"
                spellCheck={false}
                required
                value={prefix}
                onChange={(e) => setPrefix(e.target.value)}
              />
              {domains.length > 0 && (
                <Picker
                  label={t.domain}
                  value={domain ?? domains[0]?.id ?? ''}
                  choices={domains.map((d) => ({ value: d.id, label: d.label }))}
                  onChange={setDomain}
                />
              )}
            </div>
          )}
          {caps?.note && (
            <Field label={t.note} value={note} onChange={(e) => setNote(e.target.value)} />
          )}
          {caps?.name && (
            <Field label={t.replyName} value={name} onChange={(e) => setName(e.target.value)} />
          )}
          {caps?.for_domain && (
            <Field
              label={t.website}
              placeholder="https://shop.example.com"
              value={site}
              onChange={(e) => setSite(e.target.value)}
            />
          )}
          {caps?.mailboxes && (
            <Mailboxes
              loading={options.isPending}
              mailboxes={options.data?.mailboxes ?? []}
              chosen={boxes}
              onChange={setBoxes}
            />
          )}
          <FormError
            message={
              create.isError
                ? authErrorMessage(create.error)
                : options.isError
                  ? t.loadOptionsFailed
                  : undefined
            }
          />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={onClose}>
              {strings.connect.cancel}
            </Button>
            <Button
              type="submit"
              variant="primary"
              disabled={create.isPending || !serviceId || options.data?.can_create === false}
            >
              {create.isPending ? t.creating : t.create}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ManualAlias({
  archiveId,
  providers,
}: {
  archiveId: string
  providers: readonly AliasProvider[]
}) {
  const refresh = useRefresh(archiveId)
  const [address, setAddress] = useState('')
  const [provider, setProvider] = useState('apple')
  const [other, setOther] = useState('')
  const [note, setNote] = useState('')
  const add = useMutation({
    mutationFn: () =>
      addManualAlias(archiveId, {
        address: address.trim(),
        provider: provider === 'other' ? other.trim().toLowerCase() : provider,
        note: note.trim() || null,
      }),
    onSuccess: async () => {
      setAddress('')
      setNote('')
      await refresh()
    },
  })
  return (
    <form
      className="grid max-w-xl gap-4"
      aria-labelledby="alias-manual"
      onSubmit={(event) => {
        event.preventDefault()
        if (address.trim()) add.mutate()
      }}
    >
      <div className="grid gap-1">
        <h4 id="alias-manual" className="text-base font-normal">
          {t.manual}
        </h4>
        <p className="text-xs text-fg-subtle">{t.manualLead}</p>
      </div>
      <Field
        label={t.address}
        type="email"
        placeholder="quiet.river.42@icloud.com"
        autoComplete="off"
        spellCheck={false}
        required
        value={address}
        onChange={(e) => setAddress(e.target.value)}
      />
      <div className="grid gap-2">
        <span className="text-xs text-fg-muted">{t.manualProvider}</span>
        <Picker
          label={t.manualProvider}
          value={provider}
          choices={[
            ...providers.map((p) => ({ value: p.id, label: p.name })),
            { value: 'other', label: t.otherProvider },
          ]}
          onChange={setProvider}
        />
      </div>
      {provider === 'other' && (
        <Field
          label={t.manualProvider}
          placeholder="own-domain"
          autoComplete="off"
          spellCheck={false}
          required
          value={other}
          onChange={(e) => setOther(e.target.value)}
        />
      )}
      <Field label={t.note} value={note} onChange={(e) => setNote(e.target.value)} />
      <FormError message={add.isError ? authErrorMessage(add.error) : undefined} />
      <div>
        <SubmitButton busy={add.isPending} label={t.add} busyLabel={t.adding} />
      </div>
    </form>
  )
}

function ArchiveAliases({ archiveId }: { archiveId: string }) {
  const list = useQuery(aliasesQuery(archiveId))
  const providers = useQuery(aliasProvidersQuery).data
  const [text, setText] = useState('')
  const [source, setSource] = useState<AliasSource | 'all'>(ALL)
  const [provider, setProvider] = useState(ALL)
  const [creating, setCreating] = useState(false)
  const aliases = useMemo(() => list.data?.aliases ?? [], [list.data])
  const services = list.data?.services ?? []
  const creatable = services.filter(
    (s) => s.capabilities?.create_random || s.capabilities?.create_custom,
  )
  const shown = useMemo(
    () => filterAliases(aliases, { text, source, provider }),
    [aliases, text, source, provider],
  )
  if (list.isPending) return <Skeleton className="h-48" />
  if (list.isError) return <FormError message={authErrorMessage(list.error)} />
  return (
    <>
      <section aria-labelledby="alias-services" className="grid gap-4">
        <div className="grid gap-1">
          <h3 id="alias-services" className="text-lg font-normal">
            {t.services}
          </h3>
          <p className="text-sm text-fg-muted">{t.servicesLead}</p>
        </div>
        {services.length > 0 ? (
          <ul className="grid gap-4">
            {services.map((service) => (
              <ServiceCard
                key={service.id}
                service={service}
                providers={providers}
                archiveId={archiveId}
              />
            ))}
          </ul>
        ) : (
          <p className="text-sm text-fg-muted">{t.noServices}</p>
        )}
        {providers && <ConnectService archiveId={archiveId} providers={providers} />}
      </section>

      <section aria-labelledby="alias-list" className="grid gap-4">
        <div className="flex flex-wrap items-end justify-between gap-4">
          <h3 id="alias-list" className="text-lg font-normal">
            {t.list}
          </h3>
          {creatable.length > 0 && (
            <Button variant="primary" onClick={() => setCreating(true)}>
              <PlusIcon />
              {t.create}
            </Button>
          )}
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="relative min-w-60 flex-1">
            <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-fg-subtle" />
            <Input
              type="search"
              aria-label={t.search}
              placeholder={t.search}
              value={text}
              onChange={(e) => setText(e.target.value)}
              className="h-8 pl-9"
            />
          </div>
          <Picker
            label={t.sources.all ?? ''}
            value={source}
            choices={(['all', 'service', 'manual', 'mail'] as const).map((value) => ({
              value,
              label: t.sources[value] ?? value,
            }))}
            onChange={(value) => isAliasSource(value) && setSource(value)}
          />
          <Picker
            label={t.allProviders}
            value={provider}
            choices={[
              { value: ALL, label: t.allProviders },
              ...aliasProviders(aliases).map((id) => ({
                value: id,
                label: providerName(providers, id),
              })),
            ]}
            onChange={setProvider}
          />
        </div>
        {aliases.length === 0 ? (
          <p className="text-sm text-fg-muted">{t.empty}</p>
        ) : shown.length === 0 ? (
          <p className="text-sm text-fg-muted">{t.nothingFound}</p>
        ) : (
          <ul className="border-t border-border">
            {shown.map((alias) => (
              <AliasRow
                key={alias.address}
                alias={alias}
                archiveId={archiveId}
                providers={providers}
                service={services.find((s) => s.id === alias.service_id)}
              />
            ))}
          </ul>
        )}
        {creating && (
          <CreateAlias
            archiveId={archiveId}
            services={creatable}
            onClose={() => setCreating(false)}
          />
        )}
      </section>

      {providers && (
        <section className="grid gap-4">
          <ManualAlias archiveId={archiveId} providers={providers} />
        </section>
      )}
    </>
  )
}

export function AliasesPage() {
  const archives = useQuery(archivesQuery)
  const [chosen, setChosen] = useState<string>()
  const list = archives.data ?? []
  const archiveId = chosen ?? list[0]?.id
  return (
    <SettingsPage
      title={t.title}
      lead={t.lead}
      action={
        list.length > 1 && archiveId ? (
          <Picker
            label={t.archive}
            value={archiveId}
            choices={list.map((a) => ({ value: a.id, label: a.name }))}
            onChange={setChosen}
          />
        ) : undefined
      }
    >
      {archives.isPending ? (
        <Skeleton className="h-48" />
      ) : archiveId ? (
        <ArchiveAliases key={archiveId} archiveId={archiveId} />
      ) : (
        <p className="text-sm text-fg-muted">{t.empty}</p>
      )}
    </SettingsPage>
  )
}
