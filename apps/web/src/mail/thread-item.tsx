import type { Address, MessageDetail } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { cn } from '@holtmail/ui'
import { AnimatePresence, m, useReducedMotion } from 'motion/react'
import { useId } from 'react'

import { formatAddress, senderLabel } from '../lib/address.ts'
import { formatFullDate, formatListDate } from '../lib/format.ts'
import { previewOf } from '../lib/text.ts'
import { strings } from '../strings.ts'
import { Attachments } from './attachments.tsx'
import { MessageBody } from './message-body.tsx'

function AddressLine({ label, addresses }: { label: string; addresses: Address[] }) {
  if (addresses.length === 0) return null
  return (
    <div className="flex gap-2 text-sm">
      <dt className="w-16 shrink-0 text-fg-subtle">{label}</dt>
      <dd className="min-w-0 text-fg-muted wrap-break-word">
        {addresses.map(formatAddress).join(', ')}
      </dd>
    </div>
  )
}

export function ThreadItem({
  message,
  expanded,
  collapsible,
  now,
  onToggle,
}: {
  message: MessageDetail
  expanded: boolean
  collapsible: boolean
  now: number
  onToggle: () => void
}) {
  const bodyId = useId()
  const reduceMotion = useReducedMotion()
  const instant = { duration: 0 }
  const sender = message.from[0]
  const name = senderLabel(message.from, strings.list.noSender)
  const attachments = message.attachments.filter((item) => !item.inline || !message.has_html)

  const summary = expanded ? (
    <span className="grid min-w-0 flex-1 gap-0.5">
      <span className="flex items-baseline gap-2">
        <span className="truncate text-base font-medium text-fg">{name}</span>
        {sender?.name && (
          <span className="truncate font-mono text-xs text-fg-subtle">{sender.email}</span>
        )}
      </span>
    </span>
  ) : (
    <span className="flex min-w-0 flex-1 items-baseline gap-3">
      <span className="w-40 shrink-0 truncate text-sm font-medium text-fg">{name}</span>
      <span className="min-w-0 flex-1 truncate text-sm text-fg-subtle">
        {message.preview || previewOf(message.text)}
      </span>
    </span>
  )

  const date = (
    <time
      dateTime={new Date(message.date).toISOString()}
      title={formatFullDate(message.date)}
      className="shrink-0 font-mono text-xs text-fg-subtle tabular-nums"
    >
      {expanded ? formatFullDate(message.date) : formatListDate(message.date, now)}
    </time>
  )

  return (
    <article className="border-b border-border last:border-b-0" aria-label={name}>
      {collapsible ? (
        <button
          type="button"
          aria-expanded={expanded}
          aria-controls={bodyId}
          aria-label={expanded ? strings.reader.collapse : strings.reader.expand(name)}
          onClick={onToggle}
          className={cn(
            'flex w-full cursor-default items-baseline gap-3 px-6 py-3 text-left',
            'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover',
            'outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
          )}
        >
          {summary}
          {date}
        </button>
      ) : (
        <div className="flex items-baseline gap-3 px-6 py-3">
          {summary}
          {date}
        </div>
      )}
      <AnimatePresence initial={false}>
        {expanded && (
          <m.div
            key="body"
            id={bodyId}
            className="overflow-hidden"
            initial={{ height: 0, opacity: 0 }}
            animate={{
              height: 'auto',
              opacity: 1,
              transition: reduceMotion ? instant : transition.expressive.moderate02,
            }}
            exit={{
              height: 0,
              opacity: 0,
              transition: reduceMotion ? instant : transition.expressiveExit.moderate02,
            }}
          >
            <div className="grid gap-6 px-6 pb-8">
              <dl className="grid gap-0.5">
                <AddressLine label={strings.reader.to} addresses={message.to} />
                <AddressLine label={strings.reader.cc} addresses={message.cc} />
                <AddressLine label={strings.reader.bcc} addresses={message.bcc} />
                {message.reply_to.some((r) => !message.from.some((f) => f.email === r.email)) && (
                  <AddressLine label={strings.reader.replyTo} addresses={message.reply_to} />
                )}
              </dl>
              <MessageBody message={message} />
              <Attachments attachments={attachments} />
            </div>
          </m.div>
        )}
      </AnimatePresence>
    </article>
  )
}
