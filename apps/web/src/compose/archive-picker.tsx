import {
  type DraftAttachment,
  type MessageSummary,
  messageQuery,
  searchInfiniteQuery,
} from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Input,
} from '@holtmail/ui'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { ChevronLeftIcon, PaperclipIcon } from 'lucide-react'
import { useDeferredValue, useState } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { formatSize } from '../lib/format.ts'
import { archivedAttachment } from '../lib/writing.ts'
import { strings } from '../strings.ts'

const t = strings.writing.compose.picker

/** Finds a message of the archive with attachments and adds one of them to the draft; the
 * server resolves it to the stored bytes, so it works for mail long gone from the server. */
export function ArchivePicker({
  archiveId,
  open,
  onOpenChange,
  onPick,
}: {
  archiveId: string
  open: boolean
  onOpenChange: (open: boolean) => void
  onPick: (attachment: DraftAttachment) => void
}) {
  const [text, setText] = useState('')
  const [message, setMessage] = useState<MessageSummary>()
  const query = useDeferredValue(text.trim())

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) setMessage(undefined)
        onOpenChange(next)
      }}
    >
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>{t.title}</DialogTitle>
          <DialogDescription>{t.lead}</DialogDescription>
        </DialogHeader>
        {message ? (
          <Attachments
            message={message}
            onBack={() => setMessage(undefined)}
            onPick={(attachment) => {
              onPick(attachment)
              setMessage(undefined)
              onOpenChange(false)
            }}
          />
        ) : (
          <div className="grid gap-3">
            <Input
              aria-label={t.search}
              placeholder={t.search}
              value={text}
              autoFocus
              onChange={(event) => setText(event.target.value)}
            />
            {open && <Results archiveId={archiveId} query={query} onChoose={setMessage} />}
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}

function Results({
  archiveId,
  query,
  onChoose,
}: {
  archiveId: string
  query: string
  onChoose: (message: MessageSummary) => void
}) {
  const search = useInfiniteQuery(
    searchInfiniteQuery(archiveId, query ? `has:attachment ${query}` : 'has:attachment'),
  )
  if (search.isPending) return <p className="text-sm text-fg-muted">{t.searching}</p>
  if (search.isError) {
    return (
      <p role="alert" className="text-sm text-danger">
        {t.failed(authErrorMessage(search.error))}
      </p>
    )
  }
  const items = search.data.pages.flatMap((page) => page.items).slice(0, 50)
  if (items.length === 0) return <p className="text-sm text-fg-muted">{t.none}</p>
  return (
    <ul className="max-h-80 divide-y divide-border overflow-y-auto border border-border">
      {items.map((item) => (
        <li key={item.id}>
          <button
            type="button"
            className="grid w-full gap-0.5 px-3 py-2 text-left hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus"
            onClick={() => onChoose(item)}
          >
            <span className="truncate text-sm text-fg">{item.subject || '—'}</span>
            <span className="truncate text-xs text-fg-subtle">
              {item.from.map((a) => a.name || a.email).join(', ')} · {item.attachment_count}
            </span>
          </button>
        </li>
      ))}
    </ul>
  )
}

function Attachments({
  message,
  onBack,
  onPick,
}: {
  message: MessageSummary
  onBack: () => void
  onPick: (attachment: DraftAttachment) => void
}) {
  const detail = useQuery(messageQuery(message.id))
  const files = detail.data?.attachments.filter((a) => !a.inline) ?? []
  return (
    <div className="grid gap-3">
      <Button variant="ghost" size="sm" className="justify-self-start" onClick={onBack}>
        <ChevronLeftIcon />
        {t.back}
      </Button>
      <p className="truncate text-sm font-semibold">{message.subject || '—'}</p>
      {detail.isPending ? (
        <p className="text-sm text-fg-muted">{t.searching}</p>
      ) : detail.isError ? (
        <p role="alert" className="text-sm text-danger">
          {t.failed(authErrorMessage(detail.error))}
        </p>
      ) : files.length === 0 ? (
        <p className="text-sm text-fg-muted">{t.noAttachments}</p>
      ) : (
        <ul className="grid gap-1">
          {files.map((file) => (
            <li key={file.ordinal}>
              <Button
                variant="secondary"
                className="w-full justify-start"
                aria-label={t.add(file.filename ?? file.content_type)}
                onClick={() => onPick(archivedAttachment(message.id, file))}
              >
                <PaperclipIcon />
                <span className="truncate">{file.filename ?? file.content_type}</span>
                <span className="ml-auto font-mono text-xs text-fg-subtle">
                  {formatSize(file.size)}
                </span>
              </Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
