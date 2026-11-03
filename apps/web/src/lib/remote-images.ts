import type { RemoteMode } from '@holtmail/api-client'

export type RemoteImages = 'ask' | 'proxy' | 'direct'

export const remoteImagesKey = 'holtmail.remote-images'

export function parseRemoteImages(value: string | null | undefined): RemoteImages {
  return value === 'proxy' || value === 'direct' ? value : 'ask'
}

export function liveMode(preference: RemoteImages): RemoteMode {
  return preference === 'direct' ? 'direct' : 'proxy'
}

export function bodyMode(preference: RemoteImages, shown: boolean): RemoteMode {
  if (preference === 'ask') return shown ? 'proxy' : 'block'
  return preference
}
