import { infiniteQueryOptions, queryOptions } from '@tanstack/react-query'
import createClient from 'openapi-fetch'

import type { components, paths } from './schema.ts'

type Schemas = components['schemas']

export type Account = Schemas['AccountDto']
export type Address = Schemas['AddressDto']
export type Archive = Schemas['ArchiveDto']
export type Attachment = Schemas['AttachmentDto']
export type Folder = Schemas['FolderDto']
export type Label = Schemas['LabelDto']
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
export type JmapAccountRequest = Schemas['JmapAccountRequest']
export type AccountUpdateRequest = Schemas['AccountUpdateRequest']
export type Person = Schemas['PersonDto']
export type NewUserRequest = Schemas['NewUserRequest']
export type UserUpdateRequest = Schemas['UserUpdateRequest']
export type ProfileRequest = Schemas['ProfileRequest']
export type PasswordRequest = Schemas['PasswordRequest']
export type Role = 'owner' | 'admin' | 'member'
export type RemoteMode = Schemas['RemoteMode']
export type ArchiveUpdateRequest = Schemas['ArchiveUpdateRequest']
export type Task = Schemas['TaskDto']
export type TaskLine = Schemas['TaskLineDto']
export type ImageProxy = Schemas['ImageProxyDto']
export type RemoteImageJob = 'older' | 'failed'
export type ImapConnection = Schemas['ImapConnectionDto']
export type ConnectionRequest = Schemas['ConnectionRequest']
export type JmapConnection = Schemas['JmapConnectionDto']
export type JmapConnectionRequest = Schemas['JmapConnectionRequest']
export type JmapAuth = 'password' | 'token'
export type SyncSchedule = Schemas['SyncScheduleDto']
export type OAuthProvider = 'google' | 'microsoft'
export type OAuthKind = 'imap' | 'graph'
export type OAuthStartRequest = Omit<Schemas['OAuthStartRequest'], 'provider' | 'kind'> & {
  provider: OAuthProvider
  kind?: OAuthKind | null
}
export type OAuthStart = Schemas['OAuthStartDto']
export type OAuthProviderInfo = Schemas['OAuthProviderDto']
export type Ledger = Schemas['LedgerDto']
export type LedgerEntry = Schemas['LedgerEntryDto']
export type LedgerVerification = Schemas['LedgerVerificationDto']
export type VerifyLedgerRequest = Schemas['VerifyLedgerRequest']
export type DeletionRequest = Schemas['DeletionRequest']
export type DeletionReview = Schemas['DeletionReviewDto']
export type Webhook = Schemas['WebhookDto']
export type WebhookDelivery = Schemas['DeliveryDto']
export type WebhookList = Schemas['WebhookListDto']
export type NewWebhookRequest = Schemas['NewWebhookRequest']
export type WebhookUpdateRequest = Schemas['WebhookUpdateRequest']
export type SmtpStatus = Schemas['SmtpDto']
export type Invitation = Schemas['InvitationDto']
export type EventTime = Schemas['EventTimeDto']
export type Participant = Schemas['ParticipantDto']
export type ItemSummary = Schemas['ItemSummaryDto']
export type ItemDetail = Schemas['ItemDetail']
export type ItemVersion = Schemas['ItemVersionDto']
export type ItemChange = Schemas['ChangeDto']
export type SourceMessage = Schemas['SourceMessageDto']
export type ContactsCalendars = Schemas['ContactsCalendarsDto']
export type Watch = Schemas['WatchDto']
export type WatchState = Schemas['WatchStateDto']
export type WatchMode = 'off' | 'inbox' | 'folders'
export type ContactsCalendarsRun = Schemas['ContactsCalendarsRunDto']
export type ContactsCalendarsRequest = Schemas['ContactsCalendarsRequest']
export type Collection = Schemas['CollectionDto']
export type PersonSummary = Schemas['PersonSummaryDto']
export type PeoplePage = Schemas['PersonPage']
export type PersonDetail = Schemas['PersonDetailDto']
export type PersonField = Schemas['PersonFieldDto']
export type PersonFieldValue = Schemas['FieldValueDto']
export type ValueSource = Schemas['ValueSourceDto']
export type PersonCard = Schemas['PersonCardDto']
export type PersonLink = Schemas['PersonLinkDto']
export type Correspondence = Schemas['CorrespondenceDto']
export type PersonThread = Schemas['PersonThreadDto']
export type PersonAttachment = Schemas['PersonAttachmentDto']
export type Meeting = Schemas['MeetingDto']
export type AddressPerson = Schemas['AddressPersonDto']
export type CalendarRange = Schemas['CalendarRangeDto']
export type CalendarOccurrence = Schemas['CalendarOccurrenceDto']
export type CalendarSource = Schemas['CalendarSourceDto']
export type EventDetail = Schemas['EventDetailDto']
export type EventMoment = Schemas['EventMomentDto']
export type EventCopy = Schemas['EventCopyDto']
export type EventPerson = Schemas['EventPersonDto']
export type EventMessage = Schemas['EventMessageDto']
export type EventVersion = Schemas['EventVersionDto']
export type CopyDifference = Schemas['CopyDifferenceDto']
export type CopyAnswer = Schemas['CopyAnswerDto']
export type Recurrence = Schemas['RecurrenceDto']
export type RuleDay = Schemas['RuleDayDto']
export type ChangedInstance = Schemas['ChangedInstanceDto']
export type ItemHit = Schemas['ItemHitDto']
export type ItemSearchResponse = Schemas['ItemSearchResponse']
export type PersonOverride = Schemas['PersonOverrideDto']
export type AliasProvider = Schemas['AliasProviderDto']
export type AliasCapabilities = Schemas['CapabilitiesDto']
export type AliasService = Schemas['AliasServiceDto']
export type Alias = Schemas['AliasDto']
export type AliasAccount = Schemas['AliasAccountDto']
export type AliasList = Schemas['AliasListDto']
export type MessageAlias = Schemas['MessageAliasDto']
export type AliasOptions = Schemas['AliasOptionsDto']
export type NewAliasServiceRequest = Schemas['NewAliasServiceRequest']
export type AliasServiceUpdateRequest = Schemas['AliasServiceUpdateRequest']
export type ManualAliasRequest = Schemas['ManualAliasRequest']
export type CreateAliasRequest = Schemas['CreateAliasRequest']
export type AliasUpdateRequest = Schemas['AliasUpdateRequest']
export type MessageInsights = Schemas['MessageInsightsDto']
export type InsightStatus = Schemas['InsightStatusDto']
export type Insight = Schemas['InsightDto']
export type CodeInsight = Extract<Insight, { type: 'code' }>
export type UnsubscribeInsight = Extract<Insight, { type: 'unsubscribe' }>
export type InsightConfidence = Schemas['ConfidenceDto']
export type Trust = Schemas['TrustDto']
export type TrustState = Schemas['TrustStateDto']
export type Unsubscribe = Schemas['UnsubscribeDto']
export type OneClick = Schemas['OneClickDto']
export type OneClickReason = Schemas['OneClickReasonDto']
export type InsightsStatus = Schemas['InsightsStatusDto']

