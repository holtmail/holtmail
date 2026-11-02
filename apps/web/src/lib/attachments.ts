export type AttachmentKind =
  | 'image'
  | 'pdf'
  | 'text'
  | 'archive'
  | 'spreadsheet'
  | 'audio'
  | 'video'
  | 'code'
  | 'calendar'
  | 'other'

const byExtension: Record<string, AttachmentKind> = {
  pdf: 'pdf',
  txt: 'text',
  md: 'text',
  doc: 'text',
  docx: 'text',
  odt: 'text',
  rtf: 'text',
  zip: 'archive',
  gz: 'archive',
  tgz: 'archive',
  rar: 'archive',
  '7z': 'archive',
  tar: 'archive',
  xls: 'spreadsheet',
  xlsx: 'spreadsheet',
  ods: 'spreadsheet',
  csv: 'spreadsheet',
  ics: 'calendar',
  json: 'code',
  xml: 'code',
  html: 'code',
  js: 'code',
  ts: 'code',
}

export function attachmentKind(contentType: string, filename?: string | null): AttachmentKind {
  const type = contentType.toLowerCase().split(';')[0]?.trim() ?? ''
  if (type.startsWith('image/')) return 'image'
  if (type === 'application/pdf') return 'pdf'
  if (type.startsWith('audio/')) return 'audio'
  if (type.startsWith('video/')) return 'video'
  if (type === 'text/calendar') return 'calendar'
  const extension = filename?.toLowerCase().split('.').pop()
  if (extension && filename?.includes('.')) {
    const kind = byExtension[extension]
    if (kind) return kind
  }
  if (/zip|compressed|tar|gzip/.test(type)) return 'archive'
  if (/spreadsheet|excel|csv/.test(type)) return 'spreadsheet'
  if (/json|xml|javascript|html/.test(type)) return 'code'
  if (type.startsWith('text/') || /word|document/.test(type)) return 'text'
  return 'other'
}
