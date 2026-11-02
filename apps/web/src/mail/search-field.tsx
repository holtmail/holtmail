import { distance, transition } from '@holtmail/tokens'
import { cn, Input } from '@holtmail/ui'
import { useNavigate } from '@tanstack/react-router'
import { CircleAlertIcon, SearchIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { useRef, useState } from 'react'

import { insertToken, operators } from '../lib/query.ts'
import { strings } from '../strings.ts'
import { ids, useMail } from './context.ts'

export function SearchField({ query, error }: { query: string; error?: string }) {
  const { archiveId, list } = useMail()
  const navigate = useNavigate()
  const [value, setValue] = useState(query)
  const [synced, setSynced] = useState(query)
  const [focused, setFocused] = useState(false)
  const input = useRef<HTMLInputElement>(null)
  const wrapper = useRef<HTMLDivElement>(null)

  if (synced !== query) {
    setSynced(query)
    setValue(query)
  }

  const submit = () => {
    const q = value.trim()
    if (q) void navigate({ to: '/a/$archiveId/search', params: { archiveId }, search: { q } })
    else void navigate({ to: '/a/$archiveId', params: { archiveId } })
  }

  const insert = (token: string) => {
    const field = input.current
    const start = field?.selectionStart ?? value.length
    const end = field?.selectionEnd ?? value.length
    const next = insertToken(value, start, end, token)
    setValue(next.value)
    requestAnimationFrame(() => {
      field?.focus()
      field?.setSelectionRange(next.caret, next.caret)
    })
  }

  const errorId = `${ids.search}-error`
  const showHints = focused || Boolean(query)

  return (
    <div
      ref={wrapper}
      className="grid gap-2"
      onFocus={() => setFocused(true)}
      onBlur={(event) => {
        if (!wrapper.current?.contains(event.relatedTarget)) setFocused(false)
      }}
    >
      <form
        role="search"
        className="relative"
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
      >
        <SearchIcon
          aria-hidden
          className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-fg-subtle"
        />
        <Input
          ref={input}
          id={ids.search}
          type="search"
          value={value}
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault()
              event.currentTarget.blur()
              list.current?.focus()
            }
          }}
          placeholder={strings.search.placeholder}
          aria-label={strings.search.label}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? errorId : undefined}
          autoComplete="off"
          spellCheck={false}
          className="pl-9 [&::-webkit-search-cancel-button]:hidden"
        />
      </form>
      <AnimatePresence initial={false}>
        {error && (
          <m.p
            key="error"
            id={errorId}
            role="alert"
            className="flex items-start gap-2 text-xs text-danger [&_svg]:mt-0.5 [&_svg]:size-3 [&_svg]:shrink-0"
            initial={{ opacity: 0, y: -distance.xs }}
            animate={{ opacity: 1, y: 0, transition: transition.productive.fast02 }}
            exit={{ opacity: 0, transition: transition.productiveExit.fast01 }}
          >
            <CircleAlertIcon />
            {error}
          </m.p>
        )}
      </AnimatePresence>
      {showHints && (
        <div
          role="group"
          aria-label={strings.search.operators}
          className="-mx-1 flex flex-wrap gap-1"
        >
          {operators.map((token) => (
            <button
              key={token}
              type="button"
              aria-label={strings.search.insert(token)}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => insert(token)}
              className={cn(
                'h-6 px-1 font-mono text-xs text-fg-subtle',
                'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover hover:text-fg',
                'outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
              )}
            >
              {token}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