/** What a calendar request shows: a span in Unix milliseconds, the calendars to show (all when
 * absent, none when empty) and whether removed events are shown. */
export interface CalendarFilters {
  from: number
  to: number
  collections?: readonly string[]
  showRemoved?: boolean
}

/** The query string of a calendar request: calendars joined by commas, defaults left out. */
export function calendarParams(filters: CalendarFilters): {
  from: number
  to: number
  collections?: string
  show_removed?: boolean
} {
  return {
    from: Math.floor(filters.from),
    to: Math.ceil(filters.to),
    ...(filters.collections ? { collections: filters.collections.toSorted().join(',') } : {}),
    ...(filters.showRemoved ? { show_removed: true } : {}),
  }
}

/** Which people a list shows: `source` is `contacts`, `mail` or an address book's id. */
export interface PeopleFilters {
  q?: string
  source?: string
  differences?: boolean
}

/** The query string of a people list: empty filters are left out. */
export function peopleParams(filters: PeopleFilters): {
  q?: string
  source?: string
  has_differences?: boolean
} {
  const q = filters.q?.trim()
  const source = filters.source?.trim()
  return {
    ...(q ? { q } : {}),
    ...(source ? { source } : {}),
    ...(filters.differences ? { has_differences: true } : {}),
  }
}

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

export const updateArchive = async (id: string, request: ArchiveUpdateRequest) =>
  unwrap(await client.PATCH('/api/archives/{id}', { ...archivePath(id), body: request }))

export const deleteArchive = async (id: string) =>
  done(await client.DELETE('/api/archives/{id}', { ...archivePath(id), headers: json }))

