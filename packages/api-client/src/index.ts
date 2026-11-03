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
export type AccountStatus = Schemas['AccountStatusDto']
export type ImapAccountRequest = Schemas['ImapAccountRequest']
export type AccountUpdateRequest = Schemas['AccountUpdateRequest']
export type Person = Schemas['PersonDto']
export type NewUserRequest = Schemas['NewUserRequest']
export type UserUpdateRequest = Schemas['UserUpdateRequest']
export type ProfileRequest = Schemas['ProfileRequest']
export type PasswordRequest = Schemas['PasswordRequest']
export type Role = 'owner' | 'admin' | 'member'

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

const json = { 'Content-Type': 'application/json' }

function done(result: { error?: unknown; response: Response }): void {
  if (!result.response.ok) {
    throw new ApiError(
      result.response.status,
      errorMessage(result.error, result.response),
      readRetryAfter(result.response),
    )
  }
}

const archivePath = (id: string) => ({ params: { path: { id } } })

export const createArchive = async (name: string) =>
  unwrap(await client.POST('/api/archives', { body: { name } }))

export const renameArchive = async (id: string, name: string) =>
  unwrap(await client.PATCH('/api/archives/{id}', { ...archivePath(id), body: { name } }))

export const deleteArchive = async (id: string) =>
  done(await client.DELETE('/api/archives/{id}', { ...archivePath(id), headers: json }))

export const addImapAccount = async (archiveId: string, request: ImapAccountRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/accounts', { ...archivePath(archiveId), body: request }),
  )

export const updateAccount = async (id: string, request: AccountUpdateRequest) =>
  unwrap(await client.PATCH('/api/accounts/{id}', { ...archivePath(id), body: request }))

export const deleteAccount = async (id: string) =>
  done(await client.DELETE('/api/accounts/{id}', { ...archivePath(id), headers: json }))

export const syncAccount = async (id: string) =>
  unwrap(await client.POST('/api/accounts/{id}/sync', { ...archivePath(id), headers: json }))

export const createUser = async (request: NewUserRequest) =>
  unwrap(await client.POST('/api/users', { body: request }))

export const updateUser = async (id: string, request: UserUpdateRequest) =>
  unwrap(await client.PATCH('/api/users/{id}', { ...archivePath(id), body: request }))

export const deleteUser = async (id: string) =>
  done(await client.DELETE('/api/users/{id}', { ...archivePath(id), headers: json }))

export const updateProfile = async (request: ProfileRequest): Promise<User> =>
  unwrap(await client.PATCH('/api/me', { body: request })).user

export const changePassword = async (request: PasswordRequest): Promise<User> =>
  unwrap(await client.POST('/api/me/password', { body: request })).user

function isAccount(value: unknown): value is Account {
  if (typeof value !== 'object' || value === null) return false
  const fields = new Map(Object.entries(value))
  const status = fields.get('status')
  return (
    ['id', 'name', 'kind'].every((key) => typeof fields.get(key) === 'string') &&
    typeof status === 'object' &&
    status !== null
  )
}

export interface UploadOptions {
  onProgress?: (loaded: number, total: number) => void
  signal?: AbortSignal
}

/** Sends a mailbox file to be imported; resolves with the import account once it is stored. */
export function uploadImport(
  archiveId: string,
  file: File,
  { onProgress, signal }: UploadOptions = {},
): Promise<Account> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/archives/${encode(archiveId)}/imports?name=${encode(file.name)}`)
    xhr.setRequestHeader('Content-Type', 'application/octet-stream')
    xhr.responseType = 'json'
    xhr.upload.addEventListener('progress', (event) =>
      onProgress?.(event.loaded, event.lengthComputable ? event.total : file.size),
    )
    xhr.addEventListener('load', () => {
      const body: unknown = xhr.response
      if (xhr.status >= 200 && xhr.status < 300 && isAccount(body)) {
        resolve(body)
        return
      }
      const message =
        typeof body === 'object' &&
        body !== null &&
        'error' in body &&
        typeof body.error === 'string'
          ? body.error
          : xhr.statusText || `HTTP ${xhr.status}`
      reject(new ApiError(xhr.status, message))
    })
    xhr.addEventListener('error', () => reject(new ApiError(0, 'the upload failed')))
    xhr.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')))
    signal?.addEventListener('abort', () => xhr.abort(), { once: true })
    xhr.send(file)
  })
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
    refetchInterval: (query) =>
      query.state.data?.some((account) => account.status.running) ? 2_000 : 60_000,
  })

export const usersQuery = queryOptions({
  queryKey: ['users'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/users', { signal })),
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
