import { archivesQuery, sessionQuery } from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Button, cn } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link, Outlet, useLocation } from '@tanstack/react-router'
import {
  ArchiveIcon,
  ArrowLeftIcon,
  BookOpenIcon,
  type LucideIcon,
  UserIcon,
  UsersIcon,
} from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import type { ReactNode } from 'react'

import { manages } from '../lib/roles.ts'
import { UserMenu } from '../mail/user-menu.tsx'
import { strings } from '../strings.ts'

const t = strings.settings

interface Section {
  to: '/settings/archives' | '/settings/people' | '/settings/reading' | '/settings/profile'
  label: string
  icon: LucideIcon
  managers: boolean
}

const sections: Section[] = [
  { to: '/settings/archives', label: t.archives, icon: ArchiveIcon, managers: true },
  { to: '/settings/people', label: t.people, icon: UsersIcon, managers: true },
  { to: '/settings/reading', label: t.reading, icon: BookOpenIcon, managers: false },
  { to: '/settings/profile', label: t.profile, icon: UserIcon, managers: false },
]

export function SettingsLayout() {
  const user = useQuery(sessionQuery).data
  const archives = useQuery(archivesQuery)
  const { pathname } = useLocation()
  const visible = sections.filter((section) => !section.managers || manages(user?.role))
  const first = archives.data?.[0]

  return (
    <div className="flex h-full flex-col bg-bg text-fg">
      <header className="flex h-topbar shrink-0 items-center gap-2 border-b border-border px-2">
        {first ? (
          <Button variant="ghost" asChild>
            <Link to="/a/$archiveId" params={{ archiveId: first.id }}>
              <ArrowLeftIcon />
              {t.back}
            </Link>
          </Button>
        ) : (
          <span className="px-2 text-base font-semibold">{strings.appName}</span>
        )}
        <span className="flex-1" />
        <UserMenu />
      </header>
      <div className="flex min-h-0 flex-1 max-md:flex-col">
        <nav
          aria-label={t.nav}
          className="w-pane-folders shrink-0 border-r border-border py-4 max-md:w-full max-md:border-r-0 max-md:border-b max-md:py-0"
        >
          <h1 className="px-4 pb-4 text-lg font-normal max-md:sr-only">{t.title}</h1>
          <ul className="max-md:flex max-md:overflow-x-auto">
            {visible.map(({ to, label, icon: Icon }) => {
              const current = pathname.startsWith(to)
              return (
                <li key={to}>
                  <Link
                    to={to}
                    aria-current={current ? 'page' : undefined}
                    className={cn(
                      'relative flex h-10 items-center gap-3 px-4 text-sm text-fg-muted outline-none whitespace-nowrap',
                      'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover hover:text-fg',
                      'focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus',
                      'aria-[current=page]:text-fg [&_svg]:size-4',
                    )}
                  >
                    {current && (
                      <m.span
                        layoutId="settings-current"
                        className="absolute inset-0 -z-0 bg-bg-selected"
                        transition={transition.expressive.moderate02}
                      />
                    )}
                    {current && (
                      <m.span
                        layoutId="settings-bar"
                        className="absolute inset-y-0 left-0 w-0.5 bg-accent max-md:inset-x-0 max-md:top-auto max-md:h-0.5 max-md:w-auto"
                        transition={transition.expressive.moderate02}
                      />
                    )}
                    <Icon className="relative" />
                    <span className="relative">{label}</span>
                  </Link>
                </li>
              )
            })}
          </ul>
        </nav>
        <main className="min-w-0 flex-1 overflow-y-auto">
          <AnimatePresence mode="wait" initial={false}>
            <m.div
              key={pathname}
              initial={{ opacity: 0, y: distance.sm }}
              animate={{ opacity: 1, y: 0, transition: transition.expressive.moderate02 }}
              exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
            >
              <Outlet />
            </m.div>
          </AnimatePresence>
        </main>
      </div>
    </div>
  )
}

export function SettingsPage({
  title,
  lead,
  action,
  children,
}: {
  title: string
  lead: string
  action?: ReactNode
  children: ReactNode
}) {
  return (
    <div className="mx-auto grid w-full max-w-3xl gap-10 px-6 py-10 max-sm:px-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="grid gap-1">
          <h2 className="text-2xl font-normal">{title}</h2>
          <p className="text-sm text-fg-muted">{lead}</p>
        </div>
        {action}
      </div>
      {children}
    </div>
  )
}