export const addImapAccount = async (archiveId: string, request: ImapAccountRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/accounts', { ...archivePath(archiveId), body: request }),
  )

export const addJmapAccount = async (archiveId: string, request: JmapAccountRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/accounts/jmap', {
      ...archivePath(archiveId),
      body: request,
    }),
  )

export const updateAccount = async (id: string, request: AccountUpdateRequest) =>
  unwrap(await client.PATCH('/api/accounts/{id}', { ...archivePath(id), body: request }))

export const deleteAccount = async (id: string) =>
  done(await client.DELETE('/api/accounts/{id}', { ...archivePath(id), headers: json }))

export const syncAccount = async (id: string) =>
  unwrap(await client.POST('/api/accounts/{id}/sync', { ...archivePath(id), headers: json }))

export const startOAuth = async (request: OAuthStartRequest) =>
  unwrap(await client.POST('/api/oauth/start', { body: request }))

export const finishOAuth = async (pastedUrl: string) =>
  unwrap(await client.POST('/api/oauth/finish', { body: { pasted_url: pastedUrl } }))

export const startRemoteImages = async (archiveId: string, job: RemoteImageJob) =>
  unwrap(
    await client.POST('/api/archives/{id}/remote-images', {
      ...archivePath(archiveId),
      body: { job },
    }),
  )

export const cancelTask = async (id: string) =>
  done(await client.POST('/api/tasks/{id}/cancel', { ...archivePath(id), headers: json }))

export const setSyncSchedule = async (minutes: number) =>
  unwrap(await client.PUT('/api/sync-schedule', { body: { minutes } }))

export const setImageProxy = async (proxy: string | null) =>
  unwrap(await client.PUT('/api/image-proxy', { body: { proxy } }))

export const verifyLedger = async (request: VerifyLedgerRequest) =>
  unwrap(await client.POST('/api/ledger/verify', { body: request }))

export const reviewDeletion = async (archiveId: string, request: DeletionRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/deletions', { ...archivePath(archiveId), body: request }),
  )

export const confirmDeletion = async (reviewId: string, count: number) =>
  unwrap(
    await client.POST('/api/deletions/{id}/confirm', { ...archivePath(reviewId), body: { count } }),
  )

export const createWebhook = async (request: NewWebhookRequest) =>
  unwrap(await client.POST('/api/webhooks', { body: request }))

export const updateWebhook = async (id: string, request: WebhookUpdateRequest) =>
  unwrap(await client.PATCH('/api/webhooks/{id}', { ...archivePath(id), body: request }))

export const deleteWebhook = async (id: string) =>
  done(await client.DELETE('/api/webhooks/{id}', { ...archivePath(id) }))

export const rotateWebhookSecret = async (id: string) =>
  unwrap(await client.POST('/api/webhooks/{id}/secret', { ...archivePath(id), headers: json }))

export const testWebhook = async (id: string) =>
  done(await client.POST('/api/webhooks/{id}/test', { ...archivePath(id), headers: json }))

const aliasPath = (id: string, address: string) => ({ params: { path: { id, address } } })

export const addAliasService = async (archiveId: string, request: NewAliasServiceRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/alias-services', {
      ...archivePath(archiveId),
      body: request,
    }),
  )

export const updateAliasService = async (id: string, request: AliasServiceUpdateRequest) =>
  unwrap(await client.PATCH('/api/alias-services/{id}', { ...archivePath(id), body: request }))

export const removeAliasService = async (id: string, keepAliases: boolean) =>
  done(
    await client.DELETE('/api/alias-services/{id}', {
      params: { path: { id }, query: { keep_aliases: keepAliases } },
    }),
  )

export const syncAliasService = async (id: string) =>
  unwrap(await client.POST('/api/alias-services/{id}/sync', { ...archivePath(id), headers: json }))

export const createAlias = async (serviceId: string, request: CreateAliasRequest) =>
  unwrap(
    await client.POST('/api/alias-services/{id}/aliases', {
      ...archivePath(serviceId),
      body: request,
    }),
  )

export const addManualAlias = async (archiveId: string, request: ManualAliasRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/aliases', { ...archivePath(archiveId), body: request }),
  )

export const updateAlias = async (
  archiveId: string,
  address: string,
  request: AliasUpdateRequest,
) =>
  unwrap(
    await client.PATCH('/api/archives/{id}/aliases/{address}', {
      ...aliasPath(archiveId, address),
      body: request,
    }),
  )

