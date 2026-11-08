import { useCallback, useSyncExternalStore } from 'react'

import { parseReadOnOpen, readOnOpenKey } from '../lib/read-on-open.ts'

const listeners = new Set<() => void>()

function read(): boolean {
  try {
    return parseReadOnOpen(window.localStorage.getItem(readOnOpenKey))
  } catch {
    return false
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  const onStorage = (event: StorageEvent) => {
    if (event.key === readOnOpenKey) listener()
  }
  window.addEventListener('storage', onStorage)
  return () => {
    listeners.delete(listener)
    window.removeEventListener('storage', onStorage)
  }
}

/** The user's "mark as read when opened" choice in this browser; off unless turned on. */
export function useReadOnOpen(): [boolean, (next: boolean) => void] {
  const value = useSyncExternalStore(subscribe, read, () => false)
  const set = useCallback((next: boolean) => {
    try {
      window.localStorage.setItem(readOnOpenKey, next ? 'on' : 'off')
    } catch {
      return
    }
    for (const listener of listeners) listener()
  }, [])
  return [value, set]
}
