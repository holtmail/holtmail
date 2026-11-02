import './app.css'
import { ApiError, isUnauthorized, sessionQuery } from '@holtmail/api-client'
import { MotionProvider, TooltipProvider } from '@holtmail/ui'
import { QueryCache, QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from '@tanstack/react-router'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import { isAuthQuery } from './auth/session.ts'
import { createAppRouter } from './router.tsx'

const queryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error, query) => {
      if (isUnauthorized(error) && !isAuthQuery(query.queryKey)) sessionEnded()
    },
  }),
  defaultOptions: {
    queries: {
      staleTime: 15_000,
      refetchOnWindowFocus: false,
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 3,
    },
  },
})

const router = createAppRouter(queryClient)

function sessionEnded() {
  const { pathname, href } = router.state.location
  if (pathname === '/login' || pathname === '/setup') return
  queryClient.removeQueries({ queryKey: sessionQuery.queryKey })
  void router.navigate({ to: '/login', search: { next: href }, replace: true })
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <MotionProvider>
        <TooltipProvider>
          <RouterProvider router={router} />
        </TooltipProvider>
      </MotionProvider>
    </QueryClientProvider>
  </StrictMode>,
)
