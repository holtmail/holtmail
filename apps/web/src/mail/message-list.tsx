import {
  ApiError,
  archivesQuery,
  categoriesQuery,
  foldersQuery,
  labelsQuery,
  messagesInfiniteQuery,
  type MessageSummary,
  searchInfiniteQuery,
  sessionQuery,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { Button, cn, Skeleton } from '@holtmail/ui'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { defaultRangeExtractor, type Range, useVirtualizer } from '@tanstack/react-virtual'
import {
  BellIcon,
  CircleAlertIcon,
  ClockIcon,
  KeyRoundIcon,
  NewspaperIcon,
  MailOpenIcon,
  PaperclipIcon,
  SearchXIcon,
  StarIcon,
  Trash2Icon,
} from 'lucide-react'
import { m } from 'motion/react'
import {
  type KeyboardEvent,
  type MouseEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from 'react'

import { useNow } from '../hooks/use-now.ts'
import { senderLabel } from '../lib/address.ts'
import { type Category, categoryLabel, listKindGlyph, listTopic } from '../lib/categories.ts'
import { isFlagged, isUnread } from '../lib/flags.ts'
import { formatCount, formatListDate } from '../lib/format.ts'
import { groupByDay, type ListRow } from '../lib/group.ts'
import { listChips } from '../lib/labels.ts'
import { applyOverlay, overlay, type OverlayEntry, overlayHeld } from '../lib/overlay.ts'
import { manages } from '../lib/roles.ts'
import { DeletionDialog } from '../manage/deletion-dialog.tsx'
import { strings } from '../strings.ts'
import { ids, useMail } from './context.ts'
import { EmptyArchive, EmptyState } from './empty-state.tsx'
import { ExportMenu } from './export-menu.tsx'
import { ItemHits } from './item-hits.tsx'
import { SearchField } from './search-field.tsx'
import { folderLabel } from './sidebar.tsx'
import { StillReading } from './still-reading.tsx'

export type ListSource =
  | { kind: 'all' }
  | { kind: 'folder'; folderId: string }
  | { kind: 'label'; label: string }
  | { kind: 'search'; q: string }
  | { kind: 'category'; category: Category }

function sourceKey(source: ListSource): string {
  if (source.kind === 'folder') return `f:${source.folderId}`
  if (source.kind === 'label') return `l:${source.label}`
  if (source.kind === 'search') return `s:${source.q}`
  if (source.kind === 'category') return `c:${source.category}`
  return 'all'
}

function remPx(): number {
  if (typeof document === 'undefined') return 16
  return Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16
}

const rowRem = 5.25
const headerRem = 2

export function MessageListPane({ source }: { source: ListSource }) {
  const { archiveId } = useMail()
  const folderId = source.kind === 'folder' ? source.folderId : undefined
  const label = source.kind === 'label' ? source.label : undefined
  const q = source.kind === 'search' ? source.q : ''
  const category = source.kind === 'category' ? source.category : undefined
  const isSearch = source.kind === 'search'

  const listing = useInfiniteQuery({
    ...messagesInfiniteQuery(archiveId, folderId, label, category),
    enabled: !isSearch,
  })
  const categories = useQuery({
    ...categoriesQuery(archiveId),
    enabled: category !== undefined,
    retry: false,
  })
  const search = useInfiniteQuery({
    ...searchInfiniteQuery(archiveId, q),
    enabled: isSearch && q !== '',
  })
  const archives = useQuery(archivesQuery)
  const folders = useQuery(foldersQuery(archiveId))
  const labels = useQuery({ ...labelsQuery(archiveId), enabled: label !== undefined })

  const items = useMemo(() => {
    const pages: { items: MessageSummary[] }[] =
      (isSearch ? search.data?.pages : listing.data?.pages) ?? []
    const seen = new Set<string>()
    return pages
      .flatMap((page) => page.items)
      .filter((item) => !seen.has(item.id) && Boolean(seen.add(item.id)))
  }, [isSearch, search.data, listing.data])

  const active = isSearch ? search : listing
  const searchError =
    isSearch && search.error instanceof ApiError && search.error.status === 400
      ? search.error.message
      : undefined

  let title: string = strings.archive.allMail
  let total: number | undefined = archives.data?.find((a) => a.id === archiveId)?.stats.messages
  if (source.kind === 'folder') {
    const folder = folders.data?.find((f) => f.id === source.folderId)
    title = folder ? folderLabel(folder) : ''
    total = folder?.messages
  } else if (label !== undefined) {
    title = label
    total = labels.data?.find((item) => item.name === label)?.messages
  } else if (category !== undefined) {
    title = categoryLabel(category)
    total = [...(categories.data?.kinds ?? []), ...(categories.data?.topics ?? [])].find(
      (entry) => entry.id === category,
    )?.count
  } else if (isSearch) {
    title = strings.list.searchResults
    total = search.data?.pages[0]?.total
  }

  const user = useQuery(sessionQuery).data
  const [deleting, setDeleting] = useState(false)
  const canDelete = isSearch && q.trim() !== '' && manages(user?.role) && items.length > 0
  const pending = isSearch ? q !== '' && search.isPending : listing.isPending
  const failed = active.isError && !searchError

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="grid shrink-0 gap-3 border-b border-border px-4 pt-3 pb-3">
        <div className="flex h-8 items-center gap-3">
          <h1 className="truncate text-lg font-medium">{title}</h1>
          {total !== undefined && (
            <span className="ml-auto shrink-0 font-mono text-xs text-fg-subtle tabular-nums">
              {isSearch ? strings.list.results(formatCount(total)) : formatCount(total)}
            </span>
          )}
          {items.length > 0 && (
            <span className={cn('-mr-2 flex shrink-0', total === undefined && 'ml-auto')}>
              <ExportMenu archiveId={archiveId} source={source} />
              {canDelete && (
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={strings.deletion.searchAction}
                  title={strings.deletion.searchAction}
                  onClick={() => setDeleting(true)}
                >
                  <Trash2Icon />
                </Button>
              )}
            </span>
          )}
          {canDelete && (
            <DeletionDialog
              archiveId={archiveId}
              request={{ query: q }}
              open={deleting}
              onOpenChange={setDeleting}
            />
          )}
        </div>
        <SearchField query={q} error={searchError} />
        {isSearch && <StillReading archiveId={archiveId} />}
      </header>
      {isSearch && q !== '' && !searchError && <ItemHits archiveId={archiveId} q={q} />}
      {pending ? (
        <SkeletonRows />
      ) : failed ? (
        <EmptyState icon={<CircleAlertIcon />} title={strings.list.loadFailed}>
          <p>{active.error?.message}</p>
          <Button size="sm" className="mt-4" onClick={() => void active.refetch()}>
            {strings.list.retry}
          </Button>
        </EmptyState>
      ) : items.length === 0 ? (
        searchError ? null : source.kind === 'all' ? (
          <EmptyArchive />
        ) : (
          <EmptyState
            icon={isSearch ? <SearchXIcon /> : <MailOpenIcon />}
            title={strings.list.emptyTitle}
          >
            {isSearch
              ? strings.list.emptySearch
              : label !== undefined
                ? strings.list.emptyLabel
                : category !== undefined
                  ? strings.list.emptyCategory
                  : strings.list.emptyFolder}
          </EmptyState>
        )
      ) : (
        <VirtualList
          key={sourceKey(source)}
          items={items}
          label={label}
          folderId={folderId}
          hasNextPage={active.hasNextPage}
          isFetchingNextPage={active.isFetchingNextPage}
          fetchNextPage={() => void active.fetchNextPage()}
        />
      )}
    </div>
  )
}

function SkeletonRows() {
  return (
    <div aria-busy className="min-h-0 flex-1 overflow-hidden">
      <div className="flex h-8 items-center border-b border-border px-4">
        <Skeleton className="h-3 w-16" />
      </div>
      {Array.from({ length: 10 }, (_, i) => (
        <div key={i} className="grid h-21 content-center gap-2 border-b border-border px-4">
          <div className="flex justify-between gap-4">
            <Skeleton className={cn('h-3.5', i % 3 === 0 ? 'w-1/3' : 'w-2/5')} />
            <Skeleton className="h-3.5 w-10" />
          </div>
          <Skeleton className={cn('h-3.5', i % 2 === 0 ? 'w-3/4' : 'w-2/3')} />
          <Skeleton className="h-3.5 w-11/12" />
        </div>
      ))}
    </div>
  )
}

function VirtualList({
  items,
  label,
  folderId,
  hasNextPage,
  isFetchingNextPage,
  fetchNextPage,
}: {
  items: MessageSummary[]
  label?: string
  folderId?: string
  hasNextPage: boolean
  isFetchingNextPage: boolean
  fetchNextPage: () => void
}) {
  const { layout, selectedId, select, focusReader, list } = useMail()
  const now = useNow()
  useSyncExternalStore(overlay.subscribe, overlay.snapshot, overlay.snapshot)
  const scrollRef = useRef<HTMLDivElement>(null)
  const listboxRef = useRef<HTMLDivElement>(null)
  const [cursor, setCursor] = useState(selectedId)
  const [synced, setSynced] = useState(selectedId)
  if (synced !== selectedId) {
    setSynced(selectedId)
    if (selectedId) setCursor(selectedId)
  }

  const rows = useMemo(() => groupByDay(items, now), [items, now])
  const headerIndexes = useMemo(
    () => rows.flatMap((row, index) => (row.kind === 'header' ? [index] : [])),
    [rows],
  )
  const rowIndexById = useMemo(() => {
    const map = new Map<string, number>()
    rows.forEach((row, index) => {
      if (row.kind === 'item') map.set(row.item.id, index)
    })
    return map
  }, [rows])

  const [unit] = useState(remPx)
  const rowHeight = unit * rowRem
  const headerHeight = unit * headerRem
  const stickyRef = useRef(0)

  const rangeExtractor = useCallback(
    (range: Range) => {
      let sticky = headerIndexes[0] ?? 0
      for (const index of headerIndexes) {
        if (index > range.startIndex) break
        sticky = index
      }
      stickyRef.current = sticky
      return [...new Set([sticky, ...defaultRangeExtractor(range)])].toSorted((a, b) => a - b)
    },
    [headerIndexes],
  )

  const count = rows.length + (hasNextPage ? 1 : 0)
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => (rows[index]?.kind === 'header' ? headerHeight : rowHeight),
    overscan: 10,
    rangeExtractor,
    getItemKey: (index) => rows[index]?.key ?? 'loader',
  })
  const virtualItems = virtualizer.getVirtualItems()
  const lastIndex = virtualItems.at(-1)?.index ?? 0

  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage && lastIndex >= rows.length - 8) fetchNextPage()
  }, [hasNextPage, isFetchingNextPage, lastIndex, rows.length, fetchNextPage])

  const cursorIndex = cursor === undefined ? undefined : rowIndexById.get(cursor)
  const highlighted =
    cursorIndex === undefined ? undefined : virtualizer.measurementsCache[cursorIndex]

  const moveTo = useCallback(
    (index: number) => {
      const row = rows[index]
      if (row?.kind !== 'item') return
      setCursor(row.item.id)
      virtualizer.scrollToIndex(index, { align: 'auto' })
      if (layout === 'wide' || selectedId) select(row.item.id, { replace: true })
    },
    [rows, virtualizer, layout, selectedId, select],
  )

  const move = useCallback(
    (delta: number) => {
      const itemIndexes = [...rowIndexById.values()]
      if (itemIndexes.length === 0) return
      const position = cursorIndex === undefined ? -1 : itemIndexes.indexOf(cursorIndex)
      const base = position === -1 ? (delta > 0 ? -1 : itemIndexes.length) : position
      const next = base + delta
      const clamped = Math.max(0, Math.min(itemIndexes.length - 1, next))
      if (next > itemIndexes.length - 1 && hasNextPage) fetchNextPage()
      const target = itemIndexes[clamped]
      if (target !== undefined) moveTo(target)
    },
    [rowIndexById, cursorIndex, hasNextPage, fetchNextPage, moveTo],
  )

  const open = useCallback(() => {
    const id = cursor ?? items[0]?.id
    if (!id) return
    if (id !== selectedId) select(id)
    focusReader()
  }, [cursor, items, selectedId, select, focusReader])

  useEffect(() => {
    list.current = {
      move,
      open,
      focus: () => listboxRef.current?.focus({ preventScroll: true }),
    }
    return () => {
      list.current = null
    }
  }, [list, move, open])

  const onClick = (event: MouseEvent<HTMLDivElement>) => {
    if (!(event.target instanceof Element)) return
    const id = event.target.closest<HTMLElement>('[data-message-id]')?.dataset.messageId
    if (!id) return
    setCursor(id)
    if (id !== selectedId) select(id)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const keys: Record<string, () => void> = {
      ArrowDown: () => move(1),
      ArrowUp: () => move(-1),
      Home: () => move(-Infinity),
      End: () => move(Infinity),
      Enter: open,
    }
    const action = keys[event.key]
    if (!action || event.metaKey || event.ctrlKey || event.altKey) return
    event.preventDefault()
    action()
  }

  const scrolling = virtualizer.isScrolling

  return (
    <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div
        ref={listboxRef}
        id={ids.list}
        role="listbox"
        tabIndex={0}
        aria-label={strings.landmarks.messages}
        aria-activedescendant={cursor && cursorIndex !== undefined ? `msg-${cursor}` : undefined}
        data-scrolling={scrolling || undefined}
        onClick={onClick}
        onKeyDown={onKeyDown}
        className="group/list relative w-full outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
        style={{ height: virtualizer.getTotalSize() }}
      >
        {highlighted && (
          <m.div
            aria-hidden
            className="absolute inset-x-0 top-0 border-l-2 border-accent bg-bg-selected"
            style={{ height: highlighted.size }}
            initial={false}
            animate={{
              y: highlighted.start,
              transition: scrolling ? { duration: 0 } : transition.productive.moderate01,
            }}
          />
        )}
        {virtualItems.map((virtual) => {
          const row = rows[virtual.index]
          const isSticky = row?.kind === 'header' && virtual.index === stickyRef.current
          return (
            <div
              key={virtual.key}
              data-index={virtual.index}
              className={cn(
                'inset-x-0 top-0',
                isSticky ? 'sticky z-20' : 'absolute',
                row?.kind === 'header' && !isSticky && 'z-10',
              )}
              style={{
                height: virtual.size,
                transform: isSticky ? undefined : `translateY(${virtual.start}px)`,
              }}
            >
              {row ? (
                <Row
                  row={row}
                  now={now}
                  label={label}
                  folderId={folderId}
                  entry={row.kind === 'item' ? overlay.get(row.item.id) : undefined}
                  selected={row.kind === 'item' && row.item.id === cursor}
                />
              ) : (
                <LoaderRow />
              )}
            </div>
          )
        })}
      </div>
    </div>
  )
}

