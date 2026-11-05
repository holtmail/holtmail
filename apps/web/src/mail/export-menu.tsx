import type { ExportFormat } from '@holtmail/api-client'
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from '@holtmail/ui'
import { DownloadIcon, FileArchiveIcon, FileTextIcon } from 'lucide-react'

import { exportFormats, exportHref } from '../lib/export.ts'
import { strings } from '../strings.ts'
import type { ListSource } from './message-list.tsx'

const t = strings.exportMail

function FormatItems({ archiveId, source }: { archiveId: string; source: ListSource }) {
  return exportFormats.map((format: ExportFormat) => {
    const href = exportHref(archiveId, source, format)
    if (!href) return null
    const Icon = format === 'zip' ? FileArchiveIcon : FileTextIcon
    return (
      <DropdownMenuItem key={format} asChild>
        <a href={href} download>
          <Icon />
          <span className="grid">
            <span>{t.format[format]}</span>
            <span className="text-xs text-fg-subtle">{t.formatHint[format]}</span>
          </span>
        </a>
      </DropdownMenuItem>
    )
  })
}

export function ExportMenu({ archiveId, source }: { archiveId: string; source: ListSource }) {
  if (!exportHref(archiveId, source, 'mbox')) return null
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t.exportList} title={t.exportList}>
          <DownloadIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        <DropdownMenuLabel className="text-xs font-normal text-fg-subtle">
          {t.what[source.kind]}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <FormatItems archiveId={archiveId} source={source} />
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

export function ExportSubmenu({ archiveId }: { archiveId: string }) {
  return (
    <DropdownMenuSub>
      <DropdownMenuSubTrigger>
        <DownloadIcon />
        {t.exportArchive}
      </DropdownMenuSubTrigger>
      <DropdownMenuSubContent className="w-64">
        <FormatItems archiveId={archiveId} source={{ kind: 'all' }} />
      </DropdownMenuSubContent>
    </DropdownMenuSub>
  )
}
