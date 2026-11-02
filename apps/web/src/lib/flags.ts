const has = (flags: readonly string[], flag: string): boolean =>
  flags.some((item) => item.toLowerCase() === flag)

export const isUnread = (flags: readonly string[]): boolean => !has(flags, '\\seen')

export const isFlagged = (flags: readonly string[]): boolean => has(flags, '\\flagged')
