import type { JmapAccountRequest, JmapAuth } from '@holtmail/api-client'

export const fastmailSession = 'https://api.fastmail.com/jmap/session'

export type JmapPreset = 'fastmail' | 'other'

export interface JmapForm {
  preset: JmapPreset
  url: string
  auth: JmapAuth
  username: string
  secret: string
  name: string
}

export function jmapRequest(form: JmapForm): JmapAccountRequest {
  const fastmail = form.preset === 'fastmail'
  const auth: JmapAuth = fastmail ? 'token' : form.auth
  return {
    url: fastmail ? fastmailSession : form.url.trim(),
    auth,
    username: auth === 'password' ? form.username.trim() : '',
    password: form.secret.trim(),
    name: form.name.trim() || (fastmail ? 'Fastmail' : null),
  }
}

export function jmapMissing(form: JmapForm): 'url' | 'username' | 'secret' | undefined {
  const request = jmapRequest(form)
  if (!request.url) return 'url'
  if (request.auth === 'password' && !request.username) return 'username'
  if (!request.password) return 'secret'
  return undefined
}

export const isJmapAuth = (value: string): value is JmapAuth =>
  value === 'password' || value === 'token'
