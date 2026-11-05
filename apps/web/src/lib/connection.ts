import { defaultPort, type Security } from './providers.ts'

export interface ImapServer {
  host: string
  port: number
  security: string
}

export function effectivePort(port: string, security: Security): number {
  return Number(port) || defaultPort(security)
}

export function imapMoves(saved: ImapServer, next: ImapServer): boolean {
  return (
    saved.host.trim().toLowerCase() !== next.host.trim().toLowerCase() ||
    saved.port !== next.port ||
    (next.security === 'plain' && saved.security !== 'plain')
  )
}

function jmapOrigin(url: string): string | undefined {
  const trimmed = url.trim()
  if (!trimmed) return undefined
  try {
    return new URL(trimmed.includes('://') ? trimmed : `https://${trimmed}`).origin
  } catch {
    return undefined
  }
}

export function jmapMoves(saved: string, next: string): boolean {
  const before = jmapOrigin(saved)
  return before === undefined || before !== jmapOrigin(next)
}
