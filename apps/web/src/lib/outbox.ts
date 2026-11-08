import type { Outbox, OutboxState } from '@holtmail/api-client'

const outboxStates: readonly string[] = [
  'draft',
  'queued',
  'sending',
  'retrying',
  'sent',
  'failed',
  'uncertain',
  'bounced',
  'cancelled',
] satisfies OutboxState[]

export const isOutboxState = (state: string): state is OutboxState => outboxStates.includes(state)

/** A message counts as scheduled when it was queued to leave more than this after it was
 * queued; shorter waits are the undo window. */
export const SCHEDULE_THRESHOLD_MS = 60_000

export const isScheduled = (entry: Outbox): boolean =>
  entry.state === 'queued' && entry.send_at - entry.created_at > SCHEDULE_THRESHOLD_MS

/** Milliseconds left to undo: until the message's `send_at`, while it is still queued. */
export function undoRemaining(entry: Outbox, now: number): number {
  if (entry.state !== 'queued') return 0
  return Math.max(0, entry.send_at - now)
}

export const canUndo = (entry: Outbox, now: number): boolean => undoRemaining(entry, now) > 0

/** Whole seconds the undo toast counts down, rounded up so it never shows 0 while undo works. */
export const undoSeconds = (entry: Outbox, now: number): number =>
  Math.ceil(undoRemaining(entry, now) / 1000)

export type OutboxGroup =
  | 'waiting'
  | 'scheduled'
  | 'failed'
  | 'uncertain'
  | 'bounced'
  | 'sent'
  | 'cancelled'

export const outboxGroups: OutboxGroup[] = [
  'waiting',
  'scheduled',
  'failed',
  'uncertain',
  'bounced',
  'sent',
  'cancelled',
]

export function outboxGroup(entry: Outbox): OutboxGroup {
  switch (entry.state) {
    case 'queued':
      return isScheduled(entry) ? 'scheduled' : 'waiting'
    case 'failed':
      return 'failed'
    case 'uncertain':
      return 'uncertain'
    case 'bounced':
      return 'bounced'
    case 'sent':
      return 'sent'
    case 'cancelled':
    case 'draft':
      return 'cancelled'
    default:
      return 'waiting'
  }
}

export const refusedRecipients = (entry: Outbox): string[] =>
  entry.recipients.filter((r) => r.state === 'refused').map((r) => r.address)

export interface OutboxActions {
  cancel: boolean
  retry: boolean
  resend: boolean
  resendRefused: boolean
  /** Retrying may deliver a second copy, so it asks first. */
  confirmRetry: boolean
  /** Sending again to everyone delivers a second copy to whoever accepted, so it asks first. */
  confirmResend: boolean
}

/** What the outbox offers for a message, matching the states the server accepts each in.
 * Retrying and sending again need the right to compose in the archive; a manager without it
 * may still cancel. */
export function outboxActions(entry: Outbox, { compose }: { compose: boolean }): OutboxActions {
  const state = entry.state
  const ended = ['sent', 'failed', 'uncertain', 'bounced', 'cancelled'].includes(state)
  const refused = refusedRecipients(entry).length > 0
  const accepted = entry.recipients.some((r) => r.state === 'accepted')
  return {
    cancel: ['draft', 'queued', 'retrying'].includes(state),
    retry: compose && ['retrying', 'failed', 'uncertain'].includes(state),
    resend: compose && ended && state !== 'uncertain',
    resendRefused: compose && ended && refused && accepted,
    confirmRetry: state === 'uncertain',
    confirmResend: accepted,
  }
}

/** Who a resend goes to: everyone in the original envelope, or only those who refused. */
export const resendRecipients = (entry: Outbox, kind: 'resend' | 'resendRefused'): string[] =>
  kind === 'resendRefused' ? refusedRecipients(entry) : entry.recipients.map((r) => r.address)

/** Groups messages in display order, newest first inside each group, empty groups left out. */
export function groupOutbox(
  entries: readonly Outbox[],
): { group: OutboxGroup; entries: Outbox[] }[] {
  return outboxGroups
    .map((group) => ({
      group,
      entries: entries
        .filter((e) => outboxGroup(e) === group)
        .toSorted((a, b) => b.created_at - a.created_at),
    }))
    .filter((g) => g.entries.length > 0)
}