export const removeAlias = async (archiveId: string, address: string, atService: boolean) =>
  done(
    await client.DELETE('/api/archives/{id}/aliases/{address}', {
      params: { path: { id: archiveId, address }, query: { at_service: atService } },
    }),
  )

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

export function bodyUrl(id: string, remote: RemoteMode = 'block', saved = true): string {
  const params = new URLSearchParams()
  if (remote !== 'block') params.set('remote', remote)
  if (!saved) params.set('saved', 'false')
  const query = params.toString()
  return `/api/messages/${encode(id)}/body${query ? `?${query}` : ''}`
}

export const rawUrl = (id: string): string => `/api/messages/${encode(id)}/raw`

export const pdfUrl = (id: string): string => `/api/messages/${encode(id)}/pdf`

export const itemVersionRawUrl = (id: string, version: number): string =>
  `/api/items/${encode(id)}/versions/${version}/raw`

export type ExportFormat = 'mbox' | 'zip' | 'pdf'

export interface ExportScope {
  folder?: string
  label?: string
  q?: string
}

export function exportUrl(
  archiveId: string,
  format: ExportFormat,
  scope: ExportScope = {},
): string {
  const params = new URLSearchParams({ format })
  if (scope.folder) params.set('folder', scope.folder)
  else if (scope.label) params.set('label', scope.label)
  else if (scope.q?.trim()) params.set('q', scope.q.trim())
  return `/api/archives/${encode(archiveId)}/export?${params.toString()}`
}

export type ContactsExportFormat = 'vcf' | 'zip'
export type CalendarExportFormat = 'ics' | 'zip'

/** Which contacts an export holds: an address book or a person's cards; removed contacts go
 * into a `.vcf` only when asked, a zip has them always. */
export interface ContactsExportScope {
  collection?: string
  person?: string
  removed?: boolean
}

/** Which calendar items an export holds: a calendar and a span in Unix milliseconds. */
export interface CalendarExportScope {
  collection?: string
  from?: number
  to?: number
  removed?: boolean
}

export function contactsExportUrl(
  archiveId: string,
  format: ContactsExportFormat,
  scope: ContactsExportScope = {},
): string {
  const params = new URLSearchParams({ format })
  if (scope.person) params.set('person', scope.person)
  else if (scope.collection) params.set('collection', scope.collection)
  if (scope.removed) params.set('removed', 'true')
  return `/api/archives/${encode(archiveId)}/contacts/export?${params.toString()}`
}

export function calendarExportUrl(
  archiveId: string,
  format: CalendarExportFormat,
  scope: CalendarExportScope = {},
): string {
  const params = new URLSearchParams({ format })
  if (scope.collection) params.set('collection', scope.collection)
  if (scope.from !== undefined && scope.to !== undefined && scope.from < scope.to) {
    params.set('from', String(Math.floor(scope.from)))
    params.set('to', String(Math.ceil(scope.to)))
  }
  if (scope.removed) params.set('removed', 'true')
  return `/api/archives/${encode(archiveId)}/calendar/export?${params.toString()}`
}

export interface MessageBody {
  html: string
  remoteBlocked: boolean
  savedShown: boolean
}

