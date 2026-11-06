import { useCallback, useEffect, useRef, useState } from 'react'

import { type CopyEnv, type CopyResult, copyText } from './copy.ts'

/** How long `copied` stays true after a successful copy. */
export const COPIED_MS = 1500

/** State and action returned by `useCopy`. */
export interface UseCopy {
  /** True for `COPIED_MS` after the last copy reached the clipboard. */
  copied: boolean
  /** The text the user has to copy by hand after both automatic paths failed, else `null`. */
  manual: string | null
  /** Copies `text`; never rejects. A later call supersedes an earlier one. */
  copy: (text: string) => Promise<CopyResult>
  /** Clears `copied` and `manual`. */
  reset: () => void
}

/**
 * Copies text through `copyText` and tracks the outcome. No state is set after unmount, and a
 * pending timer is cleared on unmount.
 */
export function useCopy(env?: CopyEnv): UseCopy {
  const [stamp, setStamp] = useState(0)
  const [manual, setManual] = useState<string | null>(null)
  const mounted = useRef(true)
  const call = useRef(0)

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])

  useEffect(() => {
    if (stamp === 0) return undefined
    const timer = window.setTimeout(() => setStamp(0), COPIED_MS)
    return () => window.clearTimeout(timer)
  }, [stamp])

  const copy = useCallback(
    async (text: string): Promise<CopyResult> => {
      const id = ++call.current
      let result: CopyResult
      try {
        result = await copyText(text, env)
      } catch {
        result = 'manual'
      }
      if (!mounted.current || id !== call.current) return result
      setStamp(result === 'manual' ? 0 : id)
      setManual(result === 'manual' ? text : null)
      return result
    },
    [env],
  )

  const reset = useCallback(() => {
    call.current++
    setStamp(0)
    setManual(null)
  }, [])

  return { copied: stamp !== 0, manual, copy, reset }
}
