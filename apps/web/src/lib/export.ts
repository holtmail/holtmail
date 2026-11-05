import { type ExportFormat, type ExportScope, exportUrl } from '@holtmail/api-client'

type ExportSource =
  | { kind: 'all' }
  | { kind: 'folder'; folderId: string }
  | { kind: 'label'; label: string }
  | { kind: 'search'; q: string }

export const exportFormats: readonly ExportFormat[] = ['mbox', 'zip']

export function exportScope(source: ExportSource): ExportScope | null {
  if (source.kind === 'folder') return { folder: source.folderId }
  if (source.kind === 'label') return { label: source.label }
  if (source.kind === 'search') return source.q.trim() ? { q: source.q } : null
  return {}
}

export function exportHref(
  archiveId: string,
  source: ExportSource,
  format: ExportFormat,
): string | null {
  const scope = exportScope(source)
  return scope ? exportUrl(archiveId, format, scope) : null
}
