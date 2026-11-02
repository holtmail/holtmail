import {
  type MessageDetail,
  messageQuery,
  rawQuery,
  rawUrl,
  threadQuery,
} from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import {
  Button,
  type ButtonProps,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Kbd,
  Skeleton,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import {
  ArrowLeftIcon,
  CheckIcon,
  CircleAlertIcon,
  CodeIcon,
  DownloadIcon,
  LinkIcon,
  MailIcon,
  ReplyIcon,
} from 'lucide-react'
import { m } from 'motion/react'
import { type ReactNode, useEffect, useRef, useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { replyMailto, senderLabel } from '../lib/address.ts'
import { strings } from '../strings.ts'
import { ids } from './context.ts'
import { EmptyState } from './empty-state.tsx'
import { ThreadItem } from './thread-item.tsx'

export function ReaderEmpty() {
  return (
    <EmptyState icon={<MailIcon />} title={strings.reader.emptyTitle}>
      {strings.reader.emptyHint} <Kbd>J</Kbd> <Kbd>K</Kbd> {strings.reader.toOpen}
    </EmptyState>
  )
}

export function Reader({ messageId, onBack }: { messageId: string; onBack?: () => void }) {
  const message = useQuery(messageQuery(messageId))
  const threadId = message.data?.thread_id
  const thread = useQuery({ ...threadQuery(threadId ?? ''), enabled: Boolean(threadId) })

  const messages: MessageDetail[] = thread.data?.messages ?? (message.data ? [message.data] : [])

  return (
    <div className="flex h-full min-h-0 flex-col">
      <Toolbar message={message.data} onBack={onBack} />
      <div id={ids.reader} tabIndex={-1} className="min-h-0 flex-1 overflow-y-auto outline-none">
        {message.isError ? (
          <EmptyState icon={<CircleAlertIcon />} title={strings.reader.loadFailed}>
            {message.error.message}
          </EmptyState>
        ) : message.data ? (
          <m.div
            key={messageId}
            initial={{ opacity: 0, y: distance.sm }}
            animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
          >
            <header className="px-6 pt-6 pb-4">
              <h2 className="text-2xl font-normal wrap-break-word text-fg">
                {message.data.subject || strings.list.noSubject}
              </h2>
              {messages.length > 1 && (
                <p className="mt-1 text-xs text-fg-subtle">
                  {strings.reader.messagesInThread(messages.length)}
                </p>
              )}
            </header>
            <Thread key={messageId} messages={messages} selectedId={messageId} />
          </m.div>
        ) : (
          <ReaderSkeleton />
        )}
      </div>
    </div>
  )
}

function Thread({ messages, selectedId }: { messages: MessageDetail[]; selectedId: string }) {
  const now = useNow()
  const latest = messages.at(-1)?.id
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(
    () => new Set([selectedId, ...(latest ? [latest] : [])]),
  )
  const [seenLatest, setSeenLatest] = useState(latest)
  if (latest !== seenLatest) {
    setSeenLatest(latest)
    if (latest) setExpanded((previous) => new Set([...previous, latest]))
  }
  const selectedRef = useRef<HTMLDivElement>(null)
  const scrolled = useRef(false)

  useEffect(() => {
    if (scrolled.current || messages.length < 2 || selectedId === latest) return
    scrolled.current = true
    selectedRef.current?.scrollIntoView({ block: 'start' })
  }, [messages.length, selectedId, latest])

  const toggle = (id: string) =>
    setExpanded((previous) => {
      const next = new Set(previous)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  return (
    <div className="border-t border-border">
      {messages.map((message) => (
        <div key={message.id} ref={message.id === selectedId ? selectedRef : undefined}>
          <ThreadItem
            message={message}
            expanded={expanded.has(message.id) || messages.length === 1}
            collapsible={messages.length > 1}
            now={now}
            onToggle={() => toggle(message.id)}
          />
        </div>
      ))}
    </div>
  )
}

function ReaderSkeleton() {
  return (
    <div aria-busy className="grid gap-6 px-6 pt-6">
      <Skeleton className="h-8 w-2/3" />
      <div className="grid gap-2">
        <Skeleton className="h-4 w-1/3" />
        <Skeleton className="h-4 w-1/2" />
      </div>
      <div className="grid gap-2">
        <Skeleton className="h-4 w-full" />
        <Skeleton className="h-4 w-11/12" />
        <Skeleton className="h-4 w-4/5" />
      </div>
    </div>
  )
}

function ToolButton({
  label,
  children,
  ...props
}: { label: string; children: ReactNode } & Omit<ButtonProps, 'children'>) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={label} {...props}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

function Toolbar({ message, onBack }: { message?: MessageDetail; onBack?: () => void }) {
  const [copied, setCopied] = useState(false)
  const [sourceOpen, setSourceOpen] = useState(false)
  const reply = message
    ? replyMailto(
        message,
        strings.reader.quoteAttribution(senderLabel(message.from, strings.list.noSender)),
      )
    : undefined

  useEffect(() => {
    if (!copied) return undefined
    const timer = window.setTimeout(() => setCopied(false), 1500)
    return () => window.clearTimeout(timer)
  }, [copied])

  const copyLink = async () => {
    try {
      await navigator.clipboard.writeText(window.location.href)
      setCopied(true)
    } catch {
      setCopied(false)
    }
  }

  return (
    <div
      role="toolbar"
      aria-label={strings.reader.toolbar}
      className="flex h-12 shrink-0 items-center gap-1 border-b border-border px-2"
    >
      {onBack && (
        <ToolButton label={strings.reader.back} onClick={onBack}>
          <ArrowLeftIcon />
        </ToolButton>
      )}
      <div className="flex-1" />
      {message && (
        <>
          {reply && (
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="sm" className="h-8" asChild>
                  <a href={reply}>
                    <ReplyIcon />
                    {strings.reader.reply}
                  </a>
                </Button>
              </TooltipTrigger>
              <TooltipContent>{strings.reader.reply}</TooltipContent>
            </Tooltip>
          )}
          <Tooltip>
            <TooltipTrigger asChild>
              <Button variant="ghost" size="icon" asChild>
                <a href={rawUrl(message.id)} download aria-label={strings.reader.download}>
                  <DownloadIcon />
                </a>
              </Button>
            </TooltipTrigger>
            <TooltipContent>{strings.reader.download}</TooltipContent>
          </Tooltip>
          <ToolButton label={strings.reader.source} onClick={() => setSourceOpen(true)}>
            <CodeIcon />
          </ToolButton>
          <ToolButton
            label={copied ? strings.reader.copied : strings.reader.copyLink}
            onClick={() => void copyLink()}
          >
            {copied ? <CheckIcon /> : <LinkIcon />}
          </ToolButton>
          <span aria-live="polite" className="sr-only">
            {copied ? strings.reader.copied : ''}
          </span>
          <SourceDialog messageId={message.id} open={sourceOpen} onOpenChange={setSourceOpen} />
        </>
      )}
    </div>
  )
}

function SourceDialog({
  messageId,
  open,
  onOpenChange,
}: {
  messageId: string
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const raw = useQuery({ ...rawQuery(messageId), enabled: open })
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-4xl">
        <DialogHeader>
          <DialogTitle>{strings.reader.sourceTitle}</DialogTitle>
          <DialogDescription>{strings.reader.sourceDescription}</DialogDescription>
        </DialogHeader>
        {raw.isError ? (
          <p className="text-sm text-danger">{strings.reader.sourceFailed}</p>
        ) : raw.data === undefined ? (
          <div className="grid gap-2">
            <Skeleton className="h-3 w-3/4" />
            <Skeleton className="h-3 w-2/3" />
            <Skeleton className="h-3 w-4/5" />
          </div>
        ) : (
          <pre
            tabIndex={0}
            className="max-h-[70vh] overflow-auto border border-border bg-bg-subtle p-4 font-mono text-xs break-all whitespace-pre-wrap text-fg-muted outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
          >
            {raw.data}
          </pre>
        )}
      </DialogContent>
    </Dialog>
  )
}
