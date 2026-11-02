import { isUnauthorized, sessionQuery, setupStatusQuery, type User } from '@holtmail/api-client'
import type { QueryClient } from '@tanstack/react-query'
import { redirect } from '@tanstack/react-router'

const authKeys = new Set([sessionQuery.queryKey[0], setupStatusQuery.queryKey[0]])

export const isAuthQuery = (key: readonly unknown[]): boolean =>
  typeof key[0] === 'string' && authKeys.has(key[0])

export async function currentUser(queryClient: QueryClient): Promise<User | undefined> {
  try {
    return await queryClient.ensureQueryData(sessionQuery)
  } catch (error) {
    if (isUnauthorized(error)) return undefined
    throw error
  }
}

export async function requireSession(queryClient: QueryClient, href: string): Promise<User> {
  const user = await currentUser(queryClient)
  if (user) return user
  const setup = await queryClient.fetchQuery(setupStatusQuery)
  if (setup.needed) throw redirect({ to: '/setup', replace: true })
  throw redirect({ to: '/login', search: { next: href === '/' ? undefined : href }, replace: true })
}

export function signedIn(queryClient: QueryClient, user: User): void {
  queryClient.clear()
  queryClient.setQueryData(sessionQuery.queryKey, user)
}
