import {
  type Archive,
  archivesQuery,
  changeFlow,
  createFlow,
  deleteFlow,
  type FlowRecord,
  flowsQuery,
  rulesCatalogueQuery,
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
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Skeleton,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import { ArrowDownIcon, ArrowUpIcon, EllipsisIcon, PlusIcon, Trash2Icon } from 'lucide-react'
import { type FormEvent, useMemo, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { NativeSelect } from '../items/inputs.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import {
  docsOf,
  inRunOrder,
  mayWriteRules,
  newFlow,
  type NodeType,
  nodeTypeOf,
  readCatalogue,
  triggers,
} from '../lib/rules.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import { Picker } from './aliases-page.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.rules.page
const locale = currentLocale()
const modes = ['test', 'on', 'off'] as const

function useCatalogue(): NodeType[] {
  const data = useQuery(rulesCatalogueQuery).data
  return useMemo(() => readCatalogue(data), [data])
}

function FlowRow({
  flow,
  index,
  count,
  catalogue,
  onMove,
}: {
  flow: FlowRecord
  index: number
  count: number
  catalogue: NodeType[]
  onMove: (flow: FlowRecord, delta: number) => void
}) {
  const queryClient = useQueryClient()
  const [removing, setRemoving] = useState(false)
  const refresh = () =>
    queryClient.invalidateQueries({ queryKey: ['archives', flow.archive, 'flows'] })
  const mode = useMutation({
    mutationFn: (next: string) => changeFlow(flow.id, { mode: next }),
    onSuccess: refresh,
  })
  const trigger = docsOf(nodeTypeOf(catalogue, flow.trigger), locale, flow.trigger).title
  return (
    <li className="grid gap-2 py-3">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
        <span className="w-6 font-mono text-xs text-fg-subtle tabular-nums">
          {t.order(index + 1)}
        </span>
        <div className="grid min-w-0 flex-1 gap-1">
          <Link
            to="/rules/$flowId"
            params={{ flowId: flow.id }}
            aria-label={t.openLabel(flow.name)}
            className="truncate text-sm text-fg underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-focus"
          >
            {flow.name}
          </Link>
          <span className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-fg-muted">
            <span>
              <span className="text-fg-subtle">{strings.rules.inspector.trigger}: </span>
              {trigger}
            </span>
            <span className="text-fg-subtle">{t.version(flow.version)}</span>
            <span className="text-fg-subtle">{t.updated(formatFullDate(flow.updated_at))}</span>
          </span>
        </div>
        <NativeSelect
          label={t.mode}
          hideLabel
          value={flow.mode}
          choices={modes.map((m) => ({ value: m, label: t.modes[m] ?? m }))}
          onChange={(next) => mode.mutate(next)}
        />
        <Button variant="secondary" size="sm" asChild>
          <Link to="/rules/$flowId" params={{ flowId: flow.id }}>
            {t.open}
          </Link>
        </Button>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant="ghost" size="icon" aria-label={t.menu(flow.name)}>
              <EllipsisIcon />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end">
            <DropdownMenuItem disabled={index === 0} onSelect={() => onMove(flow, -1)}>
              <ArrowUpIcon />
              {t.moveUp}
            </DropdownMenuItem>
            <DropdownMenuItem disabled={index === count - 1} onSelect={() => onMove(flow, 1)}>
              <ArrowDownIcon />
              {t.moveDown}
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => setRemoving(true)}>
              <Trash2Icon />
              {t.remove}
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      <p className="pl-10 text-xs text-fg-subtle">
        {flow.paused ? <Badge variant="warning">{t.paused}</Badge> : (t.modeLead[flow.mode] ?? '')}
      </p>
      {mode.isError && (
        <p role="alert" className="pl-10 text-xs text-danger">
          {authErrorMessage(mode.error)}
        </p>
      )}
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.removeTitle(flow.name)}
        description={t.removeLead}
        action={t.remove}
        busyAction={t.removing}
        onConfirm={async () => {
          await deleteFlow(flow.id)
          await refresh()
        }}
      />
    </li>
  )
}

