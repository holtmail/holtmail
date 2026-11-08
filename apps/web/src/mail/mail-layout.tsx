import { foldersQuery } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  Kbd,
  Sheet,
  SheetContent,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Outlet, useMatch, useNavigate, useParams, useSearch } from '@tanstack/react-router'
import { KeyboardIcon, MenuIcon, SearchIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { lazy, Suspense, useCallback, useMemo, useRef, useState } from 'react'

import { ComposeContext, useComposeHost } from '../compose/compose-host.tsx'
import { useHotkeys } from '../hooks/use-hotkeys.ts'
import { useLayout } from '../hooks/use-layout.ts'
import { useLiveArchive } from '../hooks/use-live-archive.ts'
import { findInbox } from '../lib/folders.ts'
import { isMac, type KeyAction, keymap } from '../lib/keymap.ts'
import { displayStroke } from '../lib/shortcuts.ts'
import { DeviceNotices } from '../settings/device-notices.tsx'
import { strings } from '../strings.ts'
import { CommandPalette } from './command-palette.tsx'
import {
  ids,
  type InsightController,
  type ListController,
  MailContext,
  type MailboxController,
  type MailContextValue,
} from './context.ts'
import { Reader, ReaderEmpty } from './reader.tsx'
import { ShortcutsDialog } from './shortcuts-dialog.tsx'
import { Sidebar } from './sidebar.tsx'
import { UserMenu } from './user-menu.tsx'

const PersonPane = lazy(async () => ({
  default: (await import('../people/person-pane.tsx')).PersonPane,
}))
const PeopleEmpty = lazy(async () => ({
  default: (await import('../people/person-pane.tsx')).PeopleEmpty,
}))

export function MailLayout() {
  const { archiveId = '', folderId } = useParams({ strict: false })
  const { m: messageId, p: personId, category } = useSearch({ strict: false })
  const isSearch = Boolean(useMatch({ from: '/a/$archiveId/search', shouldThrow: false }))
  const isPeople = Boolean(useMatch({ from: '/a/$archiveId/people', shouldThrow: false }))
  const isCalendar = Boolean(useMatch({ from: '/a/$archiveId/calendar', shouldThrow: false }))
  const isOutbox = Boolean(useMatch({ from: '/a/$archiveId/outbox', shouldThrow: false }))
  const labelMatch = useMatch({ from: '/a/$archiveId/label', shouldThrow: false })
  const label = labelMatch?.search.name
  const isAllMail =
    !folderId && !isSearch && !labelMatch && !isPeople && !isCalendar && !isOutbox && !category
  const selectedId = isCalendar || isOutbox ? undefined : isPeople ? personId : messageId
  const layout = useLayout()
  const navigate = useNavigate()
  const folders = useQuery(foldersQuery(archiveId))
  useLiveArchive(archiveId)
  const list = useRef<ListController | null>(null)
  const insights = useRef<InsightController | null>(null)
  const mailbox = useRef<MailboxController | null>(null)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [shortcutsOpen, setShortcutsOpen] = useState(false)
  const [sheetOpen, setSheetOpen] = useState(false)
  const compose = useComposeHost(archiveId, layout)
  const inMail = !isCalendar && !isPeople && !isOutbox

  const select = useCallback(
    (id: string | undefined, options?: { replace?: boolean }) => {
      void navigate({
        to: '.',
        search: (previous) => (isPeople ? { ...previous, p: id } : { ...previous, m: id }),
        replace: options?.replace,
      })
    },
    [navigate, isPeople],
  )

  const focusReader = useCallback(() => {
    requestAnimationFrame(() => document.getElementById(ids.reader)?.focus({ preventScroll: true }))
  }, [])

  const context = useMemo<MailContextValue>(
    () => ({ archiveId, layout, selectedId, select, focusReader, list, insights, mailbox }),
    [archiveId, layout, selectedId, select, focusReader],
  )

  const overlay = layout !== 'wide'
  const back = useCallback(() => {
    select(undefined)
    requestAnimationFrame(() => list.current?.focus())
  }, [select])

  useHotkeys<KeyAction>(keymap, (action) => {
    switch (action) {
      case 'next':
        list.current?.move(1)
        break
      case 'previous':
        list.current?.move(-1)
        break
      case 'open':
        list.current?.open()
        break
      case 'back':
        if (document.activeElement instanceof HTMLElement) document.activeElement.blur()
        if (overlay && selectedId) back()
        else list.current?.focus()
        break
      case 'search': {
        const input = document.getElementById(ids.search)
        if (input instanceof HTMLInputElement) {
          input.focus()
          input.select()
        }
        break
      }
      case 'inbox': {
        const inbox = findInbox(folders.data ?? [])
        if (inbox) {
          void navigate({
            to: '/a/$archiveId/f/$folderId',
            params: { archiveId, folderId: inbox.id },
          })
        }
        break
      }
      case 'allMail':
        void navigate({ to: '/a/$archiveId', params: { archiveId } })
        break
      case 'people':
        void navigate({ to: '/a/$archiveId/people', params: { archiveId } })
        break
      case 'calendar':
        void navigate({ to: '/a/$archiveId/calendar', params: { archiveId } })
        break
      case 'palette':
        setPaletteOpen((open) => !open)
        break
      case 'help':
        setShortcutsOpen(true)
        break
      case 'copyCode':
        if (!isCalendar && !isPeople) insights.current?.copyCode()
        break
      case 'openLink':
        if (!isCalendar && !isPeople) insights.current?.openLink()
        break
      case 'unsubscribe':
        if (!isCalendar && !isPeople) insights.current?.unsubscribe()
        break
      case 'category':
        if (!isCalendar && !isPeople) insights.current?.category()
        break
      case 'compose':
        compose.controller.compose()
        break
      case 'reply':
        if (inMail && selectedId) compose.controller.reply(selectedId, 'sender')
        break
      case 'replyAll':
        if (inMail && selectedId) compose.controller.reply(selectedId, 'all')
        break
      case 'forward':
        if (inMail && selectedId) compose.controller.forward(selectedId, 'inline')
        break
      case 'toggleRead':
        if (inMail) mailbox.current?.toggleRead()
        break
      case 'toggleFlag':
        if (inMail) mailbox.current?.toggleFlag()
        break
      case 'move':
        if (inMail) mailbox.current?.move()
        break
      case 'trash':
        if (inMail) mailbox.current?.trash()
        break
    }
  })

  const sidebar = (
    <Sidebar
      folderId={folderId}
      label={label}
      category={category}
      isAllMail={isAllMail}
      isPeople={isPeople}
      isCalendar={isCalendar}
      isOutbox={isOutbox}
      onNavigate={layout === 'narrow' ? () => setSheetOpen(false) : undefined}
    />
  )

  return (
    <ComposeContext value={compose.controller}>
      <MailContext value={context}>
        <div className="flex h-full flex-col bg-bg text-fg">
          <header className="flex h-topbar shrink-0 items-center gap-2 border-b border-border px-2">
            {layout === 'narrow' && (
              <Button
                variant="ghost"
                size="icon"
                aria-label={strings.list.openMenu}
                onClick={() => setSheetOpen(true)}
              >
                <MenuIcon />
              </Button>
            )}
            <span className="px-2 text-base font-semibold">{strings.appName}</span>
            <div className="flex flex-1 justify-center px-2">
              <Button
                variant="secondary"
                className="w-full max-w-md justify-start border-border bg-bg-subtle text-fg-subtle"
                onClick={() => setPaletteOpen(true)}
              >
                <SearchIcon />
                <span className="truncate">{strings.command.open}</span>
                <Kbd className="ml-auto max-sm:hidden">{displayStroke('mod+k', isMac)[0]}</Kbd>
              </Button>
            </div>
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  aria-label={strings.shortcuts.open}
                  onClick={() => setShortcutsOpen(true)}
                  className="max-sm:hidden"
                >
                  <KeyboardIcon />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{strings.shortcuts.open}</TooltipContent>
            </Tooltip>
            <UserMenu />
          </header>
          <DeviceNotices />

          <div className="flex min-h-0 flex-1">
            {layout !== 'narrow' && (
              <nav
                aria-label={strings.landmarks.folders}
                className="w-pane-folders shrink-0 border-r border-border"
              >
                {sidebar}
              </nav>
            )}
            <main className="relative flex min-w-0 flex-1 overflow-hidden">
              {isCalendar || isOutbox ? (
                <section
                  aria-label={isOutbox ? strings.writing.outbox.title : strings.landmarks.calendar}
                  className="min-w-0 flex-1"
                >
                  <Outlet />
                </section>
              ) : (
                <>
                  <section
                    aria-label={isPeople ? strings.landmarks.people : strings.landmarks.messages}
                    inert={overlay && Boolean(selectedId)}
                    className={
                      overlay ? 'min-w-0 flex-1' : 'w-pane-list shrink-0 border-r border-border'
                    }
                  >
                    <Outlet />
                  </section>
                  {overlay ? (
                    <AnimatePresence initial={false}>
                      {selectedId && (
                        <m.section
                          key="reader"
                          aria-label={
                            isPeople ? strings.landmarks.person : strings.landmarks.reader
                          }
                          className="absolute inset-0 z-30 bg-bg"
                          initial={{ x: '100%' }}
                          animate={{ x: 0, transition: transition.expressive.moderate02 }}
                          exit={{ x: '100%', transition: transition.expressiveExit.moderate02 }}
                        >
                          {isPeople ? (
                            <Suspense>
                              <PersonPane personId={selectedId} onBack={back} />
                            </Suspense>
                          ) : (
                            <Reader messageId={selectedId} onBack={back} />
                          )}
                        </m.section>
                      )}
                    </AnimatePresence>
                  ) : (
                    <section
                      aria-label={isPeople ? strings.landmarks.person : strings.landmarks.reader}
                      className="min-w-0 flex-1"
                    >
                      {isPeople ? (
                        <Suspense>
                          {selectedId ? <PersonPane personId={selectedId} /> : <PeopleEmpty />}
                        </Suspense>
                      ) : selectedId ? (
                        <Reader messageId={selectedId} />
                      ) : (
                        <ReaderEmpty />
                      )}
                    </section>
                  )}
                </>
              )}
              {compose.element}
            </main>
          </div>
        </div>

        {layout === 'narrow' && (
          <Sheet open={sheetOpen} onOpenChange={setSheetOpen}>
            <SheetContent title={strings.landmarks.folders}>
              <nav aria-label={strings.landmarks.folders} className="h-full">
                {sidebar}
              </nav>
            </SheetContent>
          </Sheet>
        )}
        <CommandPalette
          open={paletteOpen}
          onOpenChange={setPaletteOpen}
          onShowShortcuts={() => setShortcutsOpen(true)}
        />
        <ShortcutsDialog open={shortcutsOpen} onOpenChange={setShortcutsOpen} />
      </MailContext>
    </ComposeContext>
  )
}
