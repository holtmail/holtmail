import { useEffect, useRef } from 'react'

import { isMac } from '../lib/keymap.ts'
import { createMatcher, isEditable } from '../lib/shortcuts.ts'

export function useHotkeys<Id extends string>(
  bindings: Readonly<Record<Id, readonly string[]>>,
  handler: (id: Id, event: KeyboardEvent) => void,
): void {
  const handlerRef = useRef(handler)
  useEffect(() => {
    handlerRef.current = handler
  })

  useEffect(() => {
    const matcher = createMatcher(bindings, isMac)
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) return
      const target = event.target
      if (target instanceof Element && target.closest('[role="dialog"]')) return
      const modified = event.metaKey || event.ctrlKey
      if (isEditable(target) && event.key !== 'Escape' && !modified) return
      const id = matcher.feed(event, event.timeStamp)
      if (id === undefined) return
      event.preventDefault()
      handlerRef.current(id, event)
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [bindings])
}
