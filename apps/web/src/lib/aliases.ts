import type { Address, Alias, AliasProvider, MessageAlias } from '@holtmail/api-client'

export type AliasSource = 'service' | 'manual' | 'mail'

export interface AliasFilter {
  text: string
  source: AliasSource | 'all'
  provider: string
}

export const ALL = 'all'

export function isAliasSource(value: string): value is AliasSource | 'all' {
  return value === ALL || value === 'service' || value === 'manual' || value === 'mail'
}

export function providerName(providers: readonly AliasProvider[] | undefined, id: string): string {
  return providers?.find((provider) => provider.id === id)?.name ?? (id || '—')
}

export function aliasProviders(aliases: readonly Alias[]): string[] {
  return [...new Set(aliases.map((alias) => alias.provider).filter(Boolean))].toSorted()
}

export function filterAliases(aliases: readonly Alias[], filter: AliasFilter): Alias[] {
  const words = filter.text.toLowerCase().split(/\s+/).filter(Boolean)
  return aliases.filter((alias) => {
    if (filter.source !== ALL && alias.source !== filter.source) return false
    if (filter.provider !== ALL && alias.provider !== filter.provider) return false
    const haystack = [
      alias.address,
      alias.provider,
      alias.note,
      alias.name,
      alias.for_domain,
      ...alias.mailboxes,
      ...alias.accounts.map((account) => account.name),
    ]
      .filter(Boolean)
      .join(' ')
      .toLowerCase()
    return words.every((word) => haystack.includes(word))
  })
}

const providerNames: Record<string, string> = {
  simplelogin: 'SimpleLogin',
  addy: 'addy.io',
  relay: 'Firefox Relay',
  fastmail: 'Fastmail',
  duck: 'DuckDuckGo',
  apple: 'Apple',
}

/** The display name of an alias service, or its id when unknown. */
export function aliasProviderName(id: string): string {
  return providerNames[id] ?? id
}

export function shownFrom(message: {
  from: readonly Address[]
  alias?: MessageAlias | null
}): readonly Address[] {
  const sender = message.alias?.sender
  return sender ? [sender] : message.from
}

export type AliasTrust = 'signed' | 'known' | 'claimed'

export function aliasTrust(alias: MessageAlias): AliasTrust {
  if (alias.signed) return 'signed'
  if (alias.known) return 'known'
  return 'claimed'
}

export const aliasSearch = (address: string): string => `to:${address}`

export function aliasDomain(address: string): string {
  const at = address.lastIndexOf('@')
  return at < 0 ? '' : address.slice(at + 1)
}
