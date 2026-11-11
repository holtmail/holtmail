import {
  collectionsQuery,
  foldersQuery,
  identitiesQuery,
  previewTemplate,
  searchInfiniteQuery,
  transportsQuery,
  webhooksQuery,
} from '@holtmail/api-client'
import { cn, Input, Switch } from '@holtmail/ui'
import { keepPreviousData, useInfiniteQuery, useQuery } from '@tanstack/react-query'
import { type ReactNode, useId, useRef, useState } from 'react'

import { useDebounced } from '../hooks/use-debounced.ts'
import { TextArea } from '../items/inputs.tsx'
import { insertToken, operators } from '../lib/query.ts'
import {
  type Field,
  insertVariable,
  readInteger,
  splitList,
  templateVariables,
} from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'

const t = strings.rules
const locale = currentLocale()

const control =
  'border border-border bg-bg text-sm text-fg hover:border-border-strong focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

/** The message template previews render against, chosen once for the whole editor. */
export interface PreviewMessage {
  id: string
  subject: string
}

export interface FieldContext {
  archiveId: string
  trigger: string
  message: PreviewMessage | null
}

const fieldLabel = (key: string) =>
  t.fields[key] ?? key.replace(/_/g, ' ').replace(/^\w/, (c) => c.toUpperCase())

const optionLabel = (value: string, description = '') =>
  t.options[value] ?? (description.replace(/[`.]/g, '') || value)

function shown(value: unknown): string {
  if (value === undefined || value === null) return ''
  if (typeof value === 'string') return value
  if (Array.isArray(value)) return value.map(shown).join(', ')
  return JSON.stringify(value)
}

/** A field with its label and notes. A field of several controls (`choices`) is a group named
 * by its legend; any other field's label names its one control `id`. */
function FieldShell({ field, id, children }: { field: Field; id: string; children: ReactNode }) {
  const notes: string[] = []
  if (!field.required) notes.push(t.inspector.optional)
  if (field.default !== undefined && shown(field.default) !== '') {
    notes.push(t.inspector.defaultValue(shown(field.default)))
  }
  const about = (notes.length > 0 || (locale === 'en' && field.description)) && (
    <p id={`${id}-notes`} className="text-xs text-fg-subtle">
      {locale === 'en' && field.description && (
        <span className="block">{field.description.replace(/`/g, '')}</span>
      )}
      {notes.join(' · ')}
    </p>
  )
  if (field.kind === 'choices') {
    return (
      <fieldset className="grid gap-1" aria-describedby={about ? `${id}-notes` : undefined}>
        <legend className="mb-1 text-xs text-fg-muted">{fieldLabel(field.key)}</legend>
        {children}
        {about}
      </fieldset>
    )
  }
  return (
    <div className="grid gap-1">
      <label htmlFor={id} className="text-xs text-fg-muted">
        {fieldLabel(field.key)}
      </label>
      {children}
      {about}
    </div>
  )
}

function useChoices(field: Field, archiveId: string): { value: string; label: string }[] | null {
  const kind = field.kind
  const identities = useQuery({ ...identitiesQuery(archiveId), enabled: kind === 'identity' })
  const transports = useQuery({ ...transportsQuery(archiveId), enabled: kind === 'transport' })
  const collections = useQuery({
    ...collectionsQuery(archiveId),
    enabled: kind === 'calendar' || kind === 'address_book',
  })
  const webhooks = useQuery({ ...webhooksQuery, enabled: kind === 'webhook', retry: false })
  switch (kind) {
    case 'identity':
      return identities.data
        ? [
            { value: 'auto', label: t.inspector.auto },
            ...identities.data
              .filter((i) => i.active)
              .map((i) => ({
                value: i.id,
                label: i.name ? `${i.name} <${i.address}>` : i.address,
              })),
          ]
        : null
    case 'transport':
      return transports.data?.map((tr) => ({ value: tr.id, label: tr.name })) ?? null
    case 'calendar':
    case 'address_book': {
      const wanted = kind === 'calendar' ? 'calendar' : 'contacts'
      return (
        collections.data
          ?.filter((c) => c.kind === wanted && c.removed_at == null)
          .map((c) => ({ value: c.id, label: c.name })) ?? null
      )
    }
    case 'webhook':
      return webhooks.data?.webhooks.map((w) => ({ value: w.id, label: w.url })) ?? null
    default:
      return null
  }
}

