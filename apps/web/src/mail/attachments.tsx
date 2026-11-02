import type { Attachment } from '@holtmail/api-client'
import { cn } from '@holtmail/ui'
import {
  CalendarIcon,
  FileArchiveIcon,
  FileAudioIcon,
  FileCodeIcon,
  FileIcon,
  FileImageIcon,
  FileSpreadsheetIcon,
  FileTextIcon,
  FileVideoIcon,
  type LucideIcon,
} from 'lucide-react'

import { type AttachmentKind, attachmentKind } from '../lib/attachments.ts'
import { formatSize } from '../lib/format.ts'
import { strings } from '../strings.ts'

const icons: Record<AttachmentKind, LucideIcon> = {
  image: FileImageIcon,
  pdf: FileTextIcon,
  text: FileTextIcon,
  archive: FileArchiveIcon,
  spreadsheet: FileSpreadsheetIcon,
  audio: FileAudioIcon,
  video: FileVideoIcon,
  code: FileCodeIcon,
  calendar: CalendarIcon,
  other: FileIcon,
}

export function Attachments({ attachments }: { attachments: Attachment[] }) {
  if (attachments.length === 0) return null
  return (
    <section aria-label={strings.reader.attachments} className="grid gap-2">
      <h3 className="text-xs text-fg-subtle">{strings.reader.attachments}</h3>
      <ul className="grid grid-cols-[repeat(auto-fill,minmax(--spacing(32),1fr))] gap-2">
        {attachments.map((attachment) => (
          <li key={attachment.ordinal}>
            <AttachmentTile attachment={attachment} />
          </li>
        ))}
      </ul>
    </section>
  )
}

function AttachmentTile({ attachment }: { attachment: Attachment }) {
  const kind = attachmentKind(attachment.content_type, attachment.filename)
  const Icon = icons[kind]
  const name = attachment.filename || attachment.content_type
  const viewable = kind === 'image' || kind === 'pdf'
  return (
    <a
      href={attachment.url}
      target={viewable ? '_blank' : undefined}
      rel="noopener noreferrer"
      download={viewable ? undefined : (attachment.filename ?? '')}
      title={name}
      className={cn(
        'group flex aspect-square flex-col border border-border bg-bg',
        'transition-colors duration-fast-02 ease-productive hover:border-border-strong hover:bg-bg-hover',
        'outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
      )}
    >
      <span className="flex min-h-0 flex-1 items-center justify-center overflow-hidden bg-bg-subtle text-fg-subtle">
        {kind === 'image' ? (
          <img
            src={attachment.url}
            alt=""
            loading="lazy"
            decoding="async"
            className="size-full object-cover"
          />
        ) : (
          <Icon aria-hidden className="size-8 stroke-1" />
        )}
      </span>
      <span className="grid gap-0.5 border-t border-border px-2 py-1.5">
        <span className="truncate text-xs text-fg">{name}</span>
        <span className="font-mono text-xs text-fg-subtle tabular-nums">
          {formatSize(attachment.size)}
        </span>
      </span>
    </a>
  )
}
