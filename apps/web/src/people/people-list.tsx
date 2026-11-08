import {
  collectionsQuery,
  contactsExportUrl,
  peopleInfiniteQuery,
  type PersonSummary,
  sessionQuery,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Input,
  Skeleton,
} from '@holtmail/ui'
import { useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useSearch } from '@tanstack/react-router'
import { useVirtualizer } from '@tanstack/react-virtual'
import {
  CircleAlertIcon,
  DownloadIcon,
  FileArchiveIcon,
  FileTextIcon,
  FileUpIcon,
  ListFilterIcon,
  MailIcon,
  SearchIcon,
  SearchXIcon,
  SettingsIcon,
  UsersIcon,
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
} from 'react'

import { NewItemButton } from '../items/new-item-button.tsx'
import { contactsExportFormats, contactsScope } from '../lib/export.ts'
import { formatCount } from '../lib/format.ts'
import { initials, personLabel, sourceOptions } from '../lib/people.ts'
import { manages } from '../lib/roles.ts'
import type { PeopleSearch } from '../lib/search-params.ts'
import { ids, useMail } from '../mail/context.ts'
import { EmptyState } from '../mail/empty-state.tsx'
import { strings } from '../strings.ts'

const t = strings.people

function remPx(): number {
  if (typeof document === 'undefined') return 16
  return Number.parseFloat(getComputedStyle(document.documentElement).fontSize) || 16
}

const rowRem = 3.5

export function PeopleListPane() {
  const { archiveId } = useMail()
  const search = useSearch({ from: '/a/$archiveId/people' })
  const filters = { q: search.q, source: search.source, differences: search.diff }
  const people = useInfiniteQuery(peopleInfiniteQuery(archiveId, filters))
  const filtered = Boolean(search.q?.trim() || search.source || search.diff)

  const items = useMemo(() => {
    const seen = new Set<string>()
    return (people.data?.pages ?? [])
      .flatMap((page) => page.items)
      .filter((item) => !seen.has(item.id) && Boolean(seen.add(item.id)))
  }, [people.data])
  const total = people.data?.pages[0]?.total
  const differing = useInfiniteQuery(
    peopleInfiniteQuery(archiveId, { ...filters, differences: true }),
  ).data?.pages[0]?.total
  const navigate = useNavigate()
  const toggleDifferences = () =>
    void navigate({
      to: '/a/$archiveId/people',
      params: { archiveId },
      search: { ...search, diff: search.diff ? undefined : true, p: undefined },
      replace: true,
    })

  return (
    <div className="flex h-full min-h-0 flex-col">
      <header className="grid shrink-0 gap-3 border-b border-border px-4 pt-3 pb-3">
        <div className="flex h-8 items-center gap-3">
          <h1 className="truncate text-lg font-medium">{t.title}</h1>
          {total !== undefined && (
            <span className="ml-auto shrink-0 font-mono text-xs text-fg-subtle tabular-nums">
              {formatCount(total)}
            </span>
          )}
          <span className={cn('-mr-2 flex shrink-0', total === undefined && 'ml-auto')}>
            <NewItemButton archiveId={archiveId} kind="contact" iconOnly />
            <ContactsExportMenu archiveId={archiveId} source={search.source} />
            <FilterMenu archiveId={archiveId} search={search} />
          </span>
        </div>
        {(differing ?? 0) > 0 && (
          <div className="flex">
            <Button
              size="sm"
              variant={search.diff ? 'primary' : 'secondary'}
              aria-pressed={Boolean(search.diff)}
              title={t.differencesCount(formatCount(differing ?? 0))}
              onClick={toggleDifferences}
            >
              <span aria-hidden className="font-mono">
                ≠
              </span>
              {t.differencesFilter}
              <span className="font-mono tabular-nums">{formatCount(differing ?? 0)}</span>
            </Button>
          </div>
        )}
        <PeopleSearchField query={search.q ?? ''} />
      </header>
      {people.isPending ? (
        <SkeletonRows />
      ) : people.isError ? (
        <EmptyState icon={<CircleAlertIcon />} title={t.loadFailed}>
          <p>{people.error.message}</p>
          <Button size="sm" className="mt-4" onClick={() => void people.refetch()}>
            {strings.list.retry}
          </Button>
        </EmptyState>
      ) : items.length === 0 ? (
        filtered ? (
          <EmptyState icon={<SearchXIcon />} title={t.noMatchTitle}>
            <p>{t.noMatchLead}</p>
            <Button asChild size="sm" className="mt-4">
              <Link to="/a/$archiveId/people" params={{ archiveId }}>
                {t.clearFilters}
              </Link>
            </Button>
          </EmptyState>
        ) : (
          <NoContacts archiveId={archiveId} />
        )
      ) : (
        <VirtualPeople
          key={JSON.stringify(filters)}
          items={items}
          hasNextPage={people.hasNextPage}
          isFetchingNextPage={people.isFetchingNextPage}
          fetchNextPage={() => void people.fetchNextPage()}
        />
      )}
    </div>
  )
}