function ChoiceField({
  field,
  id,
  value,
  choices,
  onChange,
}: {
  field: Field
  id: string
  value: unknown
  choices: { value: string; label: string }[]
  onChange: (value: unknown) => void
}) {
  const current = typeof value === 'string' ? value : ''
  const all = [...choices]
  if (current && !all.some((c) => c.value === current)) {
    all.push({ value: current, label: t.inspector.unknownOption(current) })
  }
  return (
    <select
      id={id}
      className={cn(control, 'h-8 max-w-full px-1')}
      value={current}
      onChange={(event) => onChange(event.target.value || (field.nullable ? null : undefined))}
    >
      <option value="">{field.required ? t.inspector.choose : t.inspector.none}</option>
      {all.map((c) => (
        <option key={c.value} value={c.value}>
          {c.label}
        </option>
      ))}
    </select>
  )
}

function ListField({
  id,
  value,
  onChange,
}: {
  id: string
  value: unknown
  onChange: (value: unknown) => void
}) {
  const list = Array.isArray(value) ? value.filter((v) => typeof v === 'string') : []
  const [text, setText] = useState(list.join(', '))
  const [synced, setSynced] = useState(list.join(', '))
  const joined = list.join(', ')
  if (joined !== synced && splitList(text).join(', ') !== joined) {
    setSynced(joined)
    setText(joined)
  }
  return (
    <input
      id={id}
      className={cn(control, 'h-8 min-w-0 px-2 font-mono text-xs')}
      value={text}
      placeholder={t.inspector.list}
      spellCheck={false}
      onChange={(event) => {
        setText(event.target.value)
        const next = splitList(event.target.value)
        setSynced(next.join(', '))
        onChange(next.length > 0 ? next : undefined)
      }}
    />
  )
}

function ChoicesField({
  field,
  value,
  onChange,
}: {
  field: Field
  value: unknown
  onChange: (value: unknown) => void
}) {
  const chosen = new Set(Array.isArray(value) ? value.filter((v) => typeof v === 'string') : [])
  return (
    <div className="flex flex-wrap gap-x-4 gap-y-1">
      {field.options.map((option) => (
        <label key={option.value} className="flex items-center gap-2 text-sm text-fg">
          <input
            type="checkbox"
            className="size-4 accent-accent"
            checked={chosen.has(option.value)}
            onChange={(event) => {
              const next = new Set(chosen)
              if (event.target.checked) next.add(option.value)
              else next.delete(option.value)
              const list = field.options.map((o) => o.value).filter((v) => next.has(v))
              onChange(list.length > 0 ? list : undefined)
            }}
          />
          {optionLabel(option.value, option.description)}
        </label>
      ))}
    </div>
  )
}

/** The search box of a `query` field: the search operators, and how many archived messages
 * the search finds now, from the index the matcher agrees with. */
function QueryField({
  id,
  value,
  archiveId,
  onChange,
}: {
  id: string
  value: unknown
  archiveId: string
  onChange: (value: unknown) => void
}) {
  const text = typeof value === 'string' ? value : ''
  const input = useRef<HTMLInputElement>(null)
  const q = useDebounced(text.trim(), 400)
  const search = useInfiniteQuery({
    ...searchInfiniteQuery(archiveId, q),
    enabled: q.length > 0,
    placeholderData: keepPreviousData,
  })
  const total = search.data?.pages[0]?.total
  const insert = (token: string) => {
    const field = input.current
    const next = insertToken(
      text,
      field?.selectionStart ?? text.length,
      field?.selectionEnd ?? text.length,
      token,
    )
    onChange(next.value)
    requestAnimationFrame(() => {
      field?.focus()
      field?.setSelectionRange(next.caret, next.caret)
    })
  }
  const statusId = `${id}-status`
  return (
    <div className="grid gap-2">
      <Input
        ref={input}
        id={id}
        value={text}
        onChange={(event) => onChange(event.target.value)}
        autoComplete="off"
        spellCheck={false}
        aria-describedby={statusId}
        aria-invalid={search.isError ? true : undefined}
        className="font-mono text-xs"
      />
      <div role="group" aria-label={t.query.operators} className="-mx-1 flex flex-wrap gap-1">
        {operators.map((token) => (
          <button
            key={token}
            type="button"
            aria-label={t.query.insert(token)}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => insert(token)}
            className="h-6 px-1 font-mono text-xs text-fg-subtle transition-colors duration-fast-02 ease-productive outline-none hover:bg-bg-hover hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
          >
            {token}
          </button>
        ))}
      </div>
      <p id={statusId} aria-live="polite" className="text-xs text-fg-subtle">
        {!q ? (
          ''
        ) : search.isError ? (
          <span className="text-danger">{search.error.message}</span>
        ) : total === undefined ? (
          t.query.checking
        ) : total === 0 ? (
          t.query.none
        ) : (
          t.query.matches(total)
        )}
      </p>
    </div>
  )
}