function NewFlowDialog({
  archiveId,
  catalogue,
  open,
  onOpenChange,
}: {
  archiveId: string
  catalogue: NodeType[]
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const navigate = useNavigate()
  const choices = triggers(catalogue)
  const [name, setName] = useState('')
  const [trigger, setTrigger] = useState('message.archived')
  const create = useMutation({
    mutationFn: async () => {
      const type = nodeTypeOf(catalogue, trigger) ?? choices[0]
      if (!type) throw new Error(t.changeFailed)
      return createFlow(archiveId, name.trim(), newFlow(type))
    },
    onSuccess: (flow) => navigate({ to: '/rules/$flowId', params: { flowId: flow.id } }),
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (name.trim()) create.mutate()
  }
  const chosen = nodeTypeOf(catalogue, trigger)
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) create.reset()
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.newTitle}</DialogTitle>
            <DialogDescription>{t.newLead}</DialogDescription>
          </DialogHeader>
          <Field
            label={t.name}
            value={name}
            maxLength={100}
            onChange={(event) => setName(event.target.value)}
            autoComplete="off"
            autoFocus
            required
          />
          <div className="grid gap-2">
            <NativeSelect
              label={t.trigger}
              value={trigger}
              choices={choices.map((c) => ({ value: c.id, label: docsOf(c, locale).title }))}
              onChange={setTrigger}
            />
            {chosen && <p className="text-xs text-fg-subtle">{docsOf(chosen, locale).summary}</p>}
          </div>
          <FormError message={create.isError ? authErrorMessage(create.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {strings.connect.cancel}
            </Button>
            <Button
              type="submit"
              variant="primary"
              disabled={!name.trim() || create.isPending}
              aria-busy={create.isPending}
            >
              {create.isPending ? t.creating : t.create}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function ArchiveFlows({ archive }: { archive: Archive }) {
  const queryClient = useQueryClient()
  const flows = useQuery(flowsQuery(archive.id))
  const catalogue = useCatalogue()
  const move = useMutation({
    mutationFn: ({ flow, position }: { flow: FlowRecord; position: number }) =>
      changeFlow(flow.id, { position }),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['archives', archive.id, 'flows'] }),
  })
  if (!mayWriteRules(archive)) return <p className="text-sm text-fg-muted">{t.noRights}</p>
  if (flows.isPending) return <Skeleton className="h-32" />
  if (flows.isError) {
    return (
      <p role="alert" className="text-sm text-danger">
        {flows.error.message || t.listFailed}
      </p>
    )
  }
  const ordered = inRunOrder(flows.data)
  if (ordered.length === 0) return <p className="text-sm text-fg-muted">{t.empty}</p>
  return (
    <section className="grid gap-2" aria-label={t.count(ordered.length)}>
      <p className="text-xs text-fg-subtle">{t.count(ordered.length)}</p>
      <ul className="grid divide-y divide-border border-y border-border">
        {ordered.map((flow, index) => (
          <FlowRow
            key={flow.id}
            flow={flow}
            index={index}
            count={ordered.length}
            catalogue={catalogue}
            onMove={(f, delta) => {
              const target = ordered[index + delta]
              if (target) move.mutate({ flow: f, position: target.position })
            }}
          />
        ))}
      </ul>
      {move.isError && (
        <p role="alert" className="text-xs text-danger">
          {authErrorMessage(move.error)}
        </p>
      )}
    </section>
  )
}

export function RulesPage() {
  const archives = useQuery(archivesQuery)
  const search = useSearch({ from: '/settings/rules' })
  const navigate = useNavigate()
  const catalogue = useCatalogue()
  const [creating, setCreating] = useState(false)
  const list = archives.data ?? []
  const archive = list.find((a) => a.id === search.archive) ?? list.find(mayWriteRules) ?? list[0]
  return (
    <SettingsPage
      title={t.title}
      lead={t.lead}
      action={
        <div className="flex flex-wrap items-center gap-2">
          {list.length > 1 && archive && (
            <Picker
              label={t.archive}
              value={archive.id}
              choices={list.map((a) => ({ value: a.id, label: a.name }))}
              onChange={(id) => void navigate({ to: '/settings/rules', search: { archive: id } })}
            />
          )}
          {archive && mayWriteRules(archive) && catalogue.length > 0 && (
            <Button variant="primary" onClick={() => setCreating(true)}>
              <PlusIcon />
              {t.newFlow}
            </Button>
          )}
        </div>
      }
    >
      {archives.isPending ? (
        <Skeleton className="h-48" />
      ) : archive ? (
        <>
          <ArchiveFlows key={archive.id} archive={archive} />
          <NewFlowDialog
            key={`new-${archive.id}`}
            archiveId={archive.id}
            catalogue={catalogue}
            open={creating}
            onOpenChange={setCreating}
          />
        </>
      ) : (
        <p className="text-sm text-fg-muted">{t.noArchives}</p>
      )}
    </SettingsPage>
  )
}
