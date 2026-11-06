import {
  type CalendarExportFormat,
  type ContactsExportFormat,
  type ContactsExportScope,
  type ExportFormat,
  type ExportScope,
  exportUrl,
} from '@holtmail/api-client'

type ExportSource =
  | { kind: 'all' }
  | { kind: 'folder'; folderId: string }
  | { kind: 'label'; label: string }
  | { kind: 'search'; q: string }

export const exportFormats: readonly ExportFormat[] = ['mbox', 'zip', 'pdf']

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

export const contactsExportFormats: readonly ContactsExportFormat[] = ['vcf', 'zip']
export const calendarExportFormats: readonly CalendarExportFormat[] = ['ics', 'zip']

/** The contacts export that matches the people list's source filter: an address book when one
 * is chosen, else every contact. */
export function contactsScope(source: string | undefined): ContactsExportScope {
  if (!source || source === 'contacts' || source === 'mail') return {}
  return { collection: source }
}
