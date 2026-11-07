import {
  type ActionInsight,
  type CodeInsight,
  type InvoiceInsight,
  type MessageDetail,
  type OrderInsight,
  type ParcelInsight,
  type PassInsight,
  type ReservationInsight,
  sessionQuery,
  type Trust,
} from '@holtmail/api-client'
import { Badge, Button, CopyButton, cn } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { type KeyboardEvent, useState } from 'react'

import { useNow } from '../hooks/use-now.ts'
import { aliasProviderName } from '../lib/aliases.ts'
import { canCategorize, cardChips, type Chip, isCategory, trustNote } from '../lib/categories.ts'
import {
  actionBadges,
  actionLabel,
  bookingReference,
  type CardView,
  cardView,
  carrierName,
  codeAge,
  codeTime,
  copyValue,
  groupCode,
  isStale,
  passLabel,
  passParts,
  purchaseParts,
  reservationLabel,
  reservationParts,
  shownStatus as canShow,
} from '../lib/insights.ts'
import { manages } from '../lib/roles.ts'
import { strings } from '../strings.ts'
import { CategoryMenu } from './category-menu.tsx'
import { insightRowSelector, InsightRow } from './row.tsx'
import { UnsubscribeControl } from './unsubscribe-control.tsx'

export const insightCopySelector = '[data-insight-copy]'
export const insightUnsubscribeSelector = '[data-insight-unsubscribe]'
export const insightLinkSelector = '[data-insight-link]'
export const insightCategorySelector = '[data-insight-category]'
const rowCopySelector = '[data-row-copy]'
const rowLinkSelector = '[data-row-link]'

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

function clickIn(row: HTMLElement, selector: string) {
  row.querySelector<HTMLElement>(selector)?.click()
}

function rowKeys(
  event: KeyboardEvent<HTMLDivElement>,
  enter: (row: HTMLElement) => void,
  copy?: (row: HTMLElement) => void,
) {
  if (event.target !== event.currentTarget) return
  const plain = !event.metaKey && !event.ctrlKey && !event.altKey && !event.shiftKey
  if (event.key === 'ArrowDown') moveFocus(event, 1)
  else if (event.key === 'ArrowUp') moveFocus(event, -1)
  else if (event.key === 'Enter' && plain) {
    event.preventDefault()
    enter(event.currentTarget)
  } else if (event.key.toLowerCase() === 'c' && plain && copy) {
    event.preventDefault()
    copy(event.currentTarget)
  }
}

const copyRow = (row: HTMLElement) => clickIn(row, rowCopySelector)

const actionKeys = (event: KeyboardEvent<HTMLDivElement>) =>
  rowKeys(event, (row) => clickIn(row, rowLinkSelector))

const copyKeys = (event: KeyboardEvent<HTMLDivElement>) => rowKeys(event, copyRow, copyRow)

const linkOrCopyKeys = (event: KeyboardEvent<HTMLDivElement>) =>
  rowKeys(
    event,
    (row) => (row.querySelector(rowLinkSelector) ? clickIn(row, rowLinkSelector) : copyRow(row)),
    copyRow,
  )

function CodeRow({
  code,
  time,
  now,
  top,
}: {
  code: CodeInsight
  time: number | undefined
  now: number
  top: boolean
}) {
  const t = strings.insights
  const [revealed, setRevealed] = useState(false)
  const collapsed = isStale(time, now) && !revealed
  const age = codeAge(time, now)

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) =>
    rowKeys(event, (row) => (collapsed ? setRevealed(true) : copyRow(row)), copyRow)

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
        data-row-copy=""
        data-insight-copy={top ? '' : undefined}
      />
    </InsightRow>
  )
}

