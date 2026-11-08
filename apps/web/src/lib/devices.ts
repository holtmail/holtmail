import { ApiError, type Device, type DeviceServer } from '@holtmail/api-client'
import type { Archive, CreateDeviceRequest, UpdateDeviceRequest } from '@holtmail/api-client'

export const deviceProtocols = ['imap', 'smtp', 'jmap', 'dav', 'api'] as const
export type DeviceProtocol = (typeof deviceProtocols)[number]

export const deviceScopes = ['read', 'write:mail', 'send:mail', 'write:pim', 'notify'] as const
export type DeviceScope = (typeof deviceScopes)[number]

export const isDeviceProtocol = (value: string): value is DeviceProtocol =>
  deviceProtocols.some((protocol) => protocol === value)

export const isDeviceScope = (value: string): value is DeviceScope =>
  deviceScopes.some((scope) => scope === value)

type ArchiveChoice = Pick<Archive, 'id'>

export interface DeviceForm {
  name: string
  protocols: string[]
  scopes: string[]
  mailArchive: string
  allArchives: boolean
  archives: string[]
  implicitSeen: boolean
}

export type DeviceFormProblem =
  | 'name'
  | 'protocol'
  | 'scope'
  | 'mailArchive'
  | 'archives'
  | 'smtpNeedsSend'
  | 'needsRead'

export const NOTICE_WINDOW_MS = 7 * 24 * 60 * 60 * 1000
export const deviceNoticesKey = 'holtmail.device-notices'

export const usesMailArchive = (protocols: readonly string[]): boolean =>
  protocols.includes('imap') || protocols.includes('smtp')

export const usesArchiveList = (protocols: readonly string[]): boolean =>
  protocols.some((protocol) => protocol === 'jmap' || protocol === 'dav' || protocol === 'api')

export const usesImplicitSeen = (protocols: readonly string[]): boolean =>
  protocols.includes('imap')

export const needsSignInAgain = (error: unknown): boolean =>
  error instanceof ApiError && error.status === 428

export function newDeviceForm(archives: readonly ArchiveChoice[] | undefined): DeviceForm {
  return {
    name: '',
    protocols: [],
    scopes: ['read'],
    mailArchive: archives?.[0]?.id ?? '',
    allArchives: true,
    archives: [],
    implicitSeen: false,
  }
}

/** Drops archives the user can no longer read; the server refuses a list that names one. */
function stillReadable(
  list: readonly string[],
  archives: readonly ArchiveChoice[] | undefined,
): string[] {
  return archives ? list.filter((id) => archives.some((archive) => archive.id === id)) : [...list]
}

export function deviceFormOf(
  device: Device,
  archives: readonly ArchiveChoice[] | undefined,
): DeviceForm {
  return {
    name: device.name,
    protocols: [...device.protocols],
    scopes: [...device.scopes],
    mailArchive: device.mail_archive ?? archives?.[0]?.id ?? '',
    allArchives: device.archives == null,
    archives: stillReadable(device.archives ?? [], archives),
    implicitSeen: device.implicit_seen,
  }
}

/** Sets one item of a list, keeping the others in order. */
export function withItem(list: readonly string[], item: string, on: boolean): string[] {
  const rest = list.filter((value) => value !== item)
  return on ? [...rest, item] : rest
}

/** Turns a protocol on or off; turning one on also adds the scope it cannot sign in without. */
export function withProtocol(form: DeviceForm, protocol: string, on: boolean): DeviceForm {
  const protocols = withItem(form.protocols, protocol, on)
  if (!on) return { ...form, protocols }
  const needed = protocol === 'smtp' ? 'send:mail' : 'read'
  return { ...form, protocols, scopes: withItem(form.scopes, needed, true) }
}

/** The first reason the form cannot be saved, or `null`. */
export function deviceFormProblem(form: DeviceForm): DeviceFormProblem | null {
  if (!form.name.trim()) return 'name'
  if (form.protocols.length === 0) return 'protocol'
  if (form.scopes.length === 0) return 'scope'
  if (usesMailArchive(form.protocols) && !form.mailArchive) return 'mailArchive'
  if (usesArchiveList(form.protocols) && !form.allArchives && form.archives.length === 0) {
    return 'archives'
  }
  if (form.protocols.includes('smtp') && !form.scopes.includes('send:mail')) {
    return 'smtpNeedsSend'
  }
  if (form.protocols.some((p) => p !== 'smtp') && !form.scopes.includes('read')) {
    return 'needsRead'
  }
  return null
}

