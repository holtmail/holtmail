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
export type Device = Schemas['DeviceDto']
export type CreatedDevice = Schemas['CreatedDeviceDto']
export type DeviceSetup = Schemas['DeviceSetupDto']
export type DeviceServer = Schemas['DeviceServerDto']
export type CreateDeviceRequest = Schemas['CreateDeviceRequest']
export type UpdateDeviceRequest = Schemas['UpdateDeviceRequest']
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
export type ActionInsight = Extract<Insight, { type: 'action' }>
export type ParcelInsight = Extract<Insight, { type: 'parcel' }>
export type ReservationInsight = Extract<Insight, { type: 'reservation' }>
export type OrderInsight = Extract<Insight, { type: 'order' }>
export type InvoiceInsight = Extract<Insight, { type: 'invoice' }>
export type PassInsight = Extract<Insight, { type: 'pass' }>
export type Reservation = Schemas['ReservationDto']
export type ReservationKind = Reservation['kind']
export type FlightReservation = Extract<Reservation, { kind: 'flight' }>
export type LodgingReservation = Extract<Reservation, { kind: 'lodging' }>
export type TrainReservation = Extract<Reservation, { kind: 'train' }>
export type BusReservation = Extract<Reservation, { kind: 'bus' }>
export type EventReservation = Extract<Reservation, { kind: 'event' }>
export type FoodReservation = Extract<Reservation, { kind: 'food' }>
export type RentalCarReservation = Extract<Reservation, { kind: 'rental_car' }>
export type InsightTime = Schemas['TimeDto']
export type Money = Schemas['MoneyDto']
export type PassStyle = Schemas['PassStyleDto']
export type BoardingPass = Schemas['BoardingPassDto']
export type BoardingLeg = Schemas['BoardingLegDto']
export type PassDate = Schemas['PassDateDto']
export type LinkAction = Schemas['LinkActionDto']
export type InsightConfidence = Schemas['ConfidenceDto']
export type Trust = Schemas['TrustDto']
export type TrustState = Schemas['TrustStateDto']
export type Unsubscribe = Schemas['UnsubscribeDto']
export type OneClick = Schemas['OneClickDto']
export type OneClickReason = Schemas['OneClickReasonDto']
export type UnsubscribeRequest = Schemas['UnsubscribeRequestDto']
export type UnsubscribeOutcome = Schemas['UnsubscribeOutcomeDto']
export type UnsubscribeResult = Schemas['UnsubscribeResultDto']
export type UnsubscribeRefusal = Schemas['UnsubscribeRefusalDto']
export type InsightsStatus = Schemas['InsightsStatusDto']
export type InsightsData = Schemas['InsightsDataDto']
export type DataFile = Schemas['DataFileDto']
export type DataOrigin = Schemas['DataOriginDto']
export type DataUpdates = Schemas['DataUpdatesDto']
export type DataOutcome = Schemas['DataOutcomeDto']
export type MessageKind = 'personal' | 'list' | 'bulk' | 'notification'
export type Topic =
  | 'purchases'
  | 'travel'
  | 'finance'
  | 'health'
  | 'social'
  | 'jobs'
  | 'events'
  | 'government'
export type OverrideScope = 'address' | 'domain'
export type CategorySource = Schemas['CategorySourceDto']
export type CategoryKind = Schemas['CategoryKindDto']
export type CategoryTopic = Schemas['CategoryTopicDto']
export type HeldBack = Schemas['HeldBackDto']
export type Categories = Schemas['CategoriesDto']
export type CategoryCount = Schemas['CategoryCountDto']
export type CategoryOverride = Schemas['CategoryOverrideDto']
export type CategoryOverrideRequest = Omit<
  Schemas['CategoryOverrideRequest'],
  'scope' | 'kind' | 'topics'
> & {
  scope: OverrideScope
  kind?: MessageKind | null
  topics?: Topic[] | null
}

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
  /** The parsed error body, when the server sent one. */
  readonly body: unknown

  constructor(status: number, message: string, retryAfter?: number, body?: unknown) {
    super(message)
    this.name = 'ApiError'
    this.status = status
    this.retryAfter = retryAfter
    this.body = body
  }
}

/** The leaks of a send refused with `409` because recipients would see the real mailbox behind
 * an alias; `null` for any other error. */
