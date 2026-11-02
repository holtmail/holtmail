import { useSyncExternalStore } from 'react'

const wide = '(min-width: 68.75rem)'
const medium = '(min-width: 45rem)'

function subscribe(listener: () => void): () => void {
  const queries = [wide, medium].map((query) => window.matchMedia(query))
  for (const query of queries) query.addEventListener('change', listener)
  return () => {
    for (const query of queries) query.removeEventListener('change', listener)
  }
}

export type Layout = 'wide' | 'medium' | 'narrow'

function snapshot(): Layout {
  if (window.matchMedia(wide).matches) return 'wide'
  if (window.matchMedia(medium).matches) return 'medium'
  return 'narrow'
}

export function useLayout(): Layout {
  return useSyncExternalStore(subscribe, snapshot, () => 'wide' as const)
}