const inOrder = (known: readonly string[], chosen: readonly string[]): string[] =>
  known.filter((item) => chosen.includes(item))

const sameSet = (a: readonly string[], b: readonly string[]): boolean =>
  a.length === b.length && a.every((item) => b.includes(item))

/** A device that serves no archive list gets an empty one, so it sees nothing beyond its mail archive. */
function archiveList(form: DeviceForm): string[] | null {
  if (!usesArchiveList(form.protocols)) return []
  return form.allArchives ? null : [...form.archives]
}

export function createDeviceRequest(form: DeviceForm): CreateDeviceRequest {
  return {
    name: form.name.trim(),
    protocols: inOrder(deviceProtocols, form.protocols),
    scopes: inOrder(deviceScopes, form.scopes),
    mail_archive: usesMailArchive(form.protocols) ? form.mailArchive : null,
    archives: archiveList(form),
    implicit_seen: usesImplicitSeen(form.protocols) && form.implicitSeen,
  }
}

/** Only what changed, so that a rename never counts as raising the device's rights. */
export function updateDeviceRequest(
  device: Device,
  form: DeviceForm,
  archives?: readonly ArchiveChoice[],
): UpdateDeviceRequest {
  const request: UpdateDeviceRequest = {}
  const name = form.name.trim()
  if (name !== device.name) request.name = name
  if (!sameSet(form.protocols, device.protocols)) {
    request.protocols = inOrder(deviceProtocols, form.protocols)
  }
  if (!sameSet(form.scopes, device.scopes)) {
    request.scopes = inOrder(deviceScopes, form.scopes)
  }
  if (usesMailArchive(form.protocols) && form.mailArchive !== device.mail_archive) {
    request.mail_archive = form.mailArchive
  }
  if (usesArchiveList(form.protocols)) {
    const list = form.allArchives ? null : [...form.archives]
    const same =
      list === null || device.archives == null
        ? list === (device.archives ?? null)
        : sameSet(list, stillReadable(device.archives, archives))
    if (!same) request.archives = list
  }
  if (usesImplicitSeen(form.protocols) && form.implicitSeen !== device.implicit_seen) {
    request.implicit_seen = form.implicitSeen
  }
  return request
}

export const hasChanges = (request: UpdateDeviceRequest): boolean => Object.keys(request).length > 0

export interface ServerRow extends DeviceServer {
  host: string
}

/** The listeners a device using `protocols` connects to, or every listener without `protocols`. */
export function serverRows(
  servers: readonly DeviceServer[] | undefined,
  fallbackHost: string,
  protocols?: readonly string[],
): ServerRow[] {
  return (servers ?? [])
    .filter((server) => !protocols || protocols.includes(server.protocol))
    .map(({ protocol, host, port, security }) => ({
      protocol,
      host: host ?? fallbackHost,
      port,
      security,
    }))
}

/** When a notice's device changed, with an explicit day to read after "on". */
export function formatNoticeDate(time: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium' }).format(new Date(time))
}

/** The day a device was last used; the server keeps days in UTC only. */
export function formatDeviceDay(day: number, locale?: string): string {
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeZone: 'UTC' }).format(
    new Date(day),
  )
}

export interface DeviceNotice {
  key: string
  name: string
  kind: 'made' | 'raised'
  at: number
}

function latestNotice(device: Device): DeviceNotice {
  const raised = device.raised_at ?? null
  const at = raised !== null && raised > device.created_at ? raised : device.created_at
  return {
    key: `${device.id}:${at}`,
    name: device.name,
    kind: at === raised ? 'raised' : 'made',
    at,
  }
}

/** Devices made or given more rights lately, newest first; revoked ones need no attention. */
export function recentDeviceNotices(
  devices: readonly Device[] | undefined,
  now: number,
): DeviceNotice[] {
  return (devices ?? [])
    .filter((device) => device.revoked_at == null)
    .map(latestNotice)
    .filter((notice) => now - notice.at < NOTICE_WINDOW_MS)
    .toSorted((a, b) => b.at - a.at)
}

export function parseDismissedNotices(raw: string | null): string[] {
  if (!raw) return []
  try {
    const value: unknown = JSON.parse(raw)
    return Array.isArray(value) ? value.filter((key) => typeof key === 'string') : []
  } catch {
    return []
  }
}
