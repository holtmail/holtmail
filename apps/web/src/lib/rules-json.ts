import { type FlowDefinition, readFlow } from './rules.ts'

/** The flow as the file Export downloads: the `holtmail-rules` JSON, the same text
 * `holtmail rules export` prints and `holtmail rules import` reads. */
export function exportText(flow: FlowDefinition): string {
  return `${JSON.stringify(flow, null, 2)}\n`
}

/** A file name for a flow's export: its name in lowercase words joined by `-`, `flow` when
 * nothing is left. */
export function exportFileName(name: string): string {
  const base = name
    .normalize('NFKD')
    .toLowerCase()
    .replace(/[^\p{L}\p{N}]+/gu, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
  return `${base || 'flow'}.json`
}

/** Reads an imported file: the flow, or why it is not one (`json`: not JSON; `flow`: JSON
 * without a trigger). The server validates the rest on save. */
export function importText(
  text: string,
): { flow: FlowDefinition; error?: never } | { flow?: never; error: 'json' | 'flow' } {
  let value: unknown
  try {
    value = JSON.parse(text)
  } catch {
    return { error: 'json' }
  }
  const flow = readFlow(value)
  return flow ? { flow } : { error: 'flow' }
}

export type DiffLine =
  | { kind: 'same' | 'added' | 'removed'; text: string }
  | { kind: 'gap'; count: number }

function sortKeys(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortKeys)
  if (typeof value === 'object' && value !== null) {
    const record = Object.fromEntries(Object.entries(value))
    return Object.fromEntries(
      Object.keys(record)
        .filter((k) => k !== 'position')
        .toSorted()
        .map((k) => [k, sortKeys(record[k])]),
    )
  }
  return value
}

/** The text a version diff compares: keys sorted, positions left out (moving a step on the
 * canvas is not a change the engine sees). */
export function diffText(flow: FlowDefinition | null): string[] {
  return flow ? JSON.stringify(sortKeys(flow), null, 2).split('\n') : []
}

const LIMIT = 4_000_000

/** A line diff from `before` to `after` (longest common subsequence), with runs of unchanged
 * lines beyond `context` lines from a change folded into gaps. Inputs whose product of lengths
 * passes four million lines show as all removed then all added. */
export function diffLines(
  before: readonly string[],
  after: readonly string[],
  context = 3,
): DiffLine[] {
  const n = before.length
  const m = after.length
  const raw: { kind: 'same' | 'added' | 'removed'; text: string }[] = []
  if (n * m > LIMIT) {
    for (const text of before) raw.push({ kind: 'removed', text })
    for (const text of after) raw.push({ kind: 'added', text })
  } else {
    const width = m + 1
    const table = new Uint32Array((n + 1) * width)
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        table[i * width + j] =
          before[i] === after[j]
            ? (table[(i + 1) * width + j + 1] ?? 0) + 1
            : Math.max(table[(i + 1) * width + j] ?? 0, table[i * width + j + 1] ?? 0)
      }
    }
    let i = 0
    let j = 0
    while (i < n || j < m) {
      const b = before[i]
      const a = after[j]
      if (i < n && j < m && b === a && b !== undefined) {
        raw.push({ kind: 'same', text: b })
        i++
        j++
      } else if (
        i < n &&
        b !== undefined &&
        (j >= m || (table[(i + 1) * width + j] ?? 0) >= (table[i * width + j + 1] ?? 0))
      ) {
        raw.push({ kind: 'removed', text: b })
        i++
      } else if (a !== undefined) {
        raw.push({ kind: 'added', text: a })
        j++
      } else {
        break
      }
    }
  }
  const near = raw.map(() => false)
  raw.forEach((line, index) => {
    if (line.kind === 'same') return
    for (let k = Math.max(0, index - context); k <= index + context && k < raw.length; k++) {
      near[k] = true
    }
  })
  const out: DiffLine[] = []
  let gap = 0
  raw.forEach((line, index) => {
    if (near[index]) {
      if (gap > 0) out.push({ kind: 'gap', count: gap })
      gap = 0
      out.push(line)
    } else {
      gap++
    }
  })
  if (gap > 0) out.push({ kind: 'gap', count: gap })
  return out
}