/** Searches the archive for a message to preview templates with. */
export function MessagePicker({
  archiveId,
  value,
  onChange,
  label = t.template.previewWith,
  hint = t.template.noMessage,
}: {
  archiveId: string
  value: PreviewMessage | null
  onChange: (value: PreviewMessage | null) => void
  label?: string
  hint?: string
}) {
  const id = useId()
  const [text, setText] = useState('')
  const q = useDebounced(text.trim(), 300)
  const search = useInfiniteQuery({ ...searchInfiniteQuery(archiveId, q), enabled: q.length > 0 })
  const hits = search.data?.pages[0]?.items.slice(0, 6) ?? []
  return (
    <div className="grid gap-2">
      {value ? (
        <span id={`${id}-label`} className="text-xs text-fg-muted">
          {label}
        </span>
      ) : (
        <label htmlFor={id} className="text-xs text-fg-muted">
          {label}
        </label>
      )}
      {value ? (
        <div
          role="group"
          aria-labelledby={`${id}-label`}
          className="flex items-center gap-2 border border-border px-2 py-1 text-sm"
        >
          <span className="min-w-0 flex-1 truncate">{value.subject || t.noSubject}</span>
          <button
            type="button"
            className="text-xs text-fg-muted underline-offset-2 hover:underline"
            onClick={() => onChange(null)}
          >
            {t.template.pick}
          </button>
        </div>
      ) : (
        <>
          <Input
            id={id}
            type="search"
            value={text}
            placeholder={t.template.pickSearch}
            onChange={(event) => setText(event.target.value)}
            autoComplete="off"
          />
          {q && (
            <ul className="grid border border-border">
              {hits.length === 0 && !search.isPending && (
                <li className="px-2 py-1 text-xs text-fg-subtle">{t.template.pickEmpty}</li>
              )}
              {hits.map((hit) => (
                <li key={hit.id}>
                  <button
                    type="button"
                    className="w-full truncate px-2 py-1 text-left text-sm hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus"
                    onClick={() => {
                      onChange({ id: hit.id, subject: hit.subject ?? '' })
                      setText('')
                    }}
                  >
                    {hit.subject || t.noSubject}
                  </button>
                </li>
              ))}
            </ul>
          )}
          <p className="text-xs text-fg-subtle">{hint}</p>
        </>
      )}
    </div>
  )
}

/** A template field: the text, buttons that insert each variable, and the text it renders to
 * for the chosen message, from the server's sandbox. */
function TemplateField({
  field,
  id,
  value,
  context,
  onChange,
}: {
  field: Field
  id: string
  value: unknown
  context: FieldContext
  onChange: (value: unknown) => void
}) {
  const text = typeof value === 'string' ? value : ''
  const area = useRef<HTMLTextAreaElement>(null)
  const template = useDebounced(text, 400)
  const preview = useQuery({
    queryKey: [
      'rules',
      'preview',
      context.archiveId,
      context.trigger,
      context.message?.id ?? null,
      template,
    ],
    queryFn: () =>
      previewTemplate(context.archiveId, {
        template,
        trigger: context.trigger,
        ...(context.message ? { message: context.message.id } : {}),
      }),
    enabled: template.length > 0,
    placeholderData: keepPreviousData,
    retry: false,
  })
  const insert = (variable: string) => {
    const el = area.current
    const next = insertVariable(
      text,
      el?.selectionStart ?? text.length,
      el?.selectionEnd ?? text.length,
      variable,
    )
    onChange(next.value)
    requestAnimationFrame(() => {
      el?.focus()
      el?.setSelectionRange(next.caret, next.caret)
    })
  }
  const long = field.key === 'body' || field.key === 'note' || field.key === 'comment'
  const previewId = `${id}-preview`
  return (
    <div className="grid gap-2">
      <TextArea
        ref={area}
        id={id}
        value={text}
        rows={long ? 5 : 1}
        spellCheck={long}
        aria-describedby={previewId}
        className={cn('font-mono text-xs', !long && 'min-h-8 resize-none')}
        onChange={(next) => onChange(next === '' && !field.required ? undefined : next)}
      />
      <div role="group" aria-label={t.template.variables} className="-mx-1 flex flex-wrap gap-1">
        {templateVariables.map((variable) => (
          <button
            key={variable}
            type="button"
            aria-label={t.template.insert(variable)}
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => insert(variable)}
            className="h-6 px-1 font-mono text-[0.625rem] text-fg-subtle transition-colors duration-fast-02 ease-productive outline-none hover:bg-bg-hover hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus"
          >
            {variable}
          </button>
        ))}
      </div>
      {template && (
        <div id={previewId} aria-live="polite" className="grid gap-1 border-l border-border pl-2">
          <span className="text-[0.625rem] tracking-wide text-fg-subtle uppercase">
            {t.template.preview}
          </span>
          {preview.isError ? (
            <span className="text-xs text-danger">{t.template.previewFailed}</span>
          ) : preview.data?.error ? (
            <span className="text-xs text-danger">{preview.data.error}</span>
          ) : (
            <span className="text-xs whitespace-pre-wrap text-fg">
              {preview.data?.text === '' ? t.template.empty : (preview.data?.text ?? '…')}
            </span>
          )}
        </div>
      )}
    </div>
  )
}

