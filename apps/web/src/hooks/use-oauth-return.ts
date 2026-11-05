import { useRouter } from '@tanstack/react-router'
import { useEffect, useState } from 'react'

import { type OAuthReturn, parseOAuthReturn, withoutOAuthParams } from '../lib/oauth.ts'

export function useOAuthReturn(): [OAuthReturn | undefined, () => void] {
  const router = useRouter()
  const [result, setResult] = useState(() => parseOAuthReturn(window.location.search))

  useEffect(() => {
    const { pathname, search, hash } = window.location
    const cleaned = withoutOAuthParams(search)
    if (cleaned !== search) router.history.replace(`${pathname}${cleaned}${hash}`)
  }, [router])

  return [result, () => setResult(undefined)]
}
