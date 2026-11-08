import type {
  Account,
  Folder,
  MailChange,
  MailChangeOp,
  MessageDetail,
  MessageLocation,
} from '@holtmail/api-client'

import { isFlagged, isUnread } from './flags.ts'

export type MailboxCapability =
  | 'mail.flags'
  | 'mail.move'
  | 'mail.delete'
  | 'mail.expunge'
  | 'mail.append'

/** What the current user may change on each account's server now, from `Account.writable`. */
export type Writable = ReadonlyMap<string, ReadonlySet<string>>

export function writableByAccount(accounts: readonly Pick<Account, 'id' | 'writable'>[]): Writable {
  return new Map(accounts.map((account) => [account.id, new Set(account.writable)]))
}

const may = (writable: Writable, account: string, capability: MailboxCapability) =>
  writable.get(account)?.has(capability) === true

export interface Place {
  folder: Folder
  flags: string[]
}

/** The message's places on a server: locations not removed, in a folder the archive knows. */
export function livePlaces(
  locations: readonly MessageLocation[],
  folders: readonly Folder[],
): Place[] {
  const byId = new Map(folders.map((folder) => [folder.id, folder]))
  return locations.flatMap((location) => {
    const folder = byId.get(location.folder_id)
    return location.removed_at == null && folder ? [{ folder, flags: location.flags }] : []
  })
}

const sameFlag = (a: string, b: string) => a.toLowerCase() === b.toLowerCase()

/** `flags` with names added and removed, compared ignoring case as IMAP does. */
export function withFlags(
  flags: readonly string[],
  add: readonly string[],
  remove: readonly string[],
): string[] {
  const out = flags.filter(
    (flag) => !remove.some((name) => sameFlag(name, flag)) || add.some((n) => sameFlag(n, flag)),
  )
  for (const name of add) if (!out.some((flag) => sameFlag(flag, name))) out.push(name)
  return out
}

const isOpen = (change: Pick<MailChange, 'state'>) =>
  change.state === 'held' || change.state === 'sending'

/** The flags the reader shows: the union over the live places, with the open flags changes
 * applied in order, so a change shows at once while the server catches up. */
export function shownFlags(
  message: Pick<MessageDetail, 'locations' | 'pending'>,
  folders: readonly Folder[],
): string[] {
  const places = livePlaces(message.locations, folders)
  let flags: string[] = []
  for (const place of places) flags = withFlags(flags, place.flags, [])
  if (places.length === 0) {
    for (const location of message.locations) flags = withFlags(flags, location.flags, [])
  }
  for (const change of message.pending) {
    if (change.kind === 'flags' && isOpen(change)) {
      flags = withFlags(flags, change.add, change.remove)
    }
  }
  return flags
}

export interface MessageActions {
  flags: boolean
  move: boolean
  delete: boolean
  restore: boolean
}

/** Which mailbox actions the reader offers for a message: each only when an account it is on
 * lets the user make that change, and restore when any account of the archive lets them save
 * into a folder. */
export function messageActions(
  message: Pick<MessageDetail, 'locations'>,
  folders: readonly Folder[],
  writable: Writable,
): MessageActions {
  const places = livePlaces(message.locations, folders)
  const on = (capability: MailboxCapability) =>
    places.some((place) => may(writable, place.folder.account_id, capability))
  const trash = trashFolders(folders)
  return {
    flags: on('mail.flags'),
    move: moveTargets(message, folders, writable).length > 0,
    delete: places.some(
      (place) =>
        may(writable, place.folder.account_id, 'mail.delete') &&
        trash.get(place.folder.account_id) !== place.folder.id,
    ),
    restore: restoreTargets(folders, [], writable).length > 0,
  }
}

function trashFolders(folders: readonly Folder[]): Map<string, string> {
  const out = new Map<string, string>()
  for (const folder of folders) {
    if (folder.role === 'trash' && !out.has(folder.account_id))
      out.set(folder.account_id, folder.id)
  }
  return out
}

export interface PlannedChange {
  account: string
  op: MailChangeOp
}

export type ToggleKind = 'read' | 'flagged'

/** The flags change a toggle makes: mark read when the message shows unread, else unread; star
 * when it shows no star, else remove it. One request per account that may change flags and has
 * a place where the flag differs. */
