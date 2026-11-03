import { useCallback, useSyncExternalStore } from 'react'

import { parseRemoteImages, type RemoteImages, remoteImagesKey } from '../lib/remote-images.ts'

const listeners = new Set<() => void>()

function read(): RemoteImages {
  try {
    return parseRemoteImages(window.localStorage.getItem(remoteImagesKey))
  } catch {
    return 'ask'
  }
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  const onStorage = (event: StorageEvent) => {
    if (event.key === remoteImagesKey) listener()
  }
  window.addEventListener('storage', onStorage)
  return () => {
    listeners.delete(listener)
    window.removeEventListener('storage', onStorage)
  }
}

export function useRemoteImages(): [RemoteImages, (next: RemoteImages) => void] {
  const value = useSyncExternalStore(subscribe, read, () => 'ask' as const)
  const set = useCallback((next: RemoteImages) => {
    try {
      window.localStorage.setItem(remoteImagesKey, next)
    } catch {
      return
    }
    for (const listener of listeners) listener()
  }, [])
  return [value, set]
}
