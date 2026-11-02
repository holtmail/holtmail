import { infiniteQueryOptions, queryOptions } from '@tanstack/react-query'
import createClient from 'openapi-fetch'

import type { components, paths } from './schema.ts'

type Schemas = components['schemas']

export type Account = Schemas['AccountDto']
export type Address = Schemas['AddressDto']
export type Archive = Schemas['ArchiveDto']
export type Attachment = Schemas['AttachmentDto']
export type Folder = Schemas['FolderDto']
export type MessageLocation = Schemas['LocationDto']
export type MessageDetail = Schemas['MessageDetail']
export type MessageSummary = Schemas['MessageSummary']
export type MessagePage = Schemas['MessagePage']
export type SearchResponse = Schemas['SearchResponse']
export type Thread = Schemas['ThreadDto']
export type User = Schemas['UserDto']
export type LoginRequest = Schemas['LoginRequest']
export type SetupRequest = Schemas['SetupRequest']

export const client = createClient<paths>({ baseUrl: '' })

export class ApiError extends Error {
  readonly status: number
  readonly retryAfter: number | undefined

  constructor(status: number, message: string, retryAfter?: number) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.retryAfter = retryAfter
  }
}

export const isUnauthorized = (error: unknown): boolean =>
  error instanceof ApiError && error.status === 401

function readRetryAfter(response: Response): number | undefined {
  const value = response.headers.get('retry-after')
  if (value === null || value.trim() === '') return undefined
  const seconds = Number(value)
  return Number.isFinite(seconds) && seconds >= 0 ? seconds : undefined
}

function errorMessage(error: unknown, response: Response): string {
  if (typeof error === 'object' && error !== null && 'error' in error) {
    const message = error.error
    if (typeof message === 'string' && message) return message
  }
  return response.statusText || `HTTP ${response.status}`
}

function unwrap<T>(result: { data?: T; error?: unknown; response: Response }): T {
  if (result.data === undefined || !result.response.ok) {
    throw new ApiError(
      result.response.status,
      errorMessage(result.error, result.response),
      readRetryAfter(result.response),
    )
  }
  return result.data
}

async function failure(response: Response): Promise<ApiError> {
  let body: unknown
  try {
    body = await response.json()
  } catch {
    body = undefined
  }
  return new ApiError(response.status, errorMessage(body, response), readRetryAfter(response))
}

async function post(path: string, body: unknown): Promise<Response> {
  const response = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
    credentials: 'same-origin',
  })
  if (!response.ok) throw await failure(response)
  return response
}

function isUser(value: unknown): value is User {
  if (typeof value !== 'object' || value === null) return false
  const fields = new Map(Object.entries(value))
  return ['id', 'email', 'name', 'role'].every((key) => typeof fields.get(key) === 'string')
}

function sessionUser(body: unknown): User {
  const user = typeof body === 'object' && body !== null && 'user' in body ? body.user : undefined
  if (!isUser(user)) throw new ApiError(500, 'malformed session')
  return user
}

export async function login(request: LoginRequest): Promise<User> {
  return sessionUser(await (await post('/api/login', request)).json())
}

export async function setup(request: SetupRequest): Promise<User> {
  return sessionUser(await (await post('/api/setup', request)).json())
}

export async function logout(): Promise<void> {
  await post('/api/logout', {})
}

const encode = encodeURIComponent

export const bodyUrl = (id: string, remote = false): string =>
  `/api/messages/${encode(id)}/body${remote ? '?remote=true' : ''}`

export const rawUrl = (id: string): string => `/api/messages/${encode(id)}/raw`

export interface MessageBody {
  html: string
  remoteBlocked: boolean
}

export async function fetchBody(
  id: string,
  remote = false,
  signal?: AbortSignal,
): Promise<MessageBody> {
  const response = await fetch(bodyUrl(id, remote), { signal })
  if (!response.ok) throw await failure(response)
  return {
    html: await response.text(),
    remoteBlocked: response.headers.get('x-holtmail-remote-blocked') === '1',
  }
}

export async function fetchRaw(id: string, signal?: AbortSignal): Promise<string> {
  const response = await fetch(rawUrl(id), { signal })
  if (!response.ok) throw await failure(response)
  return response.text()
}

const pageSize = 100

export const sessionQuery = queryOptions({
  queryKey: ['session'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/session', { signal })).user,
  retry: false,
  staleTime: Infinity,
})

export const setupStatusQuery = queryOptions({
  queryKey: ['setup'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/setup', { signal })),
  retry: false,
})

export const archivesQuery = queryOptions({
  queryKey: ['archives'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/archives', { signal })),
})

export const accountsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'accounts'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/accounts', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

export const foldersQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'folders'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/folders', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

export const messagesInfiniteQuery = (archiveId: string, folderId?: string) =>
  infiniteQueryOptions({
    queryKey: ['archives', archiveId, 'messages', folderId ?? null],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/messages', {
          params: {
            path: { id: archiveId },
            query: { folder: folderId, after: pageParam, limit: pageSize },
          },
          signal,
        }),
      ),
    getNextPageParam: (page) => page.next ?? undefined,
  })

export const searchInfiniteQuery = (archiveId: string, q: string) =>
  infiniteQueryOptions({
    queryKey: ['archives', archiveId, 'search', q],
    initialPageParam: 0,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/search', {
          params: { path: { id: archiveId }, query: { q, offset: pageParam, limit: pageSize } },
          signal,
        }),
      ),
    getNextPageParam: (page) => page.next_offset ?? undefined,
    retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 3,
  })

export const messageQuery = (id: string) =>
  queryOptions({
    queryKey: ['messages', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/messages/{id}', { params: { path: { id } }, signal })),
  })

export const threadQuery = (id: string) =>
  queryOptions({
    queryKey: ['threads', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/threads/{id}', { params: { path: { id } }, signal })),
  })

export const bodyQuery = (id: string, remote: boolean) =>
  queryOptions({
    queryKey: ['messages', id, 'body', remote],
    queryFn: ({ signal }) => fetchBody(id, remote, signal),
    staleTime: Infinity,
  })

export const rawQuery = (id: string) =>
  queryOptions({
    queryKey: ['messages', id, 'raw'],
    queryFn: ({ signal }) => fetchRaw(id, signal),
    staleTime: Infinity,
  })
