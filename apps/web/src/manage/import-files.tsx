import { uploadImport } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { Button, cn } from '@holtmail/ui'
import { useQueryClient } from '@tanstack/react-query'
import { CheckIcon, CircleAlertIcon, FileArchiveIcon, UploadIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { type DragEvent, useEffect, useRef, useState } from 'react'

import { fileKind, importExtensions } from '../lib/accounts.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { formatSize } from '../lib/format.ts'
import { strings } from '../strings.ts'

const t = strings.importFiles

type Phase = 'queued' | 'uploading' | 'sent' | 'failed'

interface Upload {
  key: string
  file: File
  phase: Phase
  loaded: number
  error?: string
}

export function ImportFiles({
  archiveId,
  onUploaded,
}: {
  archiveId: string
  onUploaded?: () => void
}) {
  const queryClient = useQueryClient()
  const input = useRef<HTMLInputElement>(null)
  const [uploads, setUploads] = useState<Upload[]>([])
  const [dragging, setDragging] = useState(false)
  const busy = useRef(false)

  const update = (key: string, change: Partial<Upload>) =>
    setUploads((list) => list.map((item) => (item.key === key ? { ...item, ...change } : item)))

  useEffect(() => {
    if (busy.current) return
    const next = uploads.find((item) => item.phase === 'queued')
    if (!next) return
    busy.current = true
    update(next.key, { phase: 'uploading' })
    const send = async () => {
      try {
        await uploadImport(archiveId, next.file, {
          onProgress: (loaded) => update(next.key, { loaded }),
        })
        update(next.key, { phase: 'sent', loaded: next.file.size })
        await queryClient.invalidateQueries({ queryKey: ['archives'] })
        onUploaded?.()
      } catch (error) {
        update(next.key, { phase: 'failed', error: authErrorMessage(error) })
      } finally {
        busy.current = false
        setUploads((list) => [...list])
      }
    }
    void send()
  }, [uploads, archiveId, queryClient, onUploaded])

  const add = (files: FileList | null) => {
    if (!files?.length) return
    const added = Array.from(files, (file, index) => ({
      key: `${Date.now()}-${index}-${file.name}`,
      file,
      phase: 'queued' as const,
      loaded: 0,
    }))
    setUploads((list) => [...list, ...added])
  }

  const drop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault()
    setDragging(false)
    add(event.dataTransfer.files)
  }

  return (
    <div className="grid gap-4">
      <div
        onDragOver={(event) => {
          event.preventDefault()
          setDragging(true)
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={drop}
        className={cn(
          'relative grid place-items-center gap-3 border border-dashed border-border-strong px-6 py-10 text-center',
          'transition-colors duration-moderate-01 ease-productive',
          dragging && 'border-accent bg-bg-hover',
        )}
      >
        <m.span
          aria-hidden
          className="text-fg-subtle"
          animate={{ y: dragging ? -6 : 0, transition: transition.expressive.moderate02 }}
        >
          <UploadIcon className="size-8 stroke-1" />
        </m.span>
        <p className="text-sm text-fg">{t.drop}</p>
        <div className="flex items-center gap-3 text-xs text-fg-subtle">
          <span className="h-px w-8 bg-border" />
          {t.or}
          <span className="h-px w-8 bg-border" />
        </div>
        <Button type="button" variant="secondary" onClick={() => input.current?.click()}>
          {t.choose}
        </Button>
        <p className="font-mono text-xs text-fg-subtle">{t.formats}</p>
        <input
          ref={input}
          type="file"
          multiple
          accept={[
            ...importExtensions,
            'application/mbox',
            'message/rfc822',
            'application/zip',
            'text/vcard',
            'text/calendar',
          ].join(',')}
          className="sr-only"
          tabIndex={-1}
          onChange={(event) => {
            add(event.target.files)
            event.target.value = ''
          }}
        />
      </div>

      <ul className="grid gap-px" aria-live="polite">
        <AnimatePresence initial={false}>
          {uploads.map((item) => (
            <m.li
              key={item.key}
              layout
              initial={{ opacity: 0, y: 8 }}
              animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
              exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
            >
              <UploadRow upload={item} />
            </m.li>
          ))}
        </AnimatePresence>
      </ul>
    </div>
  )
}

function UploadRow({ upload }: { upload: Upload }) {
  const { file, phase, loaded, error } = upload
  const share = file.size > 0 ? Math.min(1, loaded / file.size) : phase === 'sent' ? 1 : 0
  const status =
    phase === 'failed'
      ? (error ?? t.failed)
      : phase === 'sent'
        ? t.sent
        : phase === 'uploading'
          ? `${t.uploading} ${Math.round(share * 100)}%`
          : t.queued
  return (
    <div className="relative grid grid-cols-[auto_1fr_auto] items-center gap-3 overflow-hidden bg-bg-subtle px-4 py-3">
      <span aria-hidden className="text-fg-subtle [&_svg]:size-4">
        {phase === 'sent' ? (
          <CheckIcon className="text-success" />
        ) : phase === 'failed' ? (
          <CircleAlertIcon className="text-danger" />
        ) : (
          <FileArchiveIcon />
        )}
      </span>
      <span className="grid min-w-0">
        <span className="truncate text-sm text-fg">{file.name}</span>
        <span
          className={cn('truncate text-xs', phase === 'failed' ? 'text-danger' : 'text-fg-subtle')}
        >
          {status}
          {fileKind(file.name) === 'unknown' && phase === 'queued' ? ` · ${t.unknown}` : ''}
        </span>
      </span>
      <span className="font-mono text-xs text-fg-subtle tabular-nums">{formatSize(file.size)}</span>
      <m.span
        aria-hidden
        className={cn(
          'absolute bottom-0 left-0 h-0.5 w-full origin-left',
          phase === 'failed' ? 'bg-danger' : phase === 'sent' ? 'bg-success' : 'bg-accent',
        )}
        initial={{ scaleX: 0 }}
        animate={{ scaleX: share, transition: transition.productive.moderate01 }}
      />
    </div>
  )
}
