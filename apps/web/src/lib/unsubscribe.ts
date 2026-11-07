import {
  ApiError,
  type MessageInsights,
  type OneClickReason,
  type UnsubscribeRequest,
  type UnsubscribeResult,
} from '@holtmail/api-client'

import { strings } from '../strings.ts'
import { formatFullDate } from './format.ts'

type DateFormat = (time: number) => string

/** Whether the server offers one-click unsubscribe for the message. */
export function oneClickOffered(insights: MessageInsights | null | undefined): boolean {
  return insights?.unsubscribe?.one_click.available === true
}

/** The host of a link, lowercased, without port, user name or trailing dot; IDNs stay A-labels. */
export function linkHost(url: string | null | undefined): string | undefined {
  if (!url) return undefined
  try {
    const host = new URL(url).hostname.replace(/\.$/, '')
    return host ? host.toLowerCase() : undefined
  } catch {
    return undefined
  }
}

function addressDomain(address: string | undefined): string | undefined {
  const at = address?.lastIndexOf('@') ?? -1
  const domain = at === -1 ? '' : (address?.slice(at + 1).replace(/\.$/, '') ?? '')
  return domain ? domain.toLowerCase() : undefined
}

/** The domain the consent dialog names: the host of the stored https link, lowercased and without
 * port or user name, else the aligned domain, else the domain of `sender`. */
export function unsubscribeDomain(
  insights: MessageInsights | null | undefined,
  sender?: string,
): string | undefined {
  return (
    linkHost(insights?.unsubscribe?.https) ??
    insights?.trust.domain?.toLowerCase() ??
    addressDomain(sender)
  )
}

interface OutcomeNote {
  text: string
  tone: 'success' | 'warning'
  offerLink: boolean
}

/** The text shown after a one-click request, including the earlier one when nothing was sent. */
export function outcomeNote(
  result: UnsubscribeResult,
  domain: string,
  format: DateFormat = formatFullDate,
): OutcomeNote {
  const t = strings.unsubscribe
  const { outcome, status } = result.request
  const text = t.outcomes[outcome](domain, status ?? undefined)
  return {
    text: result.repeated ? `${t.repeated(format(result.request.requested_at))} ${text}` : text,
    tone: outcome === 'accepted' ? 'success' : 'warning',
    offerLink: outcome !== 'accepted',
  }
}

const isReason = (code: string): code is OneClickReason =>
  Object.hasOwn(strings.insights.reasons, code)

function refusalReason(message: string): OneClickReason | undefined {
  const code = message.split(':').at(-1)?.trim() ?? ''
  return isReason(code) ? code : undefined
}

/** The text for a failed one-click call: the refusal reason, the retry wait, or a generic error. */
export function unsubscribeErrorText(error: unknown): string {
  const t = strings.unsubscribe
  if (error instanceof ApiError) {
    if (error.status === 429) return t.retryLater
    if (error.status === 400) {
      const reason = refusalReason(error.message)
      if (reason) return strings.insights.reasons[reason]
    }
  }
  return t.failed
}

/** "Unsubscribe requested on <date>", with the outcome when the list did not accept it. */
export function requestedOnText(
  request: UnsubscribeRequest,
  format: DateFormat = formatFullDate,
): string {
  const t = strings.unsubscribe
  const date = t.requestedOn(format(request.requested_at))
  return request.outcome === 'accepted'
    ? date
    : `${date} · ${t.shortOutcomes[request.outcome](request.status ?? undefined)}`
}