function ActionRow({ action, trust }: { action: ActionInsight; trust: Trust }) {
  const t = strings.insights
  const label = actionLabel(action.action)
  const badges = actionBadges(action, trust)
  return (
    <InsightRow
      label={t.link}
      name={t.actionName(label, action.display_host)}
      onKeyDown={actionKeys}
    >
      <a
        href={action.url}
        target="_blank"
        rel="noopener noreferrer"
        referrerPolicy="no-referrer"
        data-row-link=""
        data-insight-link=""
        className={cn(linkClass, 'min-w-0')}
      >
        <span>{label}</span>
        <span aria-hidden>→</span>
        <span className="min-w-0 font-mono break-all">{action.display_host}</span>
      </a>
      {action.via && <span className="text-xs text-fg-subtle">{t.via(action.via)}</span>}
      {badges.map((badge) => (
        <Badge key={badge.key} variant={badge.warning ? 'warning' : 'outline'}>
          {badge.text}
        </Badge>
      ))}
      {action.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
    </InsightRow>
  )
}

function ParcelRow({
  parcel,
  topCopy,
  topLink,
}: {
  parcel: ParcelInsight
  topCopy: boolean
  topLink: boolean
}) {
  const t = strings.insights
  const carrier = carrierName(parcel.carrier)
  return (
    <InsightRow
      label={t.parcel}
      name={t.parcelName(carrier, parcel.number)}
      onKeyDown={linkOrCopyKeys}
    >
      <span className="text-fg-subtle">{carrier}</span>
      <span className="font-mono break-all text-fg tabular-nums">{parcel.number}</span>
      <CopyButton
        text={parcel.number}
        label={t.copyNumber}
        copiedLabel={t.copied}
        iconOnly
        data-row-copy=""
        data-insight-copy={topCopy ? '' : undefined}
      />
      {parcel.track_url && parcel.track_host && (
        <a
          href={parcel.track_url}
          target="_blank"
          rel="noopener noreferrer"
          referrerPolicy="no-referrer"
          data-row-link=""
          data-insight-link={topLink ? '' : undefined}
          className={linkClass}
        >
          <span>{t.track}</span>
          <span aria-hidden>→</span>
          <span className="font-mono">{parcel.track_host}</span>
        </a>
      )}
      {parcel.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
    </InsightRow>
  )
}

function Parts({ parts }: { parts: readonly string[] }) {
  return parts.map((part, index) => (
    <span key={`${index}-${part}`} className="flex items-center gap-2">
      {index > 0 && (
        <span aria-hidden className="text-fg-subtle">
          ·
        </span>
      )}
      <span className={index === 0 ? 'text-fg' : undefined}>{part}</span>
    </span>
  ))
}

function Reference({
  value,
  prefix,
  label,
  top,
}: {
  value: string
  prefix?: string
  label: string
  top: boolean
}) {
  const t = strings.insights
  return (
    <span className="flex items-center gap-1">
      {prefix && <span className="text-fg-subtle">{prefix}</span>}
      <span className="font-mono break-all text-fg tabular-nums">{value}</span>
      <CopyButton
        text={value}
        label={label}
        copiedLabel={t.copied}
        iconOnly
        data-row-copy=""
        data-insight-copy={top ? '' : undefined}
      />
    </span>
  )
}

function OpenLink({ url, host }: { url?: string | null; host?: string | null }) {
  if (!url || !host) return null
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      referrerPolicy="no-referrer"
      data-row-link=""
      className={linkClass}
    >
      <span>{strings.insights.open}</span>
      <span aria-hidden>→</span>
      <span className="font-mono break-all">{host}</span>
    </a>
  )
}

function ReservationRow({
  item,
  year,
  top,
}: {
  item: ReservationInsight
  year: number | undefined
  top: boolean
}) {
  const t = strings.insights
  const { reservation } = item
  const label = reservationLabel(reservation.kind)
  const parts = reservationParts(reservation, year)
  const reference = bookingReference(item)
  return (
    <InsightRow
      label={label}
      name={t.rowName(label, reference ? [...parts, `${t.ref} ${reference}`] : parts)}
      onKeyDown={linkOrCopyKeys}
    >
      <Parts parts={parts} />
      {reference && (
        <Reference value={reference} prefix={t.ref} label={t.copyReference} top={top} />
      )}
      <OpenLink url={reservation.url} host={reservation.url_host} />
      {item.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
    </InsightRow>
  )
}

function PassRow({
  pass,
  year,
  top,
}: {
  pass: PassInsight
  year: number | undefined
  top: boolean
}) {
  const t = strings.insights
  const label = passLabel(pass.style)
  const parts = passParts(pass, year)
  const pnr = pass.boarding?.pnr
  return (
    <InsightRow
      label={label}
      name={t.rowName(label, pnr ? [...parts, `${t.ref} ${pnr}`] : parts)}
      onKeyDown={copyKeys}
    >
      <Parts parts={parts} />
      {pnr && <Reference value={pnr} prefix={t.ref} label={t.copyReference} top={top} />}
      {pass.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
    </InsightRow>
  )
}

function PurchaseRow({
  item,
  year,
}: {
  item: OrderInsight | InvoiceInsight
  year: number | undefined
}) {
  const t = strings.insights
  const label = item.type === 'order' ? t.order : t.invoice
  const parts = purchaseParts(item, year)
  return (
    <InsightRow
      label={label}
      name={t.rowName(label, item.number ? [item.number, ...parts] : parts)}
      onKeyDown={linkOrCopyKeys}
    >
      {item.number && (
        <Reference
          value={item.number}
          label={item.type === 'order' ? t.copyOrderNumber : t.copyInvoiceNumber}
          top={false}
        />
      )}
      <Parts parts={parts} />
      <OpenLink url={item.url} host={item.url_host} />
      {item.forwarded && <Badge variant="outline">{t.forwarded}</Badge>}
    </InsightRow>
  )
}

function hasRows(view: CardView | undefined) {
  if (!view) return false
  return (
    Boolean(view.action) ||
    [view.codes, view.reservations, view.passes, view.parcels, view.orders, view.invoices].some(
      (list) => list.length > 0,
    )
  )
}

const chipClass =
  'border-border text-fg-muted transition-colors duration-fast-02 ease-productive hover:border-border-strong hover:text-fg focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

function ChipLink({ archiveId, chip }: { archiveId: string; chip: Chip }) {
  if (!isCategory(chip.id)) return <Badge variant="outline">{chip.label}</Badge>
  return (
    <Badge variant="outline" asChild className={chipClass}>
      <Link to="/a/$archiveId" params={{ archiveId }} search={{ category: chip.id }}>
        {chip.label}
      </Link>
    </Badge>
  )
}

const hasLastRequest = (message: MessageDetail) =>
  Boolean(message.insights?.unsubscribe?.last_request)

function Footer({
  message,
  view,
  canEdit,
}: {
  message: MessageDetail
  view: CardView | undefined
  canEdit: boolean
}) {
  const t = strings.insights
  const unsubscribe = Boolean(view?.unsubscribe || view?.oneClick) || hasLastRequest(message)
  const chips = cardChips(message.insights)
  const note = message.insights ? trustNote(message.insights.trust, aliasProviderName) : undefined
  const chipList = [...(chips.kind ? [chips.kind] : []), ...chips.topics]
  if (!unsubscribe && !note && chipList.length === 0 && !canEdit) return null
  return (
    <footer
      className={cn(
        'flex flex-wrap items-center justify-between gap-2 text-xs',
        hasRows(view) && 'border-t border-border pt-3',
      )}
    >
      <span className="flex min-w-0 flex-wrap items-center gap-2">
        {chipList.length > 0 && (
          <ul aria-label={strings.categories.shown} className="flex flex-wrap items-center gap-1">
            {chipList.map((chip) => (
              <li key={chip.id}>
                <ChipLink archiveId={message.archive_id} chip={chip} />
              </li>
            ))}
          </ul>
        )}
        {canEdit && <CategoryMenu message={message} />}
        {note && (
          <span
            className={note.tone === 'warning' ? 'text-warning' : 'text-fg-subtle'}
            title={note.tone === 'muted' ? t.notCheckedHint : undefined}
          >
            {note.text}
          </span>
        )}
      </span>
      {unsubscribe && <UnsubscribeControl message={message} view={view} />}
    </footer>
  )
}

export function InsightsCard({ message }: { message: MessageDetail }) {
  const now = useNow(30_000)
  const canEdit = manages(useQuery(sessionQuery).data?.role) && canCategorize(message)
  const view = cardView(message.insights, message.invitations)
  const chips = cardChips(message.insights)
  if (!view && !canEdit && !chips.kind && chips.topics.length === 0) return null
  if (!message.insights || !canShow(message.insights.status)) return null
  const time = codeTime(message)
  const year = time === undefined ? undefined : new Date(time).getFullYear()
  const reference = view?.copy?.kind === 'reference' ? view.copy.value : undefined
  const topReservation = reference
    ? view?.reservations.find((item) => bookingReference(item) === reference)
    : undefined
  const topPass =
    reference && !topReservation
      ? view?.passes.find((pass) => pass.boarding?.pnr === reference)
      : undefined
  const trackingLink = view?.link?.kind === 'tracking' ? view.link.href : undefined
  const trust: Trust = message.insights?.trust ?? { state: 'unknown' }
  return (
    <section
      aria-label={strings.insights.shown}
      className="grid gap-3 border border-border bg-bg-subtle p-4"
    >
      {view && hasRows(view) && (
        <dl className="grid gap-1 text-sm">
          {view.codes.map((code, index) => (
            <CodeRow
              key={`${code.value}-${index}`}
              code={code}
              time={time}
              now={now}
              top={index === 0}
            />
          ))}
          {view.action && <ActionRow action={view.action} trust={trust} />}
          {view.reservations.map((item, index) => (
            <ReservationRow
              key={`reservation-${index}`}
              item={item}
              year={year}
              top={item === topReservation}
            />
          ))}
          {view.passes.map((pass) => (
            <PassRow
              key={`pass-${pass.attachment}`}
              pass={pass}
              year={year}
              top={pass === topPass}
            />
          ))}
          {view.parcels.map((parcel, index) => (
            <ParcelRow
              key={`${parcel.carrier}-${parcel.number}`}
              parcel={parcel}
              topCopy={view.copy?.kind === 'tracking' && index === 0}
              topLink={Boolean(trackingLink) && parcel.track_url === trackingLink}
            />
          ))}
          {[...view.orders, ...view.invoices].map((item, index) => (
            <PurchaseRow key={`${item.type}-${index}`} item={item} year={year} />
          ))}
        </dl>
      )}
      <Footer message={message} view={view} canEdit={canEdit} />
    </section>
  )
}
