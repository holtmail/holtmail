import type {
  Address,
  Alias,
  AliasProvider,
  AliasStrategies,
  MessageAlias,
} from '@holtmail/api-client'

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

export const replyStrategies = ['reverse_alias', 'direct', 'reply_header'] as const
export type ReplyStrategyName = (typeof replyStrategies)[number]

export const isReplyStrategy = (value: string): value is ReplyStrategyName =>
  (replyStrategies as readonly string[]).includes(value)

export interface ServiceStrategy {
  provider: string
  /** The provider's own default. */
  fallback: string
  override: ReplyStrategyName | null
}

/** One row per provider the archive has to do with (`inUse`, or one with an override), in
 * name order, with the provider's default and the service-wide override. */
export function serviceStrategies(
  strategies: AliasStrategies,
  inUse: readonly string[],
): ServiceStrategy[] {
  const overrides = new Map<string, ReplyStrategyName>()
  for (const o of strategies.overrides) {
    if (o.scope === 'service' && isReplyStrategy(o.strategy)) overrides.set(o.key, o.strategy)
  }
  const ids = new Set([...inUse.filter(Boolean), ...overrides.keys()])
  return [...ids]
    .map((provider) => ({
      provider,
      fallback: strategies.defaults.find((d) => d.provider === provider)?.strategy ?? 'direct',
      override: overrides.get(provider) ?? null,
    }))
    .toSorted((a, b) => aliasProviderName(a.provider).localeCompare(aliasProviderName(b.provider)))
}

/** The per-domain overrides, by domain. */
export function domainOverrides(
  strategies: AliasStrategies,
): { domain: string; strategy: ReplyStrategyName }[] {
  return strategies.overrides
    .flatMap(({ scope, key, strategy }) =>
      scope === 'domain' && isReplyStrategy(strategy) ? [{ domain: key, strategy }] : [],
    )
    .toSorted((a, b) => a.domain.localeCompare(b.domain))
}

/** The domains of the archive's aliases, lowercase, unique and sorted. */
export function aliasDomains(aliases: readonly Alias[]): string[] {
  return [
    ...new Set(aliases.map((a) => aliasDomain(a.address).toLowerCase()).filter(Boolean)),
  ].toSorted()
}

/** A domain as the server keys it: lowercase, without a leading `@`; `null` when it is not a
 * domain name. */
export function domainKey(text: string): string | null {
  const domain = text.trim().replace(/^@/, '').toLowerCase()
  return /^[a-z0-9-]+(\.[a-z0-9-]+)+$/.test(domain) ? domain : null
}
