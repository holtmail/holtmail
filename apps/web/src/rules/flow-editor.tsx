import {
  ApiError,
  archivesQuery,
  changeFlow,
  type FlowIssue,
  type FlowRecord,
  flowIssues,
  flowQuery,
  rulesCatalogueQuery,
  saveFlow,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton, ToggleGroup, ToggleGroupItem } from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useBlocker, useParams } from '@tanstack/react-router'
import { ReactFlowProvider } from '@xyflow/react'
import { ArrowLeftIcon } from 'lucide-react'
import { useMemo, useState } from 'react'

import { useLayout } from '../hooks/use-layout.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { chainOf } from '../lib/rules-list.ts'
import { highlightOf, type PlanView } from '../lib/rules-plan.ts'
import {
  addNode,
  changed,
  COLUMN,
  connect,
  disconnect,
  type FlowDefinition,
  issuesByNode,
  layout,
  moveNode,
  type NodeType,
  nodeTypeOf,
  type Point,
  readCatalogue,
  readFlow,
  removeNode,
  ROW,
  setConfirm,
  setParams,
  setTrigger,
  TRIGGER_ID,
} from '../lib/rules.ts'
import { strings } from '../strings.ts'
import { Canvas } from './canvas.tsx'
import { MessagePicker, type PreviewMessage } from './fields.tsx'
import { Inspector } from './inspector.tsx'
import { JsonPanel } from './json-panel.tsx'
import { ListView } from './list-view.tsx'
import { ManualRun } from './manual-run.tsx'
import { Palette } from './palette.tsx'
import { RunsPanel } from './runs-panel.tsx'
import { TestPanel, type TestResult, testIsCurrent } from './test-panel.tsx'
import { VersionsPanel } from './versions-panel.tsx'

const t = strings.rules

type Tab = 'flow' | 'runs' | 'versions' | 'json'
type View = 'list' | 'canvas'
type Aside = 'step' | 'test'

/** A run's path shown on the flow, with the version it ran and the editor's flow when shown. */
interface ShownRun {
  plan: PlanView
  version: number
  flow: FlowDefinition
}
const tabs: readonly Tab[] = ['flow', 'runs', 'versions', 'json']

function positionOf(flow: FlowDefinition, id: string): Point {
  const placed = layout(flow)
  if (id === TRIGGER_ID) return placed.trigger.position ?? { x: 0, y: 0 }
  return placed.nodes.find((n) => n.id === id)?.position ?? { x: 0, y: 0 }
}

/** Where a step added from the palette goes: right of the selected step (or the trigger),
 * below the steps already linked from it, and linked from its first free port. */
function placeAfter(
  flow: FlowDefinition,
  catalogue: NodeType[],
  anchor: string,
  type: NodeType,
): { flow: FlowDefinition; id: string } {
  const from = positionOf(flow, anchor)
  const node = flow.nodes.find((n) => n.id === anchor)
  const ports =
    anchor === TRIGGER_ID ? ['next'] : (nodeTypeOf(catalogue, node?.type ?? '')?.ports ?? [])
  const links: Record<string, string | undefined> =
    anchor === TRIGGER_ID ? { next: flow.trigger.next } : (node?.next ?? {})
  const linked = ports.filter((p) => links[p] !== undefined).length
  const added = addNode(flow, type, { x: from.x + COLUMN, y: from.y + linked * ROW })
  const free = ports.find((p) => links[p] === undefined)
  return {
    flow: free ? connect(added.flow, anchor, free, added.id) : added.flow,
    id: added.id,
  }
}

