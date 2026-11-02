import { ApiError } from '@holtmail/api-client'

import { strings } from '../strings.ts'

export const minPasswordLength = 10

export interface LoginSearch {
  next?: string
}

export interface SetupSearch {
  token?: string
}

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() !== '' ? value.trim() : undefined
}

export function safeNext(next: unknown): string {
  if (typeof next !== 'string' || !next.startsWith('/')) return '/'
  if (next.startsWith('//') || next.startsWith('/\\')) return '/'
  const path = next.split(/[?#]/, 1)[0] ?? ''
  if (path === '/login' || path === '/setup') return '/'
  return next
}

export function parseLoginSearch(search: Record<string, unknown>): LoginSearch {
  const next = text(search.next)
  return next && safeNext(next) !== '/' ? { next } : {}
}

export function parseSetupSearch(search: Record<string, unknown>): SetupSearch {
  const token = text(search.token)
  return token ? { token } : {}
}

export function passwordShortBy(password: string): number {
  return Math.max(0, minPasswordLength - Array.from(password).length)
}

export function retryMinutes(seconds: number | undefined): number {
  return Math.max(1, Math.ceil((seconds ?? 60) / 60))
}

export function sentence(message: string): string {
  const trimmed = message.trim()
  if (!trimmed) return trimmed
  const first = trimmed.charAt(0).toUpperCase() + trimmed.slice(1)
  return /[.!?…]$/.test(first) ? first : `${first}.`
}

export function initials(name: string, email: string): string {
  const source = name.trim() || email.trim()
  return (source.at(0) ?? '').toUpperCase()
}

export function formText(form: FormData, name: string): string {
  const value = form.get(name)
  return typeof value === 'string' ? value : ''
}

export function authErrorMessage(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 429) return strings.auth.tooMany(retryMinutes(error.retryAfter))
    if (error.status < 500 && error.message) return sentence(error.message)
  }
  return strings.auth.failed
}
