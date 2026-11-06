import { archivesQuery, sessionQuery, setupStatusQuery } from '@holtmail/api-client'
import type { QueryClient } from '@tanstack/react-query'
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  lazyRouteComponent,
  Outlet,
  redirect,
  useParams,
  useSearch,
} from '@tanstack/react-router'

import { currentUser, requireSession } from './auth/session.ts'
import { parseLoginSearch, parseSetupSearch, safeNext } from './lib/auth.ts'
import { manages } from './lib/roles.ts'
import {
  parseArchivesSettingsSearch,
  parseCalendarSearch,
  parseLabelRouteSearch,
  parseMailSearch,
  parsePeopleSearch,
  parseSearchRouteSearch,
} from './lib/search-params.ts'
import { MailLayout } from './mail/mail-layout.tsx'
import { MessageListPane } from './mail/message-list.tsx'
import { ArchivesFailed, NoArchives, NotFound, ServerFailed } from './pages.tsx'

interface RouterContext {
  queryClient: QueryClient
}

const rootRoute = createRootRouteWithContext<RouterContext>()({
  component: Outlet,
  notFoundComponent: NotFound,
})

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/',
  beforeLoad: async ({ context, location }) => {
    const user = await requireSession(context.queryClient, location.href)
    const archives = await context.queryClient.fetchQuery(archivesQuery)
    const first = archives[0]
    if (first) {
      throw redirect({ to: '/a/$archiveId', params: { archiveId: first.id }, replace: true })
    }
    if (manages(user.role)) throw redirect({ to: '/welcome', replace: true })
  },
  component: NoArchives,
  errorComponent: ArchivesFailed,
})

const archiveRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/a/$archiveId',
  validateSearch: parseMailSearch,
  beforeLoad: ({ context, location }) => requireSession(context.queryClient, location.href),
  component: MailLayout,
  errorComponent: ServerFailed,
})

const loginRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/login',
  validateSearch: parseLoginSearch,
  beforeLoad: async ({ context, search }) => {
    if (await currentUser(context.queryClient)) {
      throw redirect({ href: safeNext(search.next), replace: true })
    }
  },
  component: lazyRouteComponent(() => import('./auth/login.tsx'), 'LoginScreen'),
  errorComponent: ServerFailed,
})

const setupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/setup',
  validateSearch: parseSetupSearch,
  beforeLoad: async ({ context }) => {
    const status = await context.queryClient.fetchQuery(setupStatusQuery)
    if (!status.needed) throw redirect({ to: '/login', replace: true })
  },
  component: lazyRouteComponent(() => import('./auth/setup.tsx'), 'SetupScreen'),
  errorComponent: ServerFailed,
})

const welcomeRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/welcome',
  beforeLoad: async ({ context, location }) => {
    const user = await requireSession(context.queryClient, location.href)
    if (!manages(user.role)) throw redirect({ to: '/', replace: true })
  },
  component: lazyRouteComponent(() => import('./onboarding/welcome.tsx'), 'WelcomeScreen'),
  errorComponent: ServerFailed,
})

const settingsRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/settings',
  beforeLoad: ({ context, location }) => requireSession(context.queryClient, location.href),
  component: lazyRouteComponent(() => import('./settings/settings-layout.tsx'), 'SettingsLayout'),
  errorComponent: ServerFailed,
})

const settingsIndexRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: '/',
  beforeLoad: async ({ context }) => {
    const user = await context.queryClient.ensureQueryData(sessionQuery)
    throw redirect({
      to: manages(user.role) ? '/settings/archives' : '/settings/profile',
      replace: true,
    })
  },
})

const managersOnly = async ({ context }: { context: RouterContext }) => {
  const user = await context.queryClient.ensureQueryData(sessionQuery)
  if (!manages(user.role)) throw redirect({ to: '/settings/profile', replace: true })
}

const archivesSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'archives',
  validateSearch: parseArchivesSettingsSearch,
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/archives-page.tsx'), 'ArchivesPage'),
})

const peopleSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'people',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/people-page.tsx'), 'PeoplePage'),
})

const tasksSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'tasks',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/tasks-page.tsx'), 'TasksPage'),
})

const networkSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'network',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/network-page.tsx'), 'NetworkPage'),
})

const integritySettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'integrity',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/integrity-page.tsx'), 'IntegrityPage'),
})

const webhooksSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'webhooks',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/webhooks-page.tsx'), 'WebhooksPage'),
})

const smtpSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'smtp',
  beforeLoad: managersOnly,
  component: lazyRouteComponent(() => import('./settings/smtp-page.tsx'), 'SmtpPage'),
})

const profileSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'profile',
  component: lazyRouteComponent(() => import('./settings/profile-page.tsx'), 'ProfilePage'),
})

const readingSettingsRoute = createRoute({
  getParentRoute: () => settingsRoute,
  path: 'reading',
  component: lazyRouteComponent(() => import('./settings/reading-page.tsx'), 'ReadingPage'),
})

const itemRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/items/$itemId',
  beforeLoad: ({ context, location }) => requireSession(context.queryClient, location.href),
  component: lazyRouteComponent(() => import('./items/item-page.tsx'), 'ItemPage'),
  errorComponent: ServerFailed,
})

const eventRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: '/events/$groupId',
  beforeLoad: ({ context, location }) => requireSession(context.queryClient, location.href),
  component: lazyRouteComponent(() => import('./calendar/event-page.tsx'), 'EventPage'),
  errorComponent: ServerFailed,
})

const allMailRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: '/',
  component: () => <MessageListPane source={{ kind: 'all' }} />,
})

const folderRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'f/$folderId',
  component: function FolderList() {
    const { folderId } = useParams({ from: '/a/$archiveId/f/$folderId' })
    return <MessageListPane source={{ kind: 'folder', folderId }} />
  },
})

const labelRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'label',
  validateSearch: parseLabelRouteSearch,
  component: function LabelList() {
    const { name } = useSearch({ from: '/a/$archiveId/label' })
    return <MessageListPane source={{ kind: 'label', label: name }} />
  },
})

const searchRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'search',
  validateSearch: parseSearchRouteSearch,
  component: function SearchList() {
    const { q } = useSearch({ from: '/a/$archiveId/search' })
    return <MessageListPane source={{ kind: 'search', q: q.trim() }} />
  },
})

const peopleRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'people',
  validateSearch: parsePeopleSearch,
  component: lazyRouteComponent(() => import('./people/people-list.tsx'), 'PeopleListPane'),
})

const calendarRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'calendar',
  validateSearch: parseCalendarSearch,
  component: lazyRouteComponent(() => import('./calendar/calendar-pane.tsx'), 'CalendarPane'),
})

const routeTree = rootRoute.addChildren([
  loginRoute,
  setupRoute,
  indexRoute,
  welcomeRoute,
  itemRoute,
  eventRoute,
  settingsRoute.addChildren([
    settingsIndexRoute,
    archivesSettingsRoute,
    peopleSettingsRoute,
    tasksSettingsRoute,
    networkSettingsRoute,
    integritySettingsRoute,
    webhooksSettingsRoute,
    smtpSettingsRoute,
    readingSettingsRoute,
    profileSettingsRoute,
  ]),
  archiveRoute.addChildren([
    allMailRoute,
    folderRoute,
    labelRoute,
    searchRoute,
    peopleRoute,
    calendarRoute,
  ]),
])

export function createAppRouter(queryClient: QueryClient) {
  return createRouter({
    routeTree,
    context: { queryClient },
    defaultPreload: 'intent',
    scrollRestoration: false,
  })
}

declare module '@tanstack/react-router' {
  interface Register {
    router: ReturnType<typeof createAppRouter>
  }
}
