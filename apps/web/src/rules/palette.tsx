import { cn, Input } from '@holtmail/ui'
import { PlusIcon, SearchIcon } from 'lucide-react'
import { useId, useState } from 'react'

import { docsOf, mayUseType, type NodeType, nodeTypeOf, palette } from '../lib/rules.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import { STEP_DRAG } from './canvas.tsx'

const t = strings.rules.palette
const locale = currentLocale()

/** The steps that may follow the trigger; those the archive grant `role` may not save (sending
 * without a `sender` grant) are shown disabled with the reason. */
export function Palette({
  catalogue,
  trigger,
  role,
  onAdd,
}: {
  catalogue: NodeType[]
  trigger: string
  role: string
  onAdd: (type: NodeType) => void
}) {
  const [search, setSearch] = useState('')
  const searchId = useId()
  const groups = palette(catalogue, trigger, search, locale)
  const triggerTitle = docsOf(nodeTypeOf(catalogue, trigger), locale, trigger).title
  return (
    <section aria-labelledby={`${searchId}-title`} className="grid content-start gap-3 p-3">
      <h2 id={`${searchId}-title`} className="text-xs text-fg-muted">
        {t.title}
      </h2>
      <div className="relative">
        <SearchIcon
          aria-hidden
          className="pointer-events-none absolute top-1/2 left-2 size-4 -translate-y-1/2 text-fg-subtle"
        />
        <Input
          type="search"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
          placeholder={t.search}
          aria-label={t.search}
          aria-describedby={`${searchId}-lead`}
          className="pl-8"
        />
      </div>
      <p id={`${searchId}-lead`} className="text-xs text-fg-subtle">
        {t.triggerOnly(triggerTitle)}
      </p>
      {groups.length === 0 && <p className="text-sm text-fg-muted">{t.empty}</p>}
      {groups.map((group) => (
        <div key={group.role} className="grid gap-1">
          <h3 className="font-mono text-[0.625rem] tracking-wide text-fg-subtle uppercase">
            {group.role === 'condition' ? t.conditions : t.actions}
          </h3>
          <ul className="grid">
            {group.types.map((type) => {
              const text = docsOf(type, locale)
              const allowed = mayUseType(type, role)
              const reasonId = `${searchId}-${type.id}-reason`
              return (
                <li key={type.id}>
                  <button
                    type="button"
                    draggable={allowed}
                    disabled={!allowed}
                    onDragStart={(event) => {
                      event.dataTransfer.setData(STEP_DRAG, type.id)
                      event.dataTransfer.effectAllowed = 'copy'
                    }}
                    onClick={() => onAdd(type)}
                    aria-label={t.add(text.title)}
                    aria-describedby={allowed ? undefined : reasonId}
                    title={allowed ? text.summary : t.needsSender}
                    className={cn(
                      'group flex w-full items-center gap-2 border border-transparent px-2 py-1.5 text-left text-sm text-fg',
                      'transition-colors duration-fast-02 ease-productive hover:border-border hover:bg-bg-hover',
                      'outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus',
                      'disabled:cursor-not-allowed disabled:text-fg-subtle disabled:hover:border-transparent disabled:hover:bg-transparent',
                    )}
                  >
                    <span className="min-w-0 flex-1 truncate">{text.title}</span>
                    {!allowed && (
                      <span id={reasonId} className="sr-only">
                        {t.needsSender}
                      </span>
                    )}
                    <PlusIcon
                      aria-hidden
                      className="size-3 shrink-0 text-fg-subtle opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100"
                    />
                  </button>
                </li>
              )
            })}
          </ul>
        </div>
      ))}
    </section>
  )
}