export function refusedLeaks(error: unknown): Leak[] | null {
  if (!(error instanceof ApiError) || error.status !== 409) return null
  const body = error.body
  if (typeof body !== 'object' || body === null || !('leaks' in body)) return null
  const leaks: unknown = body.leaks
  return Array.isArray(leaks) && leaks.every(isLeak) ? leaks : null
}

function isLeak(value: unknown): value is Leak {
  if (typeof value !== 'object' || value === null) return false
  const fields = new Map(Object.entries(value))
  return ['recipient', 'field', 'reason', 'fix'].every((k) => typeof fields.get(k) === 'string')
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
      result.error,
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
  return new ApiError(response.status, errorMessage(body, response), readRetryAfter(response), body)
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

/** Replaces the account's trusted authentication servers; owners and admins only. */
export const setTrustedAuthservIds = async (id: string, ids: readonly string[]) =>
  updateAccount(id, { trusted_authserv_ids: [...ids] })

/** Stores an override, replacing the one with the same scope and key; owners and admins only. */
export const putCategoryOverride = async (archiveId: string, request: CategoryOverrideRequest) =>
  unwrap(
    await client.PUT('/api/archives/{id}/category-overrides', {
      ...archivePath(archiveId),
      body: request,
    }),
  )

export const deleteCategoryOverride = async (archiveId: string, overrideId: string) =>
  done(
    await client.DELETE('/api/archives/{id}/category-overrides/{override}', {
      params: { path: { id: archiveId, override: overrideId } },
      headers: json,
    }),
  )

/**
 * Sends the one-click unsubscribe request stored with the message; owners and admins only. A 400
 * means one-click is not available, a 429 carries `retryAfter` after a failed request.
 */
export const unsubscribe = async (messageId: string) =>
  unwrap(
    await client.POST('/api/messages/{id}/unsubscribe', {
      ...archivePath(messageId),
      body: { confirm: true },
    }),
  )

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

export const createDevice = async (request: CreateDeviceRequest) =>
  unwrap(await client.POST('/api/devices', { body: request }))

export const updateDevice = async (id: string, request: UpdateDeviceRequest) =>
  unwrap(await client.PATCH('/api/devices/{id}', { ...archivePath(id), body: request }))

export const revokeDevice = async (id: string) =>
  done(await client.DELETE('/api/devices/{id}', archivePath(id)))

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

export const devicesQuery = queryOptions({
  queryKey: ['devices'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/devices', { signal })),
})

export const deviceSetupQuery = queryOptions({
  queryKey: ['device-setup'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/devices/setup', { signal })),
  staleTime: Infinity,
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

/** The server's version and the category data files it uses; every signed-in user may read it. */
export const insightsDataQuery = () =>
  queryOptions({
    queryKey: ['insights-data'],
    queryFn: async ({ signal }) => unwrap(await client.GET('/api/insights/data', { signal })),
  })

/** The categories of an archive with counts; `health` only with `includeHealth`. */
export const categoriesQuery = (archiveId: string, includeHealth = false) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'categories', includeHealth],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/categories', {
          params: {
            path: { id: archiveId },
            query: includeHealth ? { include: 'health' } : {},
          },
          signal,
        }),
      ),
  })

export const categoryOverridesQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'category-overrides'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/category-overrides', {
          ...archivePath(archiveId),
          signal,
        }),
      ),
  })

