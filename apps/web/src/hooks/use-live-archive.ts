import { accountsQuery } from '@holtmail/api-client'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef } from 'react'

import { anyRunning } from '../lib/accounts.ts'

export function useLiveArchive(archiveId: string): boolean {
  const queryClient = useQueryClient()
  const accounts = useQuery(accountsQuery(archiveId))
  const running = anyRunning(accounts.data)
  const wasRunning = useRef(running)

  useEffect(() => {
    if (wasRunning.current && !running) {
      void queryClient.invalidateQueries({ queryKey: ['archives'] })
    }
    wasRunning.current = running
    const timer = running
      ? window.setInterval(() => {
          void queryClient.invalidateQueries({ queryKey: ['archives'], exact: true })
          void queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'folders'] })
          void queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'labels'] })
          void queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'messages'] })
        }, 4_000)
      : undefined
    return () => window.clearInterval(timer)
  }, [running, archiveId, queryClient])

  return running
}