function NoContacts({ archiveId }: { archiveId: string }) {
  const user = useQuery(sessionQuery).data
  return (
    <EmptyState icon={<UsersIcon />} title={t.emptyTitle}>
      <p>{t.emptyLead}</p>
      {manages(user?.role) && (
        <div className="mt-6 flex flex-wrap justify-center gap-2">
          <Button asChild variant="primary" size="sm">
            <Link to="/settings/archives" search={{ archive: archiveId, add: 'import' }}>
              <FileUpIcon />
              {t.emptyImport}
            </Link>
          </Button>
          <Button asChild size="sm">
            <Link to="/settings/archives" search={{ archive: archiveId }}>
              <SettingsIcon />
              {t.emptyConnect}
            </Link>
          </Button>
        </div>
      )}
    </EmptyState>
  )
}

function FilterMenu({ archiveId, search }: { archiveId: string; search: PeopleSearch }) {
  const navigate = useNavigate()
  const collections = useQuery(collectionsQuery(archiveId))
  const options = useMemo(
    () =>
      sourceOptions(collections.data ?? [], {
        all: t.everyone,
        contacts: t.withCards,
        mail: t.fromMail,
      }),
    [collections.data],
  )
  const active = Boolean(search.source || search.diff)
  const update = (next: PeopleSearch) =>
    void navigate({
      to: '/a/$archiveId/people',
      params: { archiveId },
      search: { ...search, ...next, p: undefined },
      replace: true,
    })
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t.filters} className="relative">
          <ListFilterIcon />
          {active && <span aria-hidden className="absolute top-1.5 right-1.5 size-1.5 bg-accent" />}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        <DropdownMenuLabel>{t.source}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={search.source ?? 'all'}
          onValueChange={(value) => update({ source: value === 'all' ? undefined : value })}
        >
          {options.map((option) => (
            <DropdownMenuRadioItem key={option.value} value={option.value}>
              <span className="truncate">{option.label}</span>
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuCheckboxItem
          checked={Boolean(search.diff)}
          onCheckedChange={(checked) => update({ diff: checked ? true : undefined })}
        >
          {t.hasDifferences}
        </DropdownMenuCheckboxItem>
        {active && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onSelect={() => update({ source: undefined, diff: undefined })}>
              {t.clearFilters}
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function ContactsExportMenu({ archiveId, source }: { archiveId: string; source?: string }) {
  const scope = contactsScope(source)
  const [removed, setRemoved] = useState(false)
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={t.exportList} title={t.exportList}>
          <DownloadIcon />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        <DropdownMenuLabel className="text-xs font-normal text-fg-subtle">
          {t.exportWhat[scope.collection ? 'collection' : 'all']}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        {contactsExportFormats.map((format) => {
          const Icon = format === 'zip' ? FileArchiveIcon : FileTextIcon
          return (
            <DropdownMenuItem key={format} asChild>
              <a href={contactsExportUrl(archiveId, format, { ...scope, removed })} download>
                <Icon />
                <span className="grid">
                  <span>{t.exportFormat[format]}</span>
                  <span className="text-xs text-fg-subtle">{t.exportHint[format]}</span>
                </span>
              </a>
            </DropdownMenuItem>
          )
        })}
        <DropdownMenuSeparator />
        <DropdownMenuCheckboxItem
          checked={removed}
          onSelect={(event) => event.preventDefault()}
          onCheckedChange={(checked) => setRemoved(checked)}
        >
          {t.includeRemoved}
        </DropdownMenuCheckboxItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

function PeopleSearchField({ query }: { query: string }) {
  const { archiveId, list } = useMail()
  const navigate = useNavigate()
  const [value, setValue] = useState(query)
  const [synced, setSynced] = useState(query)
  if (synced !== query) {
    setSynced(query)
    setValue(query)
  }

  useEffect(() => {
    if (value.trim() === query.trim()) return undefined
    const timer = window.setTimeout(() => {
      void navigate({
        to: '/a/$archiveId/people',
        params: { archiveId },
        search: (previous) => ({ ...previous, q: value.trim() || undefined, p: undefined }),
        replace: true,
      })
    }, 200)
    return () => window.clearTimeout(timer)
  }, [value, query, archiveId, navigate])

  return (
    <form
      role="search"
      className="relative"
      onSubmit={(event) => {
        event.preventDefault()
        list.current?.focus()
      }}
    >
      <SearchIcon
        aria-hidden
        className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-fg-subtle"
      />
      <Input
        id={ids.search}
        type="search"
        value={value}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Escape') {
            event.preventDefault()
            event.currentTarget.blur()
            list.current?.focus()
          }
        }}
        placeholder={t.searchPlaceholder}
        aria-label={t.search}
        autoComplete="off"
        spellCheck={false}
        className="pl-9 [&::-webkit-search-cancel-button]:hidden"
      />
    </form>
  )
}

function SkeletonRows() {
  return (
    <div aria-busy className="min-h-0 flex-1 overflow-hidden">
      {Array.from({ length: 12 }, (_, i) => (
        <div key={i} className="flex h-14 items-center gap-3 border-b border-border px-4">
          <Skeleton className="size-8 shrink-0" />
          <div className="grid flex-1 gap-2">
            <Skeleton className={cn('h-3.5', i % 3 === 0 ? 'w-1/3' : 'w-2/5')} />
            <Skeleton className={cn('h-3', i % 2 === 0 ? 'w-1/2' : 'w-2/3')} />
          </div>
        </div>
      ))}
    </div>
  )
}

function VirtualPeople({
  items,
  hasNextPage,
  isFetchingNextPage,
  fetchNextPage,
}: {
  items: PersonSummary[]
  hasNextPage: boolean
  isFetchingNextPage: boolean
  fetchNextPage: () => void
}) {
  const { layout, selectedId, select, focusReader, list } = useMail()
  const scrollRef = useRef<HTMLDivElement>(null)
  const listboxRef = useRef<HTMLDivElement>(null)
  const [cursor, setCursor] = useState(selectedId)
  const [synced, setSynced] = useState(selectedId)
  if (synced !== selectedId) {
    setSynced(selectedId)
    if (selectedId) setCursor(selectedId)
  }
  const indexById = useMemo(() => new Map(items.map((item, index) => [item.id, index])), [items])
  const [unit] = useState(remPx)
  const rowHeight = unit * rowRem

  const count = items.length + (hasNextPage ? 1 : 0)
  const virtualizer = useVirtualizer({
    count,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => rowHeight,
    overscan: 12,
    getItemKey: (index) => items[index]?.id ?? 'loader',
  })
  const virtualItems = virtualizer.getVirtualItems()
  const lastIndex = virtualItems.at(-1)?.index ?? 0

  useEffect(() => {
    if (hasNextPage && !isFetchingNextPage && lastIndex >= items.length - 8) fetchNextPage()
  }, [hasNextPage, isFetchingNextPage, lastIndex, items.length, fetchNextPage])

  const cursorIndex = cursor === undefined ? undefined : indexById.get(cursor)

  const moveTo = useCallback(
    (index: number) => {
      const item = items[index]
      if (!item) return
      setCursor(item.id)
      virtualizer.scrollToIndex(index, { align: 'auto' })
      if (layout === 'wide' || selectedId) select(item.id, { replace: true })
    },
    [items, virtualizer, layout, selectedId, select],
  )

  const move = useCallback(
    (delta: number) => {
      if (items.length === 0) return
      const base = cursorIndex ?? (delta > 0 ? -1 : items.length)
      const next = base + delta
      if (next > items.length - 1 && hasNextPage) fetchNextPage()
      moveTo(Math.max(0, Math.min(items.length - 1, next)))
    },
    [items.length, cursorIndex, hasNextPage, fetchNextPage, moveTo],
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
    const id = event.target.closest<HTMLElement>('[data-person-id]')?.dataset.personId
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
  const highlighted =
    cursorIndex === undefined ? undefined : virtualizer.measurementsCache[cursorIndex]

  return (
    <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto overscroll-contain">
      <div
        ref={listboxRef}
        id={ids.list}
        role="listbox"
        tabIndex={0}
        aria-label={strings.landmarks.people}
        aria-activedescendant={cursor && cursorIndex !== undefined ? `person-${cursor}` : undefined}
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
          const item = items[virtual.index]
          return (
            <div
              key={virtual.key}
              data-index={virtual.index}
              className="absolute inset-x-0 top-0"
              style={{ height: virtual.size, transform: `translateY(${virtual.start}px)` }}
            >
              {item ? <PersonRow person={item} selected={item.id === cursor} /> : <LoaderRow />}
            </div>
          )
        })}
      </div>
    </div>
  )
}