function Row({
  row,
  now,
  label,
  folderId,
  entry,
  selected,
}: {
  row: ListRow<MessageSummary>
  now: number
  label?: string
  folderId?: string
  entry?: OverlayEntry
  selected: boolean
}) {
  if (row.kind === 'header') {
    return (
      <div
        role="presentation"
        className="flex h-full items-center border-b border-border bg-bg px-4 text-xs font-medium text-fg-muted"
      >
        {row.label}
      </div>
    )
  }
  const message = row.item
  const sender = senderLabel(message.from, strings.list.noSender)
  const flags = applyOverlay(message.flags, entry)
  const unread = isUnread(flags)
  const flagged = isFlagged(flags)
  const held = overlayHeld(message.flags, entry, folderId)
  const leaving = folderId !== undefined && entry?.leaves.includes(folderId) === true
  const chips = listChips(message.labels, label)
  const glyph = listKindGlyph(message.kind)
  const topic = listTopic(message.topic)
  const GlyphIcon = glyph === 'bulk' ? NewspaperIcon : BellIcon
  return (
    <div
      id={`msg-${message.id}`}
      role="option"
      aria-selected={selected}
      data-message-id={message.id}
      data-unread={unread || undefined}
      className={cn(
        'relative grid h-full cursor-default content-center gap-0.5 border-b border-border px-4 select-none',
        'transition-colors duration-fast-02 ease-productive group-data-[scrolling]/list:transition-none',
        !selected && 'hover:bg-bg-hover',
        leaving && 'opacity-60',
      )}
    >
      {unread && (
        <span className="absolute top-1/2 left-1.5 size-1.5 -translate-y-1/2 bg-accent">
          <span className="sr-only">{strings.list.unread}</span>
        </span>
      )}
      <div className="flex items-baseline gap-2">
        <span
          className={cn(
            'truncate text-sm',
            unread ? 'font-medium text-fg' : 'font-normal text-fg-muted',
          )}
        >
          {sender}
        </span>
        {message.thread_count > 1 && (
          <span className="shrink-0 self-center border border-border px-1 font-mono text-xs leading-4 text-fg-subtle tabular-nums">
            <span aria-hidden>{message.thread_count}</span>
            <span className="sr-only">{strings.list.inThread(message.thread_count)}</span>
          </span>
        )}
        {message.attachment_match ? (
          <span
            title={strings.list.attachmentMatch}
            className="flex shrink-0 items-center gap-1 self-center border border-border px-1 text-xs leading-4 text-fg-muted"
          >
            <PaperclipIcon aria-hidden className="size-3" />
            <span aria-hidden>{strings.list.inAttachment}</span>
            <span className="sr-only">{strings.list.attachmentMatch}</span>
          </span>
        ) : (
          message.attachment_count > 0 && (
            <PaperclipIcon
              aria-label={strings.list.attachments(message.attachment_count)}
              className="size-3 shrink-0 self-center text-fg-subtle"
            />
          )
        )}
        {message.insight_kinds.includes('code') && (
          <KeyRoundIcon
            aria-label={strings.list.hasCode}
            className="size-3 shrink-0 self-center text-fg-subtle"
          />
        )}
        {glyph && (
          <GlyphIcon
            aria-label={strings.categories.glyphs[glyph]}
            className="size-3 shrink-0 self-center text-fg-subtle"
          />
        )}
        {held && (
          <ClockIcon
            aria-label={strings.writing.mailbox.held}
            className="size-3 shrink-0 self-center text-fg-subtle"
          />
        )}
        {flagged && (
          <StarIcon
            aria-label={strings.list.flagged}
            className="size-3 shrink-0 self-center fill-fg-muted text-fg-muted"
          />
        )}
        <time
          dateTime={new Date(message.date).toISOString()}
          className="ml-auto shrink-0 font-mono text-xs text-fg-subtle tabular-nums"
        >
          {formatListDate(message.date, now)}
        </time>
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <span className={cn('truncate text-sm text-fg', unread ? 'font-medium' : 'font-normal')}>
          {message.subject || strings.list.noSubject}
        </span>
        {(chips.shown.length > 0 || topic) && (
          <span className="ml-auto flex shrink-0 items-center gap-1">
            {topic && (
              <span className="max-w-24 truncate border border-border px-1 text-xs leading-4 text-fg-muted">
                <span className="sr-only">{strings.categories.shown}: </span>
                {topic.label}
              </span>
            )}
            {chips.shown.length > 0 && <span className="sr-only">{strings.labels.shown}</span>}
            {chips.shown.map((name) => (
              <span
                key={name}
                className="max-w-24 truncate border border-border px-1 text-xs leading-4 text-fg-subtle"
              >
                {name}
              </span>
            ))}
            {chips.more > 0 && (
              <span className="font-mono text-xs text-fg-subtle tabular-nums">
                <span aria-hidden>+{chips.more}</span>
                <span className="sr-only">{strings.labels.more(chips.more)}</span>
              </span>
            )}
          </span>
        )}
      </div>
      <div className="truncate text-sm text-fg-subtle">{message.preview}</div>
    </div>
  )
}

function LoaderRow() {
  return (
    <div
      role="presentation"
      aria-label={strings.list.loadingMore}
      className="grid h-full content-center gap-2 px-4"
    >
      <Skeleton className="h-3.5 w-1/3" />
      <Skeleton className="h-3.5 w-3/4" />
      <Skeleton className="h-3.5 w-11/12" />
    </div>
  )
}
