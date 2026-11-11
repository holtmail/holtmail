import '@xyflow/react/dist/base.css'
import { Button, cn } from '@holtmail/ui'
import {
  type Edge,
  type EdgeChange,
  Handle,
  type Node,
  type NodeChange,
  type NodeProps,
  Panel,
  Position,
  ReactFlow,
  useReactFlow,
} from '@xyflow/react'
import { MaximizeIcon, MinusIcon, PlusIcon } from 'lucide-react'
import { type DragEvent, useMemo, useState } from 'react'

import {
  docsOf,
  type FlowDefinition,
  GRID,
  type NodeRole,
  type NodeType,
  nodeTypeOf,
  paramsOf,
  paramSummary,
  type Point,
  toGraph,
  TRIGGER_ID,
} from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'

const t = strings.rules
const locale = currentLocale()

/** The drag type a palette entry carries: the node type id. */
export const STEP_DRAG = 'application/x-holtmail-step'

interface StepData extends Record<string, unknown> {
  title: string
  summary: string
  role: NodeRole
  ports: string[]
  issues: number
  confirm: boolean
  known: boolean
  onPath: boolean
}

type StepNode = Node<StepData, 'step'>

const roleLabel: Record<NodeRole, string> = {
  trigger: t.inspector.trigger,
  condition: t.palette.conditions,
  action: t.palette.actions,
}

function StepBox({ data, selected }: NodeProps<StepNode>) {
  const trigger = data.role === 'trigger'
  return (
    <div
      className={cn(
        'hm-step grid w-56 gap-1 border bg-bg px-3 py-2 text-left',
        selected || data.onPath ? 'border-accent' : 'border-border-strong',
        data.onPath && 'hm-on-path',
        data.issues > 0 && 'border-danger',
        !data.known && 'border-dashed',
      )}
    >
      {!trigger && <Handle type="target" position={Position.Left} />}
      <span className="flex items-baseline justify-between gap-2 font-mono text-[0.625rem] tracking-wide text-fg-subtle uppercase">
        <span>{roleLabel[data.role]}</span>
        {data.confirm && <span>{t.inspector.confirm}</span>}
      </span>
      <span className="truncate text-sm text-fg">{data.title}</span>
      {data.onPath && <span className="sr-only">{t.test.onPath}</span>}
      {data.summary && (
        <span className="truncate font-mono text-xs text-fg-muted">{data.summary}</span>
      )}
      {data.issues > 0 && (
        <span className="text-xs text-danger">{t.editor.issues(data.issues)}</span>
      )}
      {data.ports.length > 0 && (
        <span className="grid justify-items-end gap-1 pt-1">
          {data.ports.map((port) => (
            <span key={port} className="relative pr-1 text-[0.625rem] text-fg-subtle">
              {t.inspector.portLabels[port] ?? port}
              <Handle type="source" id={port} position={Position.Right} className="hm-port" />
            </span>
          ))}
        </span>
      )}
    </div>
  )
}

const nodeTypes = { step: StepBox }

export interface CanvasProps {
  flow: FlowDefinition
  catalogue: NodeType[]
  selected: string | null
  issues: Map<string, unknown[]>
  onSelect: (id: string | null) => void
  onMove: (id: string, position: Point) => void
  onRemove: (id: string) => void
  onConnect: (source: string, port: string, target: string) => void
  onDisconnect: (source: string, port: string) => void
  onDrop: (type: string, position: Point) => void
  /** The nodes and edges of a tested or logged run, drawn in the accent color. */
  path?: { nodes: Set<string>; edges: Set<string> } | null
}

