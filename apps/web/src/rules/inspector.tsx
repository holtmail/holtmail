import type { FlowIssue } from '@holtmail/api-client'
import { Button, Switch, ToggleGroup, ToggleGroupItem } from '@holtmail/ui'
import { Trash2Icon } from 'lucide-react'
import { useId, useState } from 'react'

import { NativeSelect } from '../items/inputs.tsx'
import {
  docsOf,
  fieldsOf,
  type FlowDefinition,
  type NodeType,
  nodeTypeOf,
  paramsOf,
  triggers,
  TRIGGER_ID,
  withParam,
} from '../lib/rules.ts'
import type { Locale } from '../strings-writing.ts'
import { currentLocale } from '../strings-writing.ts'
import { strings } from '../strings.ts'
import { type FieldContext, SchemaField } from './fields.tsx'

const t = strings.rules
const locale = currentLocale()

type Tab = 'settings' | 'docs'

/** A node type's documentation from the catalogue in both languages, the viewer's first. */
function DocsPanel({ type }: { type: NodeType }) {
  const order: Locale[] = locale === 'ru' ? ['ru', 'en'] : ['en', 'ru']
  const catalogueTriggers = type.triggers
  return (
    <div className="grid gap-4">
      {order.map((lang) => {
        const text = type.docs[lang]
        if (!text.title && !text.summary) return null
        return (
          <section key={lang} lang={lang} className="grid gap-1">
            <h3 className="text-sm text-fg">{text.title}</h3>
            <p className="text-sm text-fg-muted">{text.summary}</p>
          </section>
        )
      })}
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 border-t border-border pt-3 text-xs">
        <dt className="text-fg-subtle">{t.inspector.id}</dt>
        <dd className="font-mono text-fg-muted">
          {type.id} v{type.version}
        </dd>
        {type.role !== 'trigger' && (
          <>
            <dt className="text-fg-subtle">{t.inspector.ports}</dt>
            <dd className="text-fg-muted">
              {type.ports.map((p) => t.inspector.portLabels[p] ?? p).join(', ') || '—'}
            </dd>
          </>
        )}
        {type.role === 'action' && (
          <>
            <dt className="sr-only">{t.inspector.needs('')}</dt>
            <dd className="col-span-2 text-fg-muted">
              {type.capability
                ? t.inspector.needs(t.capabilities[type.capability] ?? type.capability)
                : t.inspector.local}
            </dd>
          </>
        )}
        {type.templates.length > 0 && (
          <>
            <dt className="text-fg-subtle">{t.inspector.templates}</dt>
            <dd className="font-mono text-fg-muted">{type.templates.join(', ')}</dd>
          </>
        )}
        {type.role !== 'trigger' && catalogueTriggers.length > 0 && (
          <>
            <dt className="text-fg-subtle">{t.inspector.follows}</dt>
            <dd className="font-mono text-fg-muted">{catalogueTriggers.join(', ')}</dd>
          </>
        )}
      </dl>
    </div>
  )
}

export function Inspector({
  flow,
  catalogue,
  selected,
  issues,
  context,
  onParams,
  onTrigger,
  onConfirm,
  onRemove,
}: {
  flow: FlowDefinition
  catalogue: NodeType[]
  selected: string | null
  issues: FlowIssue[]
  context: FieldContext
  onParams: (id: string, params: Record<string, unknown>) => void
  onTrigger: (type: NodeType) => void
  onConfirm: (id: string, confirm: boolean) => void
  onRemove: (id: string) => void
}) {
  const [tab, setTab] = useState<Tab>('settings')
  const confirmId = useId()
  if (selected === null) {
    return <p className="p-3 text-sm text-fg-muted">{t.inspector.nothing}</p>
  }
  const isTrigger = selected === TRIGGER_ID
  const node = isTrigger ? null : flow.nodes.find((n) => n.id === selected)
  if (!isTrigger && !node) return <p className="p-3 text-sm text-fg-muted">{t.inspector.nothing}</p>
  const typeId = isTrigger ? flow.trigger.type : (node?.type ?? '')
  const type = nodeTypeOf(catalogue, typeId)
  const known = type !== undefined && (type.role === 'trigger') === isTrigger
  const params = paramsOf(flow, selected)
  const text = docsOf(type, locale, typeId)

  return (
    <section aria-label={t.inspector.title} className="grid content-start gap-4 p-3">
      <header className="grid gap-1">
        <span className="font-mono text-[0.625rem] tracking-wide text-fg-subtle uppercase">
          {isTrigger ? t.inspector.trigger : `${t.inspector.id}: ${selected}`}
        </span>
        <h2 className="text-base text-fg">{text.title}</h2>
      </header>
      {issues.length > 0 && (
        <ul role="alert" className="grid gap-1 border-l border-danger pl-2 text-xs text-danger">
          {issues.map((issue, index) => (
            <li key={`${issue.code}-${index}`}>
              <span className="font-mono">{issue.code}</span> {issue.detail}
            </li>
          ))}
        </ul>
      )}
      {!type || !known ? (
        <p className="text-sm text-danger">{t.inspector.unknown(typeId)}</p>
      ) : (
        <>
          <ToggleGroup
            type="single"
            value={tab}
            onValueChange={(next) => {
              if (next === 'settings' || next === 'docs') setTab(next)
            }}
            aria-label={t.inspector.title}
          >
            <ToggleGroupItem value="settings">{t.inspector.tabs.settings}</ToggleGroupItem>
            <ToggleGroupItem value="docs">{t.inspector.tabs.docs}</ToggleGroupItem>
          </ToggleGroup>
          {tab === 'docs' ? (
            <DocsPanel type={type} />
          ) : (
            <div className="grid gap-4">
              {isTrigger && (
                <NativeSelect
                  label={t.inspector.trigger}
                  value={flow.trigger.type}
                  choices={triggers(catalogue).map((c) => ({
                    value: c.id,
                    label: docsOf(c, locale).title,
                  }))}
                  onChange={(id) => {
                    const next = nodeTypeOf(catalogue, id)
                    if (next) onTrigger(next)
                  }}
                />
              )}
              <p className="text-xs text-fg-subtle">{text.summary}</p>
              {fieldsOf(type.schema).map((field) => (
                <SchemaField
                  key={`${selected}:${field.key}`}
                  field={field}
                  value={params[field.key]}
                  context={context}
                  onChange={(value) => onParams(selected, withParam(params, field.key, value))}
                />
              ))}
              {type.confirmable && node && (
                <div className="grid gap-1">
                  <div className="flex items-center gap-3">
                    <Switch
                      id={confirmId}
                      checked={node.confirm === true}
                      onCheckedChange={(checked) => onConfirm(selected, checked)}
                    />
                    <label htmlFor={confirmId} className="text-sm text-fg">
                      {t.inspector.confirm}
                    </label>
                  </div>
                  <p className="text-xs text-fg-subtle">{t.inspector.confirmLead}</p>
                </div>
              )}
              {type.role === 'action' && (
                <p className="text-xs text-fg-subtle">
                  {type.capability
                    ? t.inspector.needs(t.capabilities[type.capability] ?? type.capability)
                    : t.inspector.local}
                </p>
              )}
            </div>
          )}
        </>
      )}
      {!isTrigger && (
        <Button
          variant="ghost"
          size="sm"
          className="justify-self-start"
          onClick={() => onRemove(selected)}
        >
          <Trash2Icon />
          {t.inspector.remove}
        </Button>
      )}
    </section>
  )
}
