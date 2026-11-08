import { type Mailbox, peopleInfiniteQuery } from '@holtmail/api-client'
import { Button, cn } from '@holtmail/ui'
import { useInfiniteQuery } from '@tanstack/react-query'
import { XIcon } from 'lucide-react'
import { type ClipboardEvent, type KeyboardEvent, useId, useRef, useState } from 'react'

import {
  addRecipients,
  chipLabel,
  parseRecipients,
  peopleSuggestions,
  removeRecipient,
} from '../lib/recipients.ts'
import { strings } from '../strings.ts'

const t = strings.writing.recipients

/** A recipient field: chips for every address, typed text turned into chips at a comma,
 * semicolon, Enter, Tab or on leaving the field, and People suggestions while typing. */
export function RecipientField({
  archiveId,
  label,
  value,
  onChange,
  autoFocus,
}: {
  archiveId: string
  label: string
  value: Mailbox[]
  onChange: (value: Mailbox[]) => void
  autoFocus?: boolean
}) {
  const id = useId()
  const input = useRef<HTMLInputElement>(null)
  const [text, setText] = useState('')
  const [invalid, setInvalid] = useState<string[]>([])
  const [cursor, setCursor] = useState(0)
  const query = text.trim()
  const people = useInfiniteQuery({
    ...peopleInfiniteQuery(archiveId, { q: query }),
    enabled: query.length >= 2,
  })
  const [dismissed, setDismissed] = useState<string>()
  const found = peopleSuggestions(
    people.data?.pages.flatMap((page) => page.items) ?? [],
    query,
    value,
  )
  const suggestions = dismissed === query ? [] : found
  const listId = `${id}-suggestions`

  const commit = (raw: string) => {
    const parsed = parseRecipients(raw)
    if (parsed.mailboxes.length > 0) onChange(addRecipients(value, parsed.mailboxes))
    setInvalid(parsed.invalid)
    setText(parsed.invalid.join(', '))
    setCursor(0)
  }

  const pick = (mailbox: Mailbox) => {
    onChange(addRecipients(value, [mailbox]))
    setText('')
    setInvalid([])
    setCursor(0)
    input.current?.focus()
  }

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape' && suggestions.length > 0 && query) {
      event.preventDefault()
      setDismissed(query)
    } else if (event.key === 'ArrowDown' && suggestions.length > 0) {
      event.preventDefault()
      setCursor((c) => (c + 1) % suggestions.length)
    } else if (event.key === 'ArrowUp' && suggestions.length > 0) {
      event.preventDefault()
      setCursor((c) => (c - 1 + suggestions.length) % suggestions.length)
    } else if (
      event.key === 'Enter' ||
      event.key === 'Tab' ||
      event.key === ',' ||
      event.key === ';'
    ) {
      const chosen = suggestions[cursor]
      if (event.key === 'Enter' && chosen && query) {
        event.preventDefault()
        pick(chosen)
      } else if (query) {
        if (event.key !== 'Tab') event.preventDefault()
        commit(text)
      }
    } else if (event.key === 'Backspace' && text === '' && value.length > 0) {
      const last = value.at(-1)
      if (last) onChange(removeRecipient(value, last.address))
    }
  }

  const onPaste = (event: ClipboardEvent<HTMLInputElement>) => {
    const pasted = event.clipboardData.getData('text')
    if (/[,;\n]/.test(pasted)) {
      event.preventDefault()
      commit(`${text}${pasted}`)
    }
  }

  return (
    <div className="relative grid grid-cols-[4rem_1fr] items-start border-b border-border">
      <label htmlFor={id} className="pt-2.5 text-xs text-fg-muted">
        {label}
      </label>
      <div className="grid gap-1 py-1">
        <ul className="flex min-h-8 flex-wrap items-center gap-1">
          {value.map((mailbox) => (
            <li
              key={mailbox.address.toLowerCase()}
              title={mailbox.address}
              className="flex h-6 max-w-64 items-center gap-1 border border-border bg-bg-subtle pl-2 text-xs text-fg"
            >
              <span className="truncate">{chipLabel(mailbox)}</span>
              <button
                type="button"
                aria-label={t.remove(mailbox.address)}
                className="flex size-6 items-center justify-center text-fg-subtle hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus [&_svg]:size-3"
                onClick={() => onChange(removeRecipient(value, mailbox.address))}
              >
                <XIcon />
              </button>
            </li>
          ))}
          <li className="min-w-32 flex-1">
            <input
              ref={input}
              id={id}
              autoFocus={autoFocus}
              role="combobox"
              aria-expanded={suggestions.length > 0}
              aria-controls={listId}
              aria-activedescendant={suggestions.length > 0 ? `${listId}-${cursor}` : undefined}
              aria-invalid={invalid.length > 0}
              autoComplete="off"
              spellCheck={false}
              placeholder={value.length === 0 ? t.placeholder : undefined}
              className="h-8 w-full bg-transparent text-sm text-fg outline-none placeholder:text-fg-subtle"
              value={text}
              onChange={(event) => {
                setText(event.target.value)
                setInvalid([])
                setCursor(0)
              }}
              onKeyDown={onKeyDown}
              onPaste={onPaste}
              onBlur={() => {
                if (query) commit(text)
              }}
            />
          </li>
        </ul>
        {invalid.length > 0 && (
          <p role="alert" className="text-xs text-danger">
            {invalid.map((part) => t.invalid(part)).join(' ')}
          </p>
        )}
      </div>
      {suggestions.length > 0 && query && (
        <ul
          id={listId}
          role="listbox"
          aria-label={t.suggestions}
          className="absolute top-full right-0 left-16 z-10 grid border border-border-strong bg-bg"
        >
          {suggestions.map((mailbox, index) => (
            <li
              key={mailbox.address}
              id={`${listId}-${index}`}
              role="option"
              aria-selected={index === cursor}
              className={cn(
                'flex h-9 cursor-default items-center gap-2 px-3 text-sm',
                index === cursor ? 'bg-bg-selected' : 'hover:bg-bg-hover',
              )}
              onMouseDown={(event) => {
                event.preventDefault()
                pick(mailbox)
              }}
            >
              {mailbox.name && <span className="truncate text-fg">{mailbox.name}</span>}
              <span className="truncate font-mono text-xs text-fg-subtle">{mailbox.address}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}

export function AddFieldButton({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <Button variant="ghost" size="sm" onClick={onClick}>
      {label}
    </Button>
  )
}