function Editor({
  record,
  catalogue,
  onReload,
}: {
  record: FlowRecord
  catalogue: NodeType[]
  onReload: () => Promise<unknown>
}) {
  const queryClient = useQueryClient()
  const initial = useMemo(() => readFlow(record.flow), [record.flow])
  const [base, setBase] = useState(initial)
  const [flow, setFlow] = useState(initial)
  const [version, setVersion] = useState(record.version)
  const [selected, setSelected] = useState<string | null>(TRIGGER_ID)
  const [issues, setIssues] = useState<FlowIssue[]>([])
  const [conflict, setConflict] = useState(false)
  const [message, setMessage] = useState<PreviewMessage | null>(null)
  const [tab, setTab] = useState<Tab>('flow')
  const [view, setView] = useState<View>(() =>
    initial && chainOf(initial, catalogue) ? 'list' : 'canvas',
  )
  const [aside, setAside] = useState<Aside>('step')
  const [tested, setTested] = useState<TestResult | null>(null)
  const [runPlan, setRunPlan] = useState<ShownRun | null>(null)
  const archives = useQuery(archivesQuery)
  const role = archives.data?.find((a) => a.id === record.archive)?.rights.role ?? 'sender'
  const narrow = useLayout() === 'narrow'
  const dirty = changed(flow, base)

  useBlocker({
    shouldBlockFn: () => dirty && !window.confirm(t.editor.leave),
    enableBeforeUnload: () => dirty,
  })

  const save = useMutation({
    mutationFn: (definition: FlowDefinition) => saveFlow(record.id, version, definition),
    onSuccess: async (saved, definition) => {
      setBase(definition)
      setVersion(saved.version)
      setIssues([])
      queryClient.setQueryData(flowQuery(record.id).queryKey, saved)
      await queryClient.invalidateQueries({ queryKey: ['archives', record.archive, 'flows'] })
    },
    onError: (error) => {
      const found = flowIssues(error)
      if (found) setIssues(found)
      if (error instanceof ApiError && error.status === 409) setConflict(true)
    },
  })

  const mode = useMutation({
    mutationFn: (next: string) => changeFlow(record.id, { mode: next }),
    onSuccess: async (saved) => {
      queryClient.setQueryData(flowQuery(record.id).queryKey, saved)
      await queryClient.invalidateQueries({ queryKey: ['archives', record.archive, 'flows'] })
    },
  })

  if (!flow) return <p className="p-6 text-sm text-danger">{t.editor.loadFailed}</p>

  const byNode = issuesByNode(issues)
  const selectedType =
    selected === TRIGGER_ID
      ? nodeTypeOf(catalogue, flow.trigger.type)
      : nodeTypeOf(catalogue, flow.nodes.find((n) => n.id === selected)?.type ?? '')
  const hasTemplates = (selectedType?.templates.length ?? 0) > 0
  const flowWide = byNode.get('') ?? []
  const update = (change: (current: FlowDefinition) => FlowDefinition) => {
    setFlow((current) => (current ? change(current) : current))
    setRunPlan(null)
  }
  const chain = chainOf(flow, catalogue)
  const shown: View = narrow || (view === 'list' && chain) ? 'list' : 'canvas'
  const runShown =
    runPlan && runPlan.version === version && !dirty && !changed(runPlan.flow, flow)
      ? runPlan.plan
      : null
  const activePlan = tested && testIsCurrent(tested, flow) && !tested.error ? tested.plan : runShown
  const path = activePlan ? highlightOf(activePlan) : null
  const context = { archiveId: record.archive, trigger: flow.trigger.type, message }
  const replace = (next: FlowDefinition) => {
    setFlow(next)
    setSelected(TRIGGER_ID)
    setRunPlan(null)
    setTab('flow')
  }
  const testPanel = (
    <TestPanel
      flowId={record.id}
      archiveId={record.archive}
      flow={flow}
      catalogue={catalogue}
      message={message}
      onMessage={setMessage}
      result={tested}
      onResult={(result) => {
        setTested(result)
        setRunPlan(null)
      }}
      onIssues={setIssues}
    />
  )

  return (
    <div className="flex h-full flex-col bg-bg text-fg">
      <header className="flex h-topbar shrink-0 items-center gap-3 border-b border-border px-2">
        <Button variant="ghost" asChild>
          <Link to="/settings/rules" search={{ archive: record.archive }}>
            <ArrowLeftIcon />
            {t.editor.back}
          </Link>
        </Button>
        <h1 className="min-w-0 truncate text-base">{record.name}</h1>
        <Badge variant={record.paused ? 'warning' : 'outline'}>
          {t.page.modes[record.mode] ?? record.mode}
        </Badge>
        <span className="flex-1" />
        {issues.length > 0 && (
          <span className="text-xs text-danger">{t.editor.issues(issues.length)}</span>
        )}
        <span aria-live="polite" className="text-xs text-fg-subtle max-sm:sr-only">
          {save.isPending ? t.editor.saving : dirty ? t.editor.unsaved : t.editor.saved}
        </span>
        <Button
          variant="primary"
          disabled={!dirty || save.isPending || conflict}
          aria-busy={save.isPending}
          onClick={() => save.mutate(flow)}
        >
          {save.isPending ? t.editor.saving : t.editor.save}
        </Button>
      </header>
      {(conflict || flowWide.length > 0 || (save.isError && !flowIssues(save.error))) && (
        <div role="alert" className="grid gap-1 border-b border-border px-4 py-2 text-sm">
          {conflict && (
            <p className="flex flex-wrap items-center gap-3 text-warning">
              {t.editor.conflict}
              <Button variant="secondary" size="sm" onClick={() => void onReload()}>
                {t.editor.reload}
              </Button>
            </p>
          )}
          {!conflict && save.isError && !flowIssues(save.error) && (
            <p className="text-danger">{authErrorMessage(save.error)}</p>
          )}
          {flowWide.map((issue, index) => (
            <p key={`${issue.code}-${index}`} className="text-danger">
              {t.editor.flowIssue}: <span className="font-mono">{issue.code}</span> {issue.detail}
            </p>
          ))}
        </div>
      )}
      {(record.mode !== 'on' || record.paused) && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-4 py-2 text-sm">
          <span className={record.paused ? 'text-warning' : 'text-fg-muted'}>
            {record.paused ? t.page.paused : record.mode === 'off' ? t.status.off : t.status.test}
          </span>
          <Button
            variant="secondary"
            size="sm"
            disabled={mode.isPending || (dirty && !record.paused)}
            aria-busy={mode.isPending}
            onClick={() => mode.mutate(record.paused ? record.mode : 'on')}
          >
            {mode.isPending
              ? t.status.turningOn
              : record.paused
                ? t.status.runAgain
                : t.status.turnOn}
          </Button>
          {dirty && !record.paused && (
            <span className="text-xs text-fg-subtle">{t.status.saveFirst}</span>
          )}
          {mode.isError && (
            <span role="alert" className="text-xs text-danger">
              {t.status.failed} {authErrorMessage(mode.error)}
            </span>
          )}
        </div>
      )}
      {tab === 'flow' && runPlan && !runShown && !testIsCurrent(tested, flow) && (
        <p className="border-b border-border px-4 py-2 text-xs text-warning">
          {strings.rules.runs.pathStale(runPlan.version)}
        </p>
      )}
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-2 py-1">
        <ToggleGroup
          type="single"
          value={tab}
          aria-label={t.tabs.label}
          onValueChange={(next) => {
            const found = tabs.find((x) => x === next)
            if (found) setTab(found)
          }}
        >
          {tabs.map((x) => (
            <ToggleGroupItem key={x} value={x}>
              {t.tabs[x]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        {tab === 'flow' && !narrow && (
          <>
            <span className="flex-1" />
            <ToggleGroup
              type="single"
              value={shown}
              aria-label={t.view.label}
              onValueChange={(next) => {
                if (next === 'list' || next === 'canvas') setView(next)
              }}
            >
              <ToggleGroupItem
                value="list"
                disabled={!chain}
                title={chain ? undefined : t.view.branches}
              >
                {t.view.list}
              </ToggleGroupItem>
              <ToggleGroupItem value="canvas">{t.view.canvas}</ToggleGroupItem>
            </ToggleGroup>
          </>
        )}
      </div>
      {tab === 'runs' && (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <RunsPanel
            flowId={record.id}
            archiveId={record.archive}
            catalogue={catalogue}
            onShowPath={(plan, ran) => {
              setRunPlan({ plan, version: ran, flow })
              setTested(null)
              setTab('flow')
            }}
          >
            {record.trigger === 'manual' && (
              <ManualRun flowId={record.id} archiveId={record.archive} dirty={dirty} />
            )}
          </RunsPanel>
        </div>
      )}
      {tab === 'versions' && (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <VersionsPanel flowId={record.id} current={version} onRestore={replace} />
        </div>
      )}
      {tab === 'json' && (
        <div className="min-h-0 flex-1 overflow-y-auto">
          <JsonPanel name={record.name} flow={flow} onImport={replace} />
        </div>
      )}
      {tab === 'flow' && shown === 'list' && (
        <div className="min-h-0 flex-1 overflow-y-auto">
          {chain ? (
            <ListView
              flow={flow}
              chain={chain}
              catalogue={catalogue}
              role={role}
              selected={selected}
              issues={byNode}
              context={context}
              plan={activePlan}
              onSelect={setSelected}
              onChange={update}
            >
              <div className="border border-border">{testPanel}</div>
            </ListView>
          ) : (
            <div className="mx-auto grid w-full max-w-2xl gap-4 p-4">
              <p className="text-sm text-fg-muted">{t.view.phoneBranches}</p>
              <div className="border border-border">{testPanel}</div>
            </div>
          )}
        </div>
      )}
      {tab === 'flow' && shown === 'canvas' && (
        <div className="flex min-h-0 flex-1 max-lg:flex-col max-lg:overflow-y-auto">
          <aside className="w-56 shrink-0 overflow-y-auto border-r border-border max-lg:max-h-72 max-lg:w-full max-lg:border-r-0 max-lg:border-b">
            <Palette
              catalogue={catalogue}
              trigger={flow.trigger.type}
              role={role}
              onAdd={(type) => {
                const placed = placeAfter(flow, catalogue, selected ?? TRIGGER_ID, type)
                update(() => placed.flow)
                setSelected(placed.id)
              }}
            />
          </aside>
          <div className="min-h-96 min-w-0 flex-1">
            <ReactFlowProvider>
              <Canvas
                flow={flow}
                catalogue={catalogue}
                selected={selected}
                issues={byNode}
                path={path}
                onSelect={(id) => {
                  setSelected(id)
                  if (id) setAside('step')
                }}
                onMove={(id, position) => update((f) => moveNode(f, id, position))}
                onRemove={(id) => {
                  update((f) => removeNode(f, id))
                  if (selected === id) setSelected(null)
                }}
                onConnect={(source, port, target) =>
                  update((f) => connect(f, source, port, target))
                }
                onDisconnect={(source, port) => update((f) => disconnect(f, source, port))}
                onDrop={(typeId, position) => {
                  const type = nodeTypeOf(catalogue, typeId)
                  if (!type || type.role === 'trigger') return
                  const added = addNode(flow, type, position)
                  update(() => added.flow)
                  setSelected(added.id)
                }}
              />
            </ReactFlowProvider>
          </div>
          <aside className="w-80 shrink-0 overflow-y-auto border-l border-border max-lg:w-full max-lg:border-l-0 max-lg:border-t">
            <div className="border-b border-border p-2">
              <ToggleGroup
                type="single"
                value={aside}
                aria-label={t.view.aside}
                onValueChange={(next) => {
                  if (next === 'step' || next === 'test') setAside(next)
                }}
              >
                <ToggleGroupItem value="step">{t.view.step}</ToggleGroupItem>
                <ToggleGroupItem value="test">{t.view.test}</ToggleGroupItem>
              </ToggleGroup>
            </div>
            {aside === 'test' && testPanel}
            {aside === 'step' && hasTemplates && (
              <div className="border-b border-border p-3">
                <MessagePicker archiveId={record.archive} value={message} onChange={setMessage} />
              </div>
            )}
            {aside === 'step' && (
              <Inspector
                flow={flow}
                catalogue={catalogue}
                selected={selected}
                issues={selected ? (byNode.get(selected) ?? []) : []}
                context={context}
                onParams={(id, params) => update((f) => setParams(f, id, params))}
                onTrigger={(type) => update((f) => setTrigger(f, type))}
                onConfirm={(id, confirm) => update((f) => setConfirm(f, id, confirm))}
                onRemove={(id) => {
                  update((f) => removeNode(f, id))
                  setSelected(null)
                }}
              />
            )}
          </aside>
        </div>
      )}
    </div>
  )
}

export function FlowEditorPage() {
  const { flowId } = useParams({ from: '/rules/$flowId' })
  const record = useQuery({ ...flowQuery(flowId), refetchOnWindowFocus: false })
  const [generation, setGeneration] = useState(0)
  const catalogueData = useQuery(rulesCatalogueQuery)
  const catalogue = useMemo(() => readCatalogue(catalogueData.data), [catalogueData.data])
  if (record.isPending || catalogueData.isPending) {
    return (
      <div className="grid h-full gap-4 bg-bg p-6">
        <Skeleton className="h-full" />
      </div>
    )
  }
  if (record.isError || catalogueData.isError) {
    const error = record.error ?? catalogueData.error
    return (
      <main className="grid h-full content-start gap-4 bg-bg p-6 text-fg">
        <p role="alert" className="text-sm text-danger">
          {error ? authErrorMessage(error) : t.editor.loadFailed}
        </p>
        <Button variant="secondary" className="justify-self-start" asChild>
          <Link to="/settings/rules">{t.editor.back}</Link>
        </Button>
      </main>
    )
  }
  return (
    <Editor
      key={`${record.data.id}:${generation}`}
      record={record.data}
      catalogue={catalogue}
      onReload={async () => {
        await record.refetch()
        setGeneration((g) => g + 1)
      }}
    />
  )
}
