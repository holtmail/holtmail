import { archivesQuery, setupStatusQuery } from '@holtmail/api-client'
import type { QueryClient } from '@tanstack/react-query'
import {
  createRootRouteWithContext,
  createRoute,
  createRouter,
  Outlet,
  redirect,
  useParams,
  useSearch,
} from '@tanstack/react-router'

import { LoginScreen } from './auth/login.tsx'
import { currentUser, requireSession } from './auth/session.ts'
import { SetupScreen } from './auth/setup.tsx'
import { parseLoginSearch, parseSetupSearch, safeNext } from './lib/auth.ts'
import { parseMailSearch, parseSearchRouteSearch } from './lib/search-params.ts'
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
    await requireSession(context.queryClient, location.href)
    const archives = await context.queryClient.ensureQueryData(archivesQuery)
    const first = archives[0]
    if (first) {
      throw redirect({ to: '/a/$archiveId', params: { archiveId: first.id }, replace: true })
    }
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
  component: LoginScreen,
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
  component: SetupScreen,
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

const searchRoute = createRoute({
  getParentRoute: () => archiveRoute,
  path: 'search',
  validateSearch: parseSearchRouteSearch,
  component: function SearchList() {
    const { q } = useSearch({ from: '/a/$archiveId/search' })
    return <MessageListPane source={{ kind: 'search', q: q.trim() }} />
  },
})

const routeTree = rootRoute.addChildren([
  loginRoute,
  setupRoute,
  indexRoute,
  archiveRoute.addChildren([allMailRoute, folderRoute, searchRoute]),
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
