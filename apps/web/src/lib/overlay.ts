import type { MailChangeOp } from '@holtmail/api-client'

import { withFlags } from './mailbox.ts'

/** How long a recorded change shows on list rows without the server confirming it: the time
 * after which the server settles changes no sync confirmed. */
export const OVERLAY_MS = 15 * 60 * 1000

/** Changes asked for in this tab that list rows show before the server confirms them. */
export interface OverlayEntry {
  add: string[]
  remove: string[]
  /** Folders a move or delete takes the message out of. */
  leaves: string[]
  at: number
}

/** `previous` with one more change: later flag changes win over earlier ones. */
export function mergeOverlay(
  previous: OverlayEntry | undefined,
  op: MailChangeOp,
  leaves: readonly string[],
  now: number,
): OverlayEntry {
  const base = previous ?? { add: [], remove: [], leaves: [], at: now }
  const next: OverlayEntry = {
    add: [...base.add],
    remove: [...base.remove],
    leaves: [...base.leaves],
    at: now,
  }
  if (op.kind === 'flags') {
    for (const name of op.add ?? []) {
      next.remove = next.remove.filter((flag) => flag.toLowerCase() !== name.toLowerCase())
      if (!next.add.some((flag) => flag.toLowerCase() === name.toLowerCase())) next.add.push(name)
    }
    for (const name of op.remove ?? []) {
      next.add = next.add.filter((flag) => flag.toLowerCase() !== name.toLowerCase())
      if (!next.remove.some((flag) => flag.toLowerCase() === name.toLowerCase())) {
        next.remove.push(name)
      }
    }
  }
  if (op.kind === 'move') next.leaves = next.leaves.filter((folder) => folder !== op.folder)
  for (const folder of leaves) if (!next.leaves.includes(folder)) next.leaves.push(folder)
  return next
}

/** The flags a list row shows with the overlay applied. */
export function applyOverlay(flags: readonly string[], entry: OverlayEntry | undefined): string[] {
  return entry ? withFlags(flags, entry.add, entry.remove) : [...flags]
}

/** Whether the row still differs from what the server shows: some flag change not yet visible,
 * or a move or delete out of the listed folder. */
export function overlayHeld(
  flags: readonly string[],
  entry: OverlayEntry | undefined,
  folderId?: string,
): boolean {
  if (!entry) return false
  const has = (name: string) => flags.some((flag) => flag.toLowerCase() === name.toLowerCase())
  if (entry.add.some((name) => !has(name)) || entry.remove.some(has)) return true
  return folderId !== undefined && entry.leaves.includes(folderId)
}

/** Whether the entry only changes flags, so it can go once the server has no open change of
 * the message: rows then show the server's flags. Moves wait for sync to drop the row. */
export function flagsOnly(entry: OverlayEntry | undefined): boolean {
  return entry !== undefined && entry.leaves.length === 0
}

/** A per-tab store of overlay entries by message id; entries expire after {@link OVERLAY_MS}. */
export class OverlayStore {
  private entries = new Map<string, OverlayEntry>()
  private listeners = new Set<() => void>()
  private version = 0

  record(message: string, op: MailChangeOp, leaves: readonly string[], now = Date.now()): void {
    this.entries.set(message, mergeOverlay(this.get(message, now), op, leaves, now))
    this.emit()
  }

  forget(message: string): void {
    if (this.entries.delete(message)) this.emit()
  }

  get(message: string, now = Date.now()): OverlayEntry | undefined {
    const entry = this.entries.get(message)
    if (entry && now - entry.at > OVERLAY_MS) {
      this.entries.delete(message)
      return undefined
    }
    return entry
  }

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  snapshot = (): number => this.version

  private emit() {
    this.version += 1
    for (const listener of this.listeners) listener()
  }
}

export const overlay = new OverlayStore()