export function planToggle(
  message: Pick<MessageDetail, 'locations' | 'pending'>,
  folders: readonly Folder[],
  writable: Writable,
  kind: ToggleKind,
): PlannedChange[] {
  const shown = shownFlags(message, folders)
  const name = kind === 'read' ? '\\Seen' : '\\Flagged'
  const set = kind === 'read' ? isUnread(shown) : !isFlagged(shown)
  return planFlags(message, folders, writable, name, set)
}

function placeFlags(place: Place, pending: readonly MailChange[]): string[] {
  let flags = place.flags
  for (const change of pending) {
    if (change.kind === 'flags' && isOpen(change) && change.folder_id === place.folder.id) {
      flags = withFlags(flags, change.add, change.remove)
    }
  }
  return flags
}

function planFlags(
  message: Pick<MessageDetail, 'locations' | 'pending'>,
  folders: readonly Folder[],
  writable: Writable,
  name: string,
  set: boolean,
): PlannedChange[] {
  const accounts = new Set<string>()
  for (const place of livePlaces(message.locations, folders)) {
    const account = place.folder.account_id
    const has = placeFlags(place, message.pending).some((flag) => sameFlag(flag, name))
    if (has !== set && may(writable, account, 'mail.flags')) accounts.add(account)
  }
  const op: MailChangeOp = set
    ? { kind: 'flags', add: [name], remove: [] }
    : { kind: 'flags', add: [], remove: [name] }
  return [...accounts].map((account) => ({ account, op }))
}

/** The change "mark as read when opened" makes, or none: only when the user turned it on, only
 * on accounts that may change flags, and only where the message is unread and no open change
 * already marks it read or unread. */
export function planReadOnOpen(
  message: Pick<MessageDetail, 'locations' | 'pending'>,
  folders: readonly Folder[],
  writable: Writable,
  enabled: boolean,
): PlannedChange[] {
  if (!enabled) return []
  const touched = message.pending.some(
    (change) =>
      change.kind === 'flags' &&
      isOpen(change) &&
      [...change.add, ...change.remove].some((flag) => sameFlag(flag, '\\Seen')),
  )
  if (touched) return []
  return planFlags(message, folders, writable, '\\Seen', true)
}

/** "Delete": one request per account that may delete and has a place outside its trash. The
 * server moves it to the trash, or only marks it deleted where there is none. */
export function planDelete(
  message: Pick<MessageDetail, 'locations'>,
  folders: readonly Folder[],
  writable: Writable,
): PlannedChange[] {
  const trash = trashFolders(folders)
  const accounts = new Set<string>()
  for (const place of livePlaces(message.locations, folders)) {
    const account = place.folder.account_id
    if (may(writable, account, 'mail.delete') && trash.get(account) !== place.folder.id) {
      accounts.add(account)
    }
  }
  return [...accounts].map((account) => ({ account, op: { kind: 'delete' } }))
}

export interface FolderGroup {
  accountId: string
  name: string
  folders: Folder[]
}

function groupByAccount(folders: readonly Folder[], accounts: readonly Account[]): FolderGroup[] {
  const groups = new Map<string, Folder[]>()
  for (const folder of folders) {
    const list = groups.get(folder.account_id) ?? []
    list.push(folder)
    groups.set(folder.account_id, list)
  }
  return [...groups].map(([accountId, list]) => ({
    accountId,
    name: accounts.find((account) => account.id === accountId)?.name ?? '',
    folders: list,
  }))
}

/** Folders a message can be moved to: those of the accounts it is on, where the user may move
 * (or, with only `mail.delete`, the trash), leaving out folders it is already in. */
export function moveTargets(
  message: Pick<MessageDetail, 'locations'>,
  folders: readonly Folder[],
  writable: Writable,
  accounts: readonly Account[] = [],
): FolderGroup[] {
  const places = livePlaces(message.locations, folders)
  const here = new Set(places.map((place) => place.folder.id))
  const onAccounts = new Set(places.map((place) => place.folder.account_id))
  const targets = folders.filter((folder) => {
    if (here.has(folder.id) || !onAccounts.has(folder.account_id)) return false
    if (may(writable, folder.account_id, 'mail.move')) return true
    return folder.role === 'trash' && may(writable, folder.account_id, 'mail.delete')
  })
  return groupByAccount(targets, accounts)
}

/** Folders an archived message can be restored into: every folder of an account of the
 * archive where the user may save into folders. */
export function restoreTargets(
  folders: readonly Folder[],
  accounts: readonly Account[],
  writable: Writable,
): FolderGroup[] {
  return groupByAccount(
    folders.filter((folder) => may(writable, folder.account_id, 'mail.append')),
    accounts,
  )
}

