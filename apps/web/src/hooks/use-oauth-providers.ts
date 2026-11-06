import { type OAuthProvider, oauthProvidersQuery } from '@holtmail/api-client'
import { useQuery } from '@tanstack/react-query'

import { offeredProviders } from '../lib/oauth.ts'

export function useOAuthProviders(): readonly OAuthProvider[] {
  const { data } = useQuery(oauthProvidersQuery)
  return offeredProviders(data)
}
