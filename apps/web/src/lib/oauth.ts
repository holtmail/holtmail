import type { Account, OAuthProvider, OAuthStartRequest } from '@holtmail/api-client'

import { domainOf } from './providers.ts'

const domains: Record<OAuthProvider, readonly string[]> = {
  google: ['gmail.com', 'googlemail.com'],
  microsoft: ['outlook.com', 'hotmail.com', 'live.com', 'msn.com', 'office365.com'],
}

const hosts: Record<OAuthProvider, readonly string[]> = {
  google: ['imap.gmail.com', 'imap.googlemail.com'],
  microsoft: ['outlook.office365.com', 'imap-mail.outlook.com', 'outlook.live.com'],
}

const providerIds: readonly OAuthProvider[] = ['google', 'microsoft']

export const oauthProviderNames: Record<OAuthProvider, string> = {
  google: 'Google',
  microsoft: 'Microsoft',
}

export const isOAuthProvider = (value: unknown): value is OAuthProvider =>
  value === 'google' || value === 'microsoft'

export function oauthProviderFor(email: string): OAuthProvider | undefined {
  const domain = domainOf(email)
  if (!domain) return undefined
  return providerIds.find((id) => domains[id].includes(domain))
}

export function oauthProviderForHost(host: string): OAuthProvider | undefined {
  const name = host.trim().toLowerCase()
  return providerIds.find((id) => hosts[id].includes(name))
}

export function switchableProvider(account: Account): OAuthProvider | undefined {
  if (account.kind !== 'imap' || account.oauth_provider || !account.connection) return undefined
  return oauthProviderForHost(account.connection.host)
}

export function graphSignIn(
  archiveId: string,
  email: string,
  name: string,
): Omit<OAuthStartRequest, 'return_to'> | undefined {
  const address = email.trim()
  if (!/^[^\s@]+@[^\s@]+$/.test(address)) return undefined
  return {
    provider: 'microsoft',
    kind: 'graph',
    archive_id: archiveId,
    email: address,
    name: name.trim() || null,
  }
}

export function looksLikeSignInAnswer(text: string): boolean {
  const value = text.trim()
  const has = (name: string) => new RegExp(`(^|[?&#])${name}=`).test(value)
  return (has('code') && has('state')) || has('error')
}

export type OAuthReturn = { kind: 'done'; account: string } | { kind: 'error'; message: string }

const returnParams = ['oauth', 'oauth_error', 'account'] as const

export function parseOAuthReturn(search: string): OAuthReturn | undefined {
  const params = new URLSearchParams(search)
  const error = params.get('oauth_error')
  if (error !== null) return { kind: 'error', message: error.trim() || 'The sign-in failed.' }
  const account = params.get('account')?.trim()
  if (params.get('oauth') === 'done' && account) return { kind: 'done', account }
  return undefined
}

export function withoutOAuthParams(search: string): string {
  const params = new URLSearchParams(search)
  if (!params.has('oauth') && !params.has('oauth_error')) return search
  for (const name of returnParams) params.delete(name)
  const rest = params.toString()
  return rest ? `?${rest}` : ''
}
