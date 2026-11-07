import type { DataFile, DataUpdates } from '@holtmail/api-client'

import { strings } from '../strings.ts'
import { formatCount, formatFullDate } from './format.ts'

const t = strings.about

export interface DataFileRow {
  key: string
  name: string
  file: string
  version: string
  rows: string
  license: string
  origin: string
}

/** A table's display name; an unknown table keeps its own name. */
export function dataName(name: string): string {
  return t.names[name] ?? name
}

/** One display row per loaded file, in the server's order. */
export function dataFileRows(files: readonly DataFile[], locale?: string): DataFileRow[] {
  return files.map((file) => ({
    key: `${file.name}:${file.file}`,
    name: dataName(file.name),
    file: file.file,
    version: file.version,
    rows: t.rowCount(formatCount(file.rows, locale)),
    license: file.license,
    origin: t.origins[file.origin] ?? file.origin,
  }))
}

/** Whether a UT1 table is among the loaded files. */
export function ut1Loaded(files: readonly DataFile[]): boolean {
  return files.some((file) => file.name === 'ut1')
}

/** Attribution lines to show, each once: the server's text, and the UT1 line whenever UT1 is loaded. */
export function attributions(files: readonly DataFile[]): string[] {
  const lines = files.flatMap((file) => {
    const text = file.attribution?.trim()
    if (text) return [text]
    return file.name === 'ut1' ? [t.ut1Attribution] : []
  })
  return [...new Set(lines)]
}

/** The update state in at most two sentences: on or off, then the last check when there was one. */
export function updateLines(updates: DataUpdates, locale?: string): string[] {
  if (!updates.enabled) return [t.updatesOff]
  const sources = updates.data_sources.map(dataName).join(', ')
  const head = t.updatesOn(sources)
  if (updates.last_checked_at == null) return [head, t.neverChecked]
  const outcome = updates.last_outcome ? t.outcomes[updates.last_outcome] : undefined
  return [head, t.lastCheck(formatFullDate(updates.last_checked_at, locale), outcome)]
}