function PersonRow({ person, selected }: { person: PersonSummary; selected: boolean }) {
  const label = personLabel(person, t.noName)
  const secondary = person.name ? (person.email ?? person.phone) : person.phone
  const letters = initials(person.name ?? '')
  return (
    <div
      id={`person-${person.id}`}
      role="option"
      aria-selected={selected}
      data-person-id={person.id}
      className={cn(
        'relative flex h-full cursor-default items-center gap-3 border-b border-border px-4 select-none',
        'transition-colors duration-fast-02 ease-productive group-data-[scrolling]/list:transition-none',
        !selected && 'hover:bg-bg-hover',
      )}
    >
      <span
        aria-hidden
        className="flex size-8 shrink-0 items-center justify-center border border-border bg-bg-subtle text-xs font-medium text-fg-muted [&_svg]:size-4"
      >
        {letters || <UsersIcon />}
      </span>
      <span className="grid min-w-0 flex-1 gap-0.5">
        <span
          className={cn('truncate text-sm text-fg', person.removed && 'text-fg-muted line-through')}
        >
          {label}
        </span>
        {secondary && (
          <span className="truncate font-mono text-xs text-fg-subtle">{secondary}</span>
        )}
      </span>
      <span className="flex shrink-0 items-center gap-2 text-xs text-fg-subtle">
        {person.differences && (
          <span
            title={t.differs}
            className="border border-border px-1 font-mono leading-4 text-fg-muted"
          >
            <span aria-hidden>≠</span>
            <span className="sr-only">{t.differs}</span>
          </span>
        )}
        {person.removed && <span>{t.removed}</span>}
        {person.cards === 0 ? (
          <MailIcon aria-label={t.mailOnly} className="size-3" />
        ) : (
          person.cards > 1 && (
            <span className="font-mono tabular-nums">
              <span aria-hidden>×{person.cards}</span>
              <span className="sr-only">{t.cards(person.cards)}</span>
            </span>
          )
        )}
        {person.messages > 0 && (
          <span className="w-10 text-right font-mono tabular-nums">
            <span aria-hidden>{formatCount(person.messages)}</span>
            <span className="sr-only">{t.messages(formatCount(person.messages))}</span>
          </span>
        )}
      </span>
    </div>
  )
}

function LoaderRow() {
  return (
    <div
      role="presentation"
      aria-label={t.loadingMore}
      className="flex h-full items-center gap-3 px-4"
    >
      <Skeleton className="size-8 shrink-0" />
      <Skeleton className="h-3.5 w-1/3" />
    </div>
  )
}