export async function fetchBody(
  id: string,
  remote: RemoteMode = 'block',
  saved = true,
  signal?: AbortSignal,
): Promise<MessageBody> {
  const response = await fetch(bodyUrl(id, remote, saved), { signal })
  if (!response.ok) throw await failure(response)
  return {
    html: await response.text(),
    remoteBlocked: response.headers.get('x-holtmail-remote-blocked') === '1',
    savedShown: response.headers.get('x-holtmail-remote-saved') === '1',
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

export const tasksQuery = queryOptions({
  queryKey: ['tasks'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/tasks', { signal })),
  refetchInterval: (query) =>
    query.state.data?.some((task) => task.state === 'running') ? 1_000 : 10_000,
})

export const syncScheduleQuery = queryOptions({
  queryKey: ['sync-schedule'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/sync-schedule', { signal })),
})

export const imageProxyQuery = queryOptions({
  queryKey: ['image-proxy'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/image-proxy', { signal })),
})

export const oauthProvidersQuery = queryOptions({
  queryKey: ['oauth-providers'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/oauth/providers', { signal })),
  staleTime: Infinity,
})

export const ledgerQuery = queryOptions({
  queryKey: ['ledger'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/ledger', { signal })),
})

export const webhooksQuery = queryOptions({
  queryKey: ['webhooks'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/webhooks', { signal })),
})

export const webhookDeliveriesQuery = (id: string) =>
  queryOptions({
    queryKey: ['webhooks', id, 'deliveries'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/webhooks/{id}/deliveries', { ...archivePath(id), signal })),
  })

export const aliasProvidersQuery = queryOptions({
  queryKey: ['alias-providers'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/alias-providers', { signal })),
  staleTime: Infinity,
})

export const aliasesQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'aliases'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/archives/{id}/aliases', { ...archivePath(archiveId), signal })),
  })

export const aliasOptionsQuery = (serviceId: string) =>
  queryOptions({
    queryKey: ['alias-services', serviceId, 'options'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/alias-services/{id}/options', { ...archivePath(serviceId), signal }),
      ),
    staleTime: 60_000,
  })

export const smtpQuery = queryOptions({
  queryKey: ['smtp'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/smtp', { signal })),
  refetchInterval: 30_000,
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

export const labelsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'labels'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/labels', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

export const insightsStatusQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'insights-status'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/insights/status', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

export const messagesInfiniteQuery = (archiveId: string, folderId?: string, label?: string) =>
  infiniteQueryOptions({
    queryKey: ['archives', archiveId, 'messages', folderId ?? null, label ?? null],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/messages', {
          params: {
            path: { id: archiveId },
            query: { folder: folderId, label, after: pageParam, limit: pageSize },
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

export const bodyQuery = (id: string, remote: RemoteMode, saved = true) =>
  queryOptions({
    queryKey: ['messages', id, 'body', remote, saved],
    queryFn: ({ signal }) => fetchBody(id, remote, saved, signal),
    staleTime: Infinity,
  })

export const rawQuery = (id: string) =>
  queryOptions({
    queryKey: ['messages', id, 'raw'],
    queryFn: ({ signal }) => fetchRaw(id, signal),
    staleTime: Infinity,
  })

export const itemQuery = (id: string) =>
  queryOptions({
    queryKey: ['items', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/items/{id}', { params: { path: { id } }, signal })),
  })

export const collectionsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'collections'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/collections', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

export const peopleInfiniteQuery = (archiveId: string, filters: PeopleFilters) => {
  const params = peopleParams(filters)
  return infiniteQueryOptions({
    queryKey: ['archives', archiveId, 'people', params],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/people', {
          params: {
            path: { id: archiveId },
            query: { ...params, cursor: pageParam, limit: pageSize },
          },
          signal,
        }),
      ),
    getNextPageParam: (page) => page.next ?? undefined,
  })
}

export const calendarQuery = (archiveId: string, filters: CalendarFilters) => {
  const params = calendarParams(filters)
  return queryOptions({
    queryKey: ['archives', archiveId, 'calendar', params],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/calendar', {
          params: { path: { id: archiveId }, query: params },
          signal,
        }),
      ),
  })
}

export const eventQuery = (id: string, tz?: string) =>
  queryOptions({
    queryKey: ['events', id, tz ?? null],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/events/{id}', {
          params: { path: { id }, query: tz ? { tz } : {} },
          signal,
        }),
      ),
  })

export const itemSearchQuery = (archiveId: string, q: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'search-items', q],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/search/items', {
          params: { path: { id: archiveId }, query: { q, limit: 20 } },
          signal,
        }),
      ),
    retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 3,
  })

export const peopleOverridesQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'people-overrides'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/people/overrides', {
          params: { path: { id: archiveId } },
          signal,
        }),
      ),
  })

/** Moves `cards` out of the person into a person of their own; `people` of the answer is the
 * person left with the other cards, then the new one. */
export const splitPerson = async (personId: string, cards: readonly string[]) =>
  unwrap(
    await client.POST('/api/people/{id}/split', {
      params: { path: { id: personId } },
      body: { cards: [...cards] },
    }),
  )

/** Makes the people one; `people` of the answer is the person they are now. */
export const joinPeople = async (people: readonly string[]) =>
  unwrap(await client.POST('/api/people/join', { body: { people: [...people] } }))

/** Undoes a split or join; `people` of the answer holds its cards and addresses now. */
export const undoPersonOverride = async (id: string) =>
  unwrap(
    await client.DELETE('/api/people/overrides/{id}', {
      params: { path: { id } },
      headers: json,
    }),
  )

export const personQuery = (id: string) =>
  queryOptions({
    queryKey: ['people', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/people/{id}', { params: { path: { id } }, signal })),
  })