export type ChangeKind =
  | 'read'
  | 'unread'
  | 'starred'
  | 'unstarred'
  | 'flags'
  | 'labels'
  | 'move'
  | 'trash'
  | 'delete'
  | 'expunge'
  | 'restore'

export interface ChangeLine {
  id: string
  kind: ChangeKind
  /** The folder a move or restore goes to, as the archive names it. */
  folder?: string
  /** A short code for why it failed, for failed changes. */
  error?: string
}

const only = (list: readonly string[], name: string) =>
  list.length === 1 && sameFlag(list[0] ?? '', name)

function flagKind(change: Pick<MailChange, 'add' | 'remove'>): ChangeKind {
  if (only(change.add, '\\Seen') && change.remove.length === 0) return 'read'
  if (only(change.remove, '\\Seen') && change.add.length === 0) return 'unread'
  if (only(change.add, '\\Flagged') && change.remove.length === 0) return 'starred'
  if (only(change.remove, '\\Flagged') && change.add.length === 0) return 'unstarred'
  return 'flags'
}

/** What a held or failed change does, in words the reader shows. */
export function describeChange(change: MailChange, folders: readonly Folder[]): ChangeLine {
  const byId = new Map(folders.map((folder) => [folder.id, folder]))
  const line: ChangeLine = { id: change.id, kind: 'flags' }
  if (change.state === 'failed' && change.last_error) line.error = change.last_error
  switch (change.kind) {
    case 'flags':
      line.kind = flagKind(change)
      break
    case 'labels':
      line.kind = 'labels'
      break
    case 'move': {
      const target = change.target_folder_id ? byId.get(change.target_folder_id) : undefined
      line.kind = target?.role === 'trash' ? 'trash' : 'move'
      if (target) line.folder = target.name
      break
    }
    case 'delete':
      line.kind = change.expunge ? 'expunge' : 'delete'
      break
    case 'append': {
      line.kind = 'restore'
      const folder = byId.get(change.folder_id)
      if (folder) line.folder = folder.name
      break
    }
  }
  return line
}

/** Whether the user may try a failed change again: the account still allows its kind, as the
 * server checks on retry (a move to the trash also passes with `mail.delete`). */
export function mayRetry(
  change: Pick<MailChange, 'account_id' | 'kind' | 'expunge' | 'target_folder_id'>,
  folders: readonly Folder[],
  writable: Writable,
): boolean {
  const can = (capability: MailboxCapability) => may(writable, change.account_id, capability)
  switch (change.kind) {
    case 'flags':
    case 'labels':
      return can('mail.flags')
    case 'move': {
      const target = folders.find((folder) => folder.id === change.target_folder_id)
      return can('mail.move') || (target?.role === 'trash' && can('mail.delete'))
    }
    case 'delete':
      return change.expunge ? can('mail.expunge') : can('mail.delete')
    case 'append':
      return can('mail.append')
    default:
      return false
  }
}

/** Whether the user may dismiss a failed change: owners and admins, or anyone who may change
 * mail on its account (an editor grant). */
export function mayDismiss(
  change: Pick<MailChange, 'account_id'>,
  writable: Writable,
  managesInstance: boolean,
): boolean {
  return managesInstance || (writable.get(change.account_id)?.size ?? 0) > 0
}

/** The held mark: one line per open change, each kind and folder once, oldest first. */
export function heldLines(
  pending: readonly MailChange[],
  folders: readonly Folder[],
): ChangeLine[] {
  const seen = new Set<string>()
  const out: ChangeLine[] = []
  for (const change of pending) {
    if (!isOpen(change)) continue
    const line = describeChange(change, folders)
    const key = `${line.kind}:${line.folder ?? ''}`
    if (seen.has(key)) continue
    seen.add(key)
    out.push(line)
  }
  return out
}

export const MAX_RETENTION_DAYS = 36_500

/** A retention period typed by the user: whole days from 1 to 36 500, `null` for an empty field,
 * `undefined` for anything else. */
export function parseRetentionDays(text: string): number | null | undefined {
  const trimmed = text.trim()
  if (trimmed === '') return null
  if (!/^\d+$/.test(trimmed)) return undefined
  const days = Number(trimmed)
  return days >= 1 && days <= MAX_RETENTION_DAYS ? days : undefined
}
