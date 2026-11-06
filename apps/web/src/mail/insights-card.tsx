import type { CodeInsight, MessageDetail } from '@holtmail/api-client'
import {
  Badge,
  Button,
  CopyButton,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
  cn,
} from '@holtmail/ui'
import { MailMinusIcon } from 'lucide-react'
import { type KeyboardEvent, useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import {
  type CardView,
  cardView,
  codeAge,
  codeTime,
  copyValue,
  groupCode,
  isStale,
} from '../lib/insights.ts'
import { strings } from '../strings.ts'
import { insightRowSelector, InsightRow } from './row.tsx'

export const insightCopySelector = '[data-insight-copy]'
export const insightUnsubscribeSelector = '[data-insight-unsubscribe]'

const linkClass =
  'flex items-center gap-1 text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

function moveFocus(event: KeyboardEvent<HTMLElement>, delta: number) {
  const card = event.currentTarget.closest('section')
  if (!card) return
  const rows = [...card.querySelectorAll<HTMLElement>(insightRowSelector)]
  const next = rows[rows.indexOf(event.currentTarget) + delta]
  if (next) {
    event.preventDefault()
    next.focus()
  }
}

function clickCopy(row: HTMLElement) {
  row.querySelector<HTMLElement>(insightCopySelector)?.click()
}

function CodeRow({
  code,
  time,
  now,
}: {
  code: CodeInsight
  time: number | undefined
  now: number
}) {
  const t = strings.insights
  const [revealed, setRevealed] = useState(false)
  const collapsed = isStale(time, now) && !revealed
  const age = codeAge(time, now)

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.target !== event.currentTarget) return
    const plain = !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey
    if (event.key === 'ArrowDown') moveFocus(event, 1)
    else if (event.key === 'ArrowUp') moveFocus(event, -1)
    else if (event.key === 'Enter' && plain) {
      event.preventDefault()
      if (collapsed) setRevealed(true)
      else clickCopy(event.currentTarget)
    } else if (event.key.toLowerCase() === 'c' && plain) {
      event.preventDefault()
      clickCopy(event.currentTarget)
    }
  }

  return (
    <InsightRow label={t.code} name={collapsed ? age : t.code} onKeyDown={onKeyDown}>
      {collapsed ? (
        <>
          <span className="text-fg-muted">{age}</span>
          <Button
            variant="ghost"
            size="sm"
            aria-label={t.showCode}
            onClick={() => setRevealed(true)}
          >
            {t.show}
          </Button>
        </>
      ) : (
        <span className="font-mono text-base text-fg tabular-nums">{groupCode(code.value)}</span>
      )}
      {code.origin_bound && (
        <span className="font-mono text-xs text-fg-subtle">{code.origin_bound}</span>
      )}
      {code.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
      <CopyButton
        text={copyValue(code.value)}
        label={t.copyCode}
        copiedLabel={t.copied}
        iconOnly
        data-insight-copy=""
      />
    </InsightRow>
  )
}

function Footer({ view }: { view: CardView }) {
  const t = strings.insights
  const { unsubscribe, reason, unchecked } = view
  if (!unsubscribe && !unchecked) return null
  const link = unsubscribe && (
    <a
      href={unsubscribe.href}
      target="_blank"
      rel="noopener noreferrer"
      referrerPolicy="no-referrer"
      data-insight-unsubscribe=""
      className={linkClass}
    >
      <MailMinusIcon aria-hidden className="size-3" />
      {unsubscribe.kind === 'mailto' ? t.unsubscribeMail : t.unsubscribe}
    </a>
  )
  return (
    <footer
      className={cn(
        'flex flex-wrap items-center justify-between gap-2 text-xs',
        view.codes.length > 0 && 'border-t border-border pt-3',
      )}
    >
      {unchecked ? (
        <span className="text-fg-subtle" title={t.notCheckedHint}>
          {t.notChecked}
        </span>
      ) : (
        <span />
      )}
      {link &&
        (reason ? (
          <Tooltip>
            <TooltipTrigger asChild>{link}</TooltipTrigger>
            <TooltipContent>{reason}</TooltipContent>
          </Tooltip>
        ) : (
          link
        ))}
    </footer>
  )
}

export function InsightsCard({ message }: { message: MessageDetail }) {
  const now = useNow(30_000)
  const view = cardView(message.insights)
  if (!view) return null
  const time = codeTime(message)
  return (
    <section
      aria-label={strings.insights.shown}
      className="grid gap-3 border border-border bg-bg-subtle p-4"
    >
      {view.codes.length > 0 && (
        <dl className="grid gap-1 text-sm">
          {view.codes.map((code, index) => (
            <CodeRow key={`${code.value}-${index}`} code={code} time={time} now={now} />
          ))}
        </dl>
      )}
      <Footer view={view} />
    </section>
  )
}
