import type { Account, ContactsCalendarsRequest } from '@holtmail/api-client'

import { type AccountTone, formatAgo } from './accounts.ts'
import { domainOf } from './providers.ts'

export interface DavPreset {
  name: string
  contacts: string
  calendars: string
}

const presets: readonly (DavPreset & { domains: readonly string[] })[] = [
  {
    name: 'Fastmail',
    domains: ['fastmail.com', 'fastmail.fm', 'messagingengine.com', 'sent.com'],
    contacts: 'https://carddav.fastmail.com',
    calendars: 'https://caldav.fastmail.com',
  },
  {
    name: 'iCloud',
    domains: ['icloud.com', 'me.com', 'mac.com'],
    contacts: 'https://contacts.icloud.com',
    calendars: 'https://caldav.icloud.com',
  },
  {
    name: 'Yandex',
    domains: ['yandex.ru', 'yandex.com', 'ya.ru'],
    contacts: 'https://carddav.yandex.ru',
    calendars: 'https://caldav.yandex.ru',
  },
]

export function davPreset(address: string): DavPreset | undefined {
  const domain = domainOf(address)
  const found = presets.find((preset) => preset.domains.includes(domain))
  return found && { name: found.name, contacts: found.contacts, calendars: found.calendars }
}

export type ContactsSupport = 'dav' | 'google' | 'graph' | 'jmap' | 'microsoft-imap' | 'none'

export function contactsSupport(account: Account): ContactsSupport {
  if (account.kind === 'import') return 'none'
  if (!account.contacts_calendars) {
    return account.oauth_provider === 'microsoft' ? 'microsoft-imap' : 'none'
  }
  if (account.kind === 'graph') return 'graph'
  if (account.kind === 'jmap') return 'jmap'
  return account.oauth_provider === 'google' ? 'google' : 'dav'
}

export function asksConsent(support: ContactsSupport): boolean {
  return support === 'google' || support === 'graph'
}

const clean = (value: string) => value.trim() || null

export function davRequest(
  enabled: boolean,
  contacts: string,
  calendars: string,
): ContactsCalendarsRequest {
  return { enabled, contacts_url: clean(contacts), calendars_url: clean(calendars) }
}

export interface ContactsState {
  tone: AccountTone
  label: string
}

export function contactsState(account: Account, now: number): ContactsState | undefined {
  const settings = account.contacts_calendars
  if (!settings?.enabled) return undefined
  if (settings.needs_consent) {
    return { tone: 'error', label: 'Contacts and calendars: sign in again to allow access' }
  }
  const run = account.status.contacts_calendars
  if (!run) return { tone: 'waiting', label: 'Contacts and calendars: wait for the next sync' }
  if (run.running) return { tone: 'running', label: 'Syncing contacts and calendars…' }
  if (run.error) return { tone: 'error', label: `Contacts and calendars: ${run.error}` }
  return {
    tone: 'done',
    label:
      run.finished_at == null
        ? 'Contacts and calendars archived'
        : `Contacts and calendars synced ${formatAgo(run.finished_at, now)}`,
  }
}
