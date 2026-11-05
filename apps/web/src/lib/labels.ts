import type { Label } from '@holtmail/api-client'

const systemOrder: readonly string[] = [
  'Inbox',
  'Starred',
  'Important',
  'Sent',
  'Drafts',
  'Spam',
  'Trash',
]

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true })

export function isSystemLabel(name: string): boolean {
  return systemOrder.includes(name)
}

export interface LabelNode {
  label: Label
  name: string
  depth: number
}

function parentPath(path: string): string | undefined {
  const cut = path.lastIndexOf('/')
  return cut > 0 ? path.slice(0, cut) : undefined
}

function compareNames(a: string, b: string): number {
  const systemA = systemOrder.indexOf(a)
  const systemB = systemOrder.indexOf(b)
  if (systemA >= 0 && systemB >= 0) return systemA - systemB
  if (systemA >= 0) return -1
  if (systemB >= 0) return 1
  return collator.compare(a, b)
}

export function orderLabels(labels: readonly Label[]): LabelNode[] {
  const byName = new Map(labels.map((label) => [label.name, label]))
  const children = new Map<string | undefined, Label[]>()
  const parents = new Map<string, string | undefined>()
  for (const label of labels) {
    let parent: string | undefined
    if (!isSystemLabel(label.name)) {
      parent = parentPath(label.name)
      while (parent !== undefined && !byName.has(parent)) parent = parentPath(parent)
    }
    parents.set(label.name, parent)
    const list = children.get(parent) ?? []
    list.push(label)
    children.set(parent, list)
  }
  const result: LabelNode[] = []
  const visit = (parent: string | undefined, depth: number) => {
    for (const label of (children.get(parent) ?? []).toSorted((a, b) =>
      compareNames(a.name, b.name),
    )) {
      const prefix = parents.get(label.name)
      result.push({
        label,
        name: prefix ? label.name.slice(prefix.length + 1) : label.name,
        depth,
      })
      visit(label.name, depth + 1)
    }
  }
  visit(undefined, 0)
  return result
}

export interface Chips {
  shown: string[]
  more: number
}

export function listChips(labels: readonly string[], current?: string, max = 2): Chips {
  const visible = labels.filter((name) => name !== current && !isSystemLabel(name))
  return { shown: visible.slice(0, max), more: Math.max(0, visible.length - max) }
}

export function readerLabels(labels: readonly string[]): string[] {
  return labels.toSorted(compareNames)
}