export const messagesInfiniteQuery = (
  archiveId: string,
  folderId?: string,
  label?: string,
  category?: MessageKind | Topic,
) =>
  infiniteQueryOptions({
    queryKey: [
      'archives',
      archiveId,
      'messages',
      folderId ?? null,
      label ?? null,
      category ?? null,
    ],
    initialPageParam: undefined as string | undefined,
    queryFn: async ({ pageParam, signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/messages', {
          params: {
            path: { id: archiveId },
            query: { folder: folderId, label, category, after: pageParam, limit: pageSize },
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

export type ArchiveRights = Schemas['ArchiveRightsDto']
export type GrantRole = 'reader' | 'editor' | 'sender'
export type Grant = Schemas['GrantDto']
export type Capability =
  | 'mail.flags'
  | 'mail.move'
  | 'mail.delete'
  | 'mail.expunge'
  | 'mail.append'
  | 'mail.send'
  | 'pim.edit'
  | 'pim.respond'
export type AccountCapabilities = Schemas['AccountCapabilitiesDto']
export type WriteSwitches = Schemas['WriteSwitches']
export type Transport = Schemas['TransportDto']
export type TransportLimits = Schemas['TransportLimitsDto']
export type TransportRequest = Schemas['TransportRequest']
export type TransportUpdateRequest = Schemas['TransportUpdateRequest']
export type TransportCheck = Schemas['TransportCheckDto']
export type TransportSecurity = 'tls' | 'starttls' | 'none'
export type Identity = Schemas['IdentityDto']
export type IdentityRequest = Schemas['IdentityRequest']
export type IdentityUpdateRequest = Schemas['IdentityUpdateRequest']
export type IdentitySuggestion = Schemas['IdentitySuggestionDto']
export type Mailbox = Schemas['MailboxDto']
export type Draft = Schemas['DraftDto']
export type DraftPayload = Schemas['DraftPayload']
export type DraftAttachment = Schemas['DraftAttachment']
export type DraftVersion = Schemas['DraftVersionDto']
export type Route = Schemas['RouteDto']
export type RouteReason = Schemas['RouteReasonDto']
export type RouteError = Schemas['RouteErrorDto']
export type Leak = Schemas['LeakDto']
export type AliasStrategies = Schemas['AliasStrategiesDto']
export type Outbox = Schemas['OutboxDto']
export type OutboxRecipient = Schemas['OutboxRecipientDto']
export type OutboxState =
  | 'draft'
  | 'queued'
  | 'sending'
  | 'retrying'
  | 'sent'
  | 'failed'
  | 'uncertain'
  | 'bounced'
  | 'cancelled'
export type Undo = Schemas['UndoDto']
export type ReplyKind = 'sender' | 'all' | 'list'
export type ForwardKind = 'inline' | 'attachment'

const idPath = (id: string) => ({ params: { path: { id } } })

/** A fresh `Idempotency-Key`: send the same key again only to repeat the same request. Works
 * outside secure contexts too (plain HTTP on a LAN), where `crypto.randomUUID` is missing. */
export const idempotencyKey = (): string => {
  if (typeof crypto.randomUUID === 'function') return crypto.randomUUID()
  const bytes = crypto.getRandomValues(new Uint8Array(16))
  return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

export const capabilitiesQuery = (accountId: string) =>
  queryOptions({
    queryKey: ['accounts', accountId, 'capabilities'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/accounts/{id}/capabilities', { ...idPath(accountId), signal })),
  })

/** Replaces the account's capabilities: every one listed is on, the rest off. */
export const setCapabilities = async (accountId: string, capabilities: readonly Capability[]) =>
  unwrap(
    await client.PUT('/api/accounts/{id}/capabilities', {
      ...idPath(accountId),
      body: { capabilities: [...capabilities] },
    }),
  )

export const putGrant = async (userId: string, archiveId: string, role: GrantRole) =>
  done(
    await client.PUT('/api/users/{id}/grants/{archive}', {
      params: { path: { id: userId, archive: archiveId } },
      body: { role },
    }),
  )

export const deleteGrant = async (userId: string, archiveId: string) =>
  done(
    await client.DELETE('/api/users/{id}/grants/{archive}', {
      params: { path: { id: userId, archive: archiveId } },
      headers: json,
    }),
  )

export const transportsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'transports'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/archives/{id}/transports', { ...idPath(archiveId), signal })),
  })

export const instanceTransportsQuery = queryOptions({
  queryKey: ['transports'],
  queryFn: async ({ signal }) => unwrap(await client.GET('/api/transports', { signal })),
})

export const createTransport = async (archiveId: string, request: TransportRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/transports', { ...idPath(archiveId), body: request }),
  )

export const createInstanceTransport = async (request: TransportRequest) =>
  unwrap(await client.POST('/api/transports', { body: request }))

export const updateTransport = async (id: string, request: TransportUpdateRequest) =>
  unwrap(await client.PATCH('/api/transports/{id}', { ...idPath(id), body: request }))

export const deleteTransport = async (id: string) =>
  done(await client.DELETE('/api/transports/{id}', { ...idPath(id), headers: json }))

export const setTransportGrants = async (id: string, archives: readonly string[]) =>
  done(
    await client.PUT('/api/transports/{id}/grants', {
      ...idPath(id),
      body: { archives: [...archives] },
    }),
  )

/** Signs in to the transport's server without sending anything. */
export const checkTransport = async (id: string) =>
  unwrap(await client.POST('/api/transports/{id}/check', { ...idPath(id), headers: json }))

export const identitiesQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'identities'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/archives/{id}/identities', { ...idPath(archiveId), signal })),
  })

export const identitySuggestionsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'identities', 'suggestions'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/identities/suggestions', {
          ...idPath(archiveId),
          signal,
        }),
      ),
  })