function zones(): string[] {
  try {
    return Intl.supportedValuesOf('timeZone')
  } catch {
    return []
  }
}

/** One inspector input for a schema field. */
export function SchemaField({
  field,
  value,
  context,
  onChange,
}: {
  field: Field
  value: unknown
  context: FieldContext
  onChange: (value: unknown) => void
}) {
  const id = useId()
  const choices = useChoices(field, context.archiveId)
  const text = typeof value === 'string' ? value : ''
  const setText = (next: string) =>
    onChange(next === '' ? (field.nullable ? null : undefined) : next)

  let input: ReactNode
  switch (field.kind) {
    case 'template':
      input = (
        <TemplateField field={field} id={id} value={value} context={context} onChange={onChange} />
      )
      break
    case 'query':
      input = <QueryField id={id} value={value} archiveId={context.archiveId} onChange={onChange} />
      break
    case 'boolean':
      input = (
        <Switch
          id={id}
          checked={typeof value === 'boolean' ? value : field.default === true}
          onCheckedChange={(checked) => onChange(checked)}
        />
      )
      break
    case 'integer':
      input = (
        <input
          id={id}
          type="number"
          inputMode="numeric"
          className={cn(control, 'h-8 w-32 px-2')}
          value={typeof value === 'number' ? String(value) : ''}
          min={field.minimum}
          max={field.maximum}
          placeholder={field.default !== undefined ? shown(field.default) : undefined}
          onChange={(event) => onChange(readInteger(event.target.value))}
        />
      )
      break
    case 'choice':
      input = (
        <ChoiceField
          field={field}
          id={id}
          value={value}
          choices={field.options.map((o) => ({
            value: o.value,
            label: optionLabel(o.value, o.description),
          }))}
          onChange={onChange}
        />
      )
      break
    case 'choices':
      input = <ChoicesField field={field} value={value} onChange={onChange} />
      break
    case 'list':
      input = <ListField id={id} value={value} onChange={onChange} />
      break
    case 'time':
      input = (
        <input
          id={id}
          type="time"
          className={cn(control, 'h-8 w-32 px-2')}
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      )
      break
    case 'timezone':
      input = (
        <>
          <input
            id={id}
            list={`${id}-zones`}
            className={cn(control, 'h-8 min-w-0 px-2')}
            value={text}
            placeholder={shown(field.default)}
            spellCheck={false}
            onChange={(event) => setText(event.target.value)}
          />
          <datalist id={`${id}-zones`}>
            {zones().map((zone) => (
              <option key={zone} value={zone} />
            ))}
          </datalist>
        </>
      )
      break
    case 'folder':
      input = <FolderField id={id} archiveId={context.archiveId} value={text} onChange={setText} />
      break
    default:
      input = choices ? (
        <ChoiceField field={field} id={id} value={value} choices={choices} onChange={onChange} />
      ) : (
        <input
          id={id}
          type={field.kind === 'address' ? 'email' : 'text'}
          className={cn(
            control,
            'h-8 min-w-0 px-2',
            (field.kind === 'cron' || field.kind === 'identity') && 'font-mono text-xs',
          )}
          value={text}
          maxLength={field.maxLength}
          spellCheck={false}
          onChange={(event) => setText(event.target.value)}
        />
      )
  }
  return (
    <FieldShell field={field} id={id}>
      {input}
    </FieldShell>
  )
}

function FolderField({
  id,
  archiveId,
  value,
  onChange,
}: {
  id: string
  archiveId: string
  value: string
  onChange: (value: string) => void
}) {
  const folders = useQuery(foldersQuery(archiveId))
  const paths = [...new Set((folders.data ?? []).map((f) => f.path))].toSorted()
  return (
    <>
      <input
        id={id}
        list={`${id}-folders`}
        className={cn(control, 'h-8 min-w-0 px-2')}
        value={value}
        spellCheck={false}
        onChange={(event) => onChange(event.target.value)}
      />
      <datalist id={`${id}-folders`}>
        {paths.map((path) => (
          <option key={path} value={path} />
        ))}
      </datalist>
    </>
  )
}
