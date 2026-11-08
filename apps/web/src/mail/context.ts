import { createContext, type RefObject, use } from 'react'

import type { Layout } from '../hooks/use-layout.ts'

export interface ListController {
  move: (delta: number) => void
  open: () => void
  focus: () => void
}

export interface InsightController {
  copyCode: () => boolean
  openLink: () => boolean
  unsubscribe: () => boolean
  category: () => boolean
}

export interface MailboxController {
  toggleRead: () => void
  toggleFlag: () => void
  move: () => void
  trash: () => void
}

export interface MailContextValue {
  archiveId: string
  layout: Layout
  selectedId: string | undefined
  select: (id: string | undefined, options?: { replace?: boolean }) => void
  focusReader: () => void
  list: RefObject<ListController | null>
  insights: RefObject<InsightController | null>
  mailbox: RefObject<MailboxController | null>
}

export const MailContext = createContext<MailContextValue | null>(null)

export function useMail(): MailContextValue {
  const value = use(MailContext)
  if (!value) throw new Error('useMail outside MailContext')
  return value
}

export const ids = {
  list: 'message-list',
  search: 'search-input',
  reader: 'reader',
} as const