export const createIdentity = async (archiveId: string, request: IdentityRequest) =>
  unwrap(
    await client.POST('/api/archives/{id}/identities', { ...idPath(archiveId), body: request }),
  )

export const updateIdentity = async (id: string, request: IdentityUpdateRequest) =>
  unwrap(await client.PATCH('/api/identities/{id}', { ...idPath(id), body: request }))

export const deleteIdentity = async (id: string) =>
  done(await client.DELETE('/api/identities/{id}', { ...idPath(id), headers: json }))

export const setIdentityUsers = async (id: string, users: readonly string[]) =>
  done(
    await client.PUT('/api/identities/{id}/users', { ...idPath(id), body: { users: [...users] } }),
  )

export const setDefaultIdentity = async (archiveId: string, identity: string | null) =>
  done(
    await client.PUT('/api/archives/{id}/default-identity', {
      ...idPath(archiveId),
      body: { identity },
    }),
  )

export const draftsQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'drafts'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/archives/{id}/drafts', { ...idPath(archiveId), signal })),
  })

export const draftQuery = (id: string) =>
  queryOptions({
    queryKey: ['drafts', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/drafts/{id}', { ...idPath(id), signal })),
  })

export const createDraft = async (archiveId: string, payload: DraftPayload) =>
  unwrap(
    await client.POST('/api/archives/{id}/drafts', { ...idPath(archiveId), body: { payload } }),
  )

/** Saves a new version. `conflict` is true when another save came first; `draft` is then the
 * draft as the server has it, and nothing of `payload` was kept. */
function isDraft(value: unknown): value is Draft {
  if (typeof value !== 'object' || value === null) return false
  const fields = new Map(Object.entries(value))
  return (
    typeof fields.get('id') === 'string' &&
    typeof fields.get('version') === 'number' &&
    typeof fields.get('payload') === 'object'
  )
}

export async function saveDraft(
  id: string,
  version: number,
  payload: DraftPayload,
): Promise<{ draft: Draft; conflict: boolean }> {
  const result = await client.PUT('/api/drafts/{id}', { ...idPath(id), body: { version, payload } })
  const error: unknown = result.error
  if (result.response.status === 409 && isDraft(error)) return { draft: error, conflict: true }
  return { draft: unwrap(result), conflict: false }
}

export const deleteDraft = async (id: string) =>
  done(await client.DELETE('/api/drafts/{id}', { ...idPath(id), headers: json }))

/** Uploads a file into the draft; resolves with the draft at its new version. */
export async function uploadDraftAttachment(
  id: string,
  file: File,
  signal?: AbortSignal,
): Promise<Draft> {
  const params = new URLSearchParams({ name: file.name })
  if (file.type) params.set('content_type', file.type)
  const response = await fetch(`/api/drafts/${encode(id)}/attachments?${params}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/octet-stream' },
    body: file,
    credentials: 'same-origin',
    signal,
  })
  if (!response.ok) throw await failure(response)
  const body: unknown = await response.json()
  if (!isDraft(body)) throw new ApiError(500, 'malformed draft')
  return body
}

/** A new draft answering the message, with its recipients, quote and route filled in. */
export const replyToMessage = async (messageId: string, kind: ReplyKind, identity?: string) =>
  unwrap(
    await client.POST('/api/messages/{id}/reply', {
      ...idPath(messageId),
      body: { kind, identity: identity ?? null },
    }),
  )

/** A new draft forwarding the message inline or as an attached `.eml`. */
export const forwardMessage = async (messageId: string, kind: ForwardKind, identity?: string) =>
  unwrap(
    await client.POST('/api/messages/{id}/forward', {
      ...idPath(messageId),
      body: { kind, identity: identity ?? null },
    }),
  )

/** Queues the draft. The same `key` makes a repeated request answer with the first result. */
export const sendDraft = async (
  id: string,
  request: { version?: number; send_at?: number | null; allow_leaks?: boolean },
  key: string,
) =>
  unwrap(
    await client.POST('/api/drafts/{id}/send', {
      ...idPath(id),
      body: request,
      headers: { 'Idempotency-Key': key },
    }),
  )

/** Applies the `use_reverse_alias` fix of a leak: the server replaces `recipient` in the draft by
 * its reverse alias through the thread's alias and saves a new version. */
export const reverseAliasFix = async (id: string, version: number, recipient: string) =>
  unwrap(
    await client.POST('/api/drafts/{id}/reverse-alias', {
      ...idPath(id),
      body: { version, recipient },
    }),
  )

/** Reply strategies of an archive's aliases: overrides and each provider's default. */
export const aliasStrategiesQuery = (archiveId: string) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'alias-strategies'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/alias-strategies', {
          ...idPath(archiveId),
          signal,
        }),
      ),
  })

/** Sets the reply strategy override of an alias, a domain or a provider; `null` removes it. */
export const setAliasStrategy = async (
  archiveId: string,
  scope: 'alias' | 'domain' | 'service',
  key: string,
  strategy: 'reverse_alias' | 'direct' | 'reply_header' | null,
) =>
  unwrap(
    await client.PUT('/api/archives/{id}/alias-strategies', {
      ...idPath(archiveId),
      body: { scope, key, strategy },
    }),
  )

export const outboxQuery = (archiveId: string, states?: readonly OutboxState[]) =>
  queryOptions({
    queryKey: ['archives', archiveId, 'outbox', states?.join(',') ?? null],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/archives/{id}/outbox', {
          params: {
            path: { id: archiveId },
            query: states && states.length > 0 ? { state: states.join(',') } : {},
          },
          signal,
        }),
      ),
    refetchInterval: (query) =>
      query.state.data?.some((entry) => ['queued', 'sending', 'retrying'].includes(entry.state))
        ? 2_000
        : 30_000,
  })

export const outboxEntryQuery = (id: string) =>
  queryOptions({
    queryKey: ['outbox', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/outbox/{id}', { ...idPath(id), signal })),
  })

/** Cancels a message still waiting and gives it back as a new draft. */
export const undoOutbox = async (id: string) =>
  unwrap(await client.POST('/api/outbox/{id}/undo', { ...idPath(id), headers: json }))

export const cancelOutbox = async (id: string) =>
  unwrap(await client.POST('/api/outbox/{id}/cancel', { ...idPath(id), headers: json }))

export const retryOutbox = async (id: string, key: string) =>
  unwrap(
    await client.POST('/api/outbox/{id}/retry', {
      ...idPath(id),
      headers: { ...json, 'Idempotency-Key': key },
    }),
  )

/** Sends the bytes again to `recipients` of the original envelope, the refused ones when
 * missing. */
export const resendOutbox = async (id: string, key: string, recipients?: readonly string[]) =>
  unwrap(
    await client.POST('/api/outbox/{id}/resend', {
      ...idPath(id),
      body: { recipients: recipients ? [...recipients] : null },
      headers: { 'Idempotency-Key': key },
    }),
  )

export type MailChange = Schemas['MailChangeDto']
export type MailChangeOp = Schemas['MailChangeOp']
export type MailChanges = Schemas['MailChangesDto']
export type MailChangeSkip = Schemas['SkippedDto']
export type MailChangeState = 'held' | 'sending' | 'applied' | 'superseded_by_server' | 'failed'
export type Retention = Schemas['RetentionDto']

/** Records a change to one message on its servers, in every account it is in unless `account`
 * narrows it. The change is held and sent later; the answer lists what was recorded and what
 * needed nothing. */
export const changeMessage = async (id: string, op: MailChangeOp, account?: string) =>
  unwrap(
    await client.POST('/api/messages/{id}/changes', {
      ...idPath(id),
      body: { op, account: account ?? null },
    }),
  )

/** The same change to up to 500 messages; nothing is recorded when one account refuses. */
export const changeMessages = async (
  messages: readonly string[],
  op: MailChangeOp,
  account?: string,
) =>
  unwrap(
    await client.POST('/api/mail/changes', {
      body: { messages: [...messages], op, account: account ?? null },
    }),
  )

/** Puts the archived bytes of a message back into a folder on its server (`mail.append`). */
export const restoreMessage = async (id: string, folder: string) =>
  unwrap(await client.POST('/api/messages/{id}/restore', { ...idPath(id), body: { folder } }))

/** Asks again for a failed change: the failed one is dismissed and a new held one returned. */
export const retryMailChange = async (id: string) =>
  unwrap(await client.POST('/api/mail-changes/{id}/retry', { ...idPath(id), headers: json }))

/** Hides a failed change from the reader. */
export const dismissMailChange = async (id: string) =>
  unwrap(await client.POST('/api/mail-changes/{id}/dismiss', { ...idPath(id), headers: json }))

/** The account's retention; with `days`, a dry run counting for that period instead. */
export const retentionQuery = (accountId: string, days?: number) =>
  queryOptions({
    queryKey: ['accounts', accountId, 'retention', days ?? null],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/accounts/{id}/retention', {
          params: { path: { id: accountId }, query: days === undefined ? {} : { days } },
          signal,
        }),
      ),
  })

/** Sets the account's retention in days after arrival; `null` keeps everything on the server. */
export const setRetention = async (accountId: string, days: number | null) =>
  unwrap(await client.PUT('/api/accounts/{id}/retention', { ...idPath(accountId), body: { days } }))

export type EventForm = Schemas['EventFormDto']
export type ContactForm = Schemas['ContactFormDto']
export type AlarmForm = Schemas['AlarmFormDto']
export type PersonForm = Schemas['PersonFormDto']
export type Labeled = Schemas['LabeledDto']
export type NameParts = Schemas['NamePartsDto']
export type PostalAddress = Schemas['PostalAddressDto']
export type ItemContentRequest = Schemas['ItemContentRequest']
export type ServedState = Schemas['ServedStateDto']
export type ItemEdit = Schemas['ItemEditDto']
export type LocalEdit = Schemas['LocalEditDto']
export type PimChange = Schemas['PimChangeDto']
export type PimChangeState = 'held' | 'sending' | 'conflict' | 'rejected'
export type PimResolution = 'keep_mine' | 'take_theirs' | 'discard' | 'retry'
export type TimelineEntry = Schemas['TimelineEntryDto']
export type WriteModeState = Schemas['WriteModeDto']
export type WriteMode = 'read_only' | 'held' | 'write_through'
export type ChangeDiff = Schemas['ChangeDiffDto']
export type FieldDiff = Schemas['FieldDiffDto']

/** What is served for an item as text, the form read from it and the state to pass back as
 * `expect`. */
export const itemEditQuery = (id: string) =>
  queryOptions({
    queryKey: ['items', id, 'edit'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/items/{id}/edit', { ...idPath(id), signal })),
    staleTime: 0,
  })

/** Records a held create in the collection from a form, raw text or a message's sender. */
export const createItem = async (collectionId: string, request: ItemContentRequest) =>
  unwrap(
    await client.POST('/api/collections/{id}/items', { ...idPath(collectionId), body: request }),
  )

/** Records a held edit of the item; `409` when `expect` no longer matches what is served. */
export const editItem = async (id: string, request: ItemContentRequest) =>
  unwrap(await client.PUT('/api/items/{id}/content', { ...idPath(id), body: request }))

/** Records a held delete of the item; with `expect`, refused with `409` when something else is
 * served now. For an event the user organizes, `invitations: false` deletes it without mailing
 * attendees a cancellation. The archive keeps the item and its versions. */
export const deleteItem = async (
  id: string,
  options: { expect?: ServedState; invitations?: boolean } = {},
) =>
  unwrap(
    await client.DELETE('/api/items/{id}', {
      params: {
        path: { id },
        query: {
          version: options.expect?.version ?? undefined,
          change: options.expect?.change ?? undefined,
          invitations: options.invitations,
        },
      },
      headers: json,
    }),
  )

export type RsvpRequest = Schemas['RsvpRequest']
export type Rsvp = Schemas['RsvpDto']
export type RsvpAnswer = 'accepted' | 'tentative' | 'declined'

/** Answers the invitation of an event item (an account's copy or the event from mail). The
 * answer goes through the account's server when it schedules, else as an iMIP reply through the
 * outbox, which waits for the undo window. Send the same `key` again only to repeat it. */
export const rsvpItem = async (id: string, request: RsvpRequest, key = idempotencyKey()) =>
  unwrap(
    await client.POST('/api/items/{id}/rsvp', {
      ...idPath(id),
      body: request,
      headers: { 'Idempotency-Key': key },
    }),
  )

/** Answers the invitation a message carries, as `rsvpItem` does. */
export const rsvpMessage = async (id: string, request: RsvpRequest, key = idempotencyKey()) =>
  unwrap(
    await client.POST('/api/messages/{id}/rsvp', {
      ...idPath(id),
      body: request,
      headers: { 'Idempotency-Key': key },
    }),
  )

export type RsvpOptions = Schemas['RsvpOptionsDto']
export type RsvpOccurrence = Schemas['RsvpOccurrenceDto']
export type Organizing = Schemas['OrganizingDto']

/** What answering the invitation of an event item would do now; records and sends nothing. */
export const rsvpItemOptionsQuery = (id: string) =>
  queryOptions({
    queryKey: ['items', id, 'rsvp'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/items/{id}/rsvp', { ...idPath(id), signal })),
    retry: false,
  })

/** What answering the invitation a message carries would do now. */
export const rsvpMessageOptionsQuery = (id: string) =>
  queryOptions({
    queryKey: ['messages', id, 'rsvp'],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/messages/{id}/rsvp', { ...idPath(id), signal })),
    retry: false,
  })

/** Who would send the invitations of an event in the calendar: `itemId` for an existing one. */
export const organizingQuery = (collectionId: string, itemId?: string) =>
  queryOptions({
    queryKey: ['collections', collectionId, 'organizing', itemId ?? null],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/collections/{id}/organizing', {
          params: { path: { id: collectionId }, query: itemId ? { item: itemId } : {} },
          signal,
        }),
      ),
    retry: false,
  })

/** Edits the person's primary card, or makes one in the archive's first editable address book. */
export const editPersonCard = async (personId: string, request: ItemContentRequest) =>
  unwrap(await client.PUT('/api/people/{id}/card', { ...idPath(personId), body: request }))

/** Open changes of the archives the user may read, oldest first; narrowed by state, archive and
 * collection when given. */
export const pimChangesQuery = (
  filters: { states?: readonly PimChangeState[]; archive?: string; collection?: string } = {},
) => {
  const query = {
    ...(filters.states?.length ? { state: filters.states.toSorted().join(',') } : {}),
    ...(filters.archive ? { archive: filters.archive } : {}),
    ...(filters.collection ? { collection: filters.collection } : {}),
  }
  return queryOptions({
    queryKey: ['pim-changes', query],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/pim/changes', { params: { query }, signal })),
  })
}

/** The change next to its base and the latest upstream version, field by field, with its own
 * text. */
export const pimChangeDiffQuery = (id: string) =>
  queryOptions({
    queryKey: ['pim-changes', 'diff', id],
    queryFn: async ({ signal }) =>
      unwrap(await client.GET('/api/pim/changes/{id}/diff', { ...idPath(id), signal })),
  })

/** Decides an open change; `keep_mine` answers with the change that takes its place. */
export const resolvePimChange = async (id: string, action: PimResolution) =>
  unwrap(await client.POST('/api/pim/changes/{id}/resolve', { ...idPath(id), body: { action } }))

export const writeModeQuery = (collectionId: string) =>
  queryOptions({
    queryKey: ['collections', collectionId, 'write-mode'],
    queryFn: async ({ signal }) =>
      unwrap(
        await client.GET('/api/collections/{id}/write-mode', { ...idPath(collectionId), signal }),
      ),
  })

/** Sets the collection's write mode (owners and admins); `write_through` while changes are open
 * needs `confirm`, else `409`. */
export const setWriteMode = async (collectionId: string, mode: WriteMode, confirm = false) =>
  unwrap(
    await client.PUT('/api/collections/{id}/write-mode', {
      ...idPath(collectionId),
      body: { mode, confirm },
    }),
  )