export function Canvas({
  flow,
  catalogue,
  selected,
  issues,
  onSelect,
  onMove,
  onRemove,
  onConnect,
  onDisconnect,
  onDrop,
  path,
}: CanvasProps) {
  const { screenToFlowPosition, zoomIn, zoomOut, fitView } = useReactFlow()
  const [measured, setMeasured] = useState<Record<string, { width: number; height: number }>>({})
  const [selectedEdge, setSelectedEdge] = useState<string | null>(null)
  const graph = useMemo(() => toGraph(flow, catalogue), [flow, catalogue])

  const nodes: StepNode[] = graph.nodes.map((n) => {
    const type = nodeTypeOf(catalogue, n.type)
    const size = measured[n.id]
    const node: StepNode = {
      id: n.id,
      type: 'step',
      position: n.position,
      selected: n.id === selected,
      deletable: n.id !== TRIGGER_ID,
      data: {
        title: docsOf(type, locale, n.type).title,
        summary: paramSummary(paramsOf(flow, n.id)),
        role: n.role,
        ports: n.ports,
        issues: issues.get(n.id)?.length ?? 0,
        confirm: n.confirm,
        known: n.known,
        onPath: path?.nodes.has(n.id) ?? false,
      },
      ariaLabel: `${roleLabel[n.role]}: ${docsOf(type, locale, n.type).title}`,
    }
    if (size) node.measured = size
    return node
  })

  const edges: Edge[] = graph.edges.map((e) => ({
    id: e.id,
    source: e.source,
    sourceHandle: e.port,
    target: e.target,
    type: 'step',
    selected: e.id === selectedEdge,
    className: path?.edges.has(e.id) ? 'hm-path' : undefined,
  }))

  const nodeChanges = (changes: NodeChange<StepNode>[]) => {
    let chosen: string | null | undefined
    for (const change of changes) {
      if (change.type === 'dimensions' && change.dimensions) {
        const { width, height } = change.dimensions
        setMeasured((m) => ({ ...m, [change.id]: { width, height } }))
      } else if (change.type === 'position' && change.position) {
        onMove(change.id, change.position)
      } else if (change.type === 'select') {
        if (change.selected) chosen = change.id
        else if (change.id === selected && chosen === undefined) chosen = null
      } else if (change.type === 'remove') {
        onRemove(change.id)
      }
    }
    if (chosen !== undefined) {
      onSelect(chosen)
      if (chosen) setSelectedEdge(null)
    }
  }

  const edgeChanges = (changes: EdgeChange[]) => {
    for (const change of changes) {
      if (change.type === 'select') {
        setSelectedEdge(change.selected ? change.id : null)
      } else if (change.type === 'remove') {
        const edge = graph.edges.find((e) => e.id === change.id)
        if (edge) onDisconnect(edge.source, edge.port)
      }
    }
  }

  const drop = (event: DragEvent) => {
    const type = event.dataTransfer.getData(STEP_DRAG)
    if (!type) return
    event.preventDefault()
    onDrop(type, screenToFlowPosition({ x: event.clientX, y: event.clientY }))
  }

  return (
    <div className="hm-flow relative h-full min-h-96 w-full">
      <ReactFlow<StepNode>
        nodes={nodes}
        edges={edges}
        nodeTypes={nodeTypes}
        onNodesChange={nodeChanges}
        onEdgesChange={edgeChanges}
        onConnect={(c) => {
          if (c.source && c.target) onConnect(c.source, c.sourceHandle ?? 'next', c.target)
        }}
        onPaneClick={() => {
          onSelect(null)
          setSelectedEdge(null)
        }}
        onDragOver={(event) => {
          if (event.dataTransfer.types.includes(STEP_DRAG)) {
            event.preventDefault()
            event.dataTransfer.dropEffect = 'copy'
          }
        }}
        onDrop={drop}
        deleteKeyCode={['Backspace', 'Delete']}
        snapToGrid
        snapGrid={[GRID, GRID]}
        fitView
        minZoom={0.25}
        maxZoom={2}
        aria-label={t.editor.canvas}
      >
        <Panel position="bottom-left" className="flex border border-border bg-bg">
          <Button
            variant="ghost"
            size="icon"
            aria-label={t.editor.zoomIn}
            onClick={() => void zoomIn()}
          >
            <PlusIcon />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            aria-label={t.editor.zoomOut}
            onClick={() => void zoomOut()}
          >
            <MinusIcon />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            aria-label={t.editor.fit}
            onClick={() => void fitView()}
          >
            <MaximizeIcon />
          </Button>
        </Panel>
        <Panel position="top-left" className="max-w-sm text-xs text-fg-subtle">
          {t.editor.canvasHint}
        </Panel>
      </ReactFlow>
    </div>
  )
}
