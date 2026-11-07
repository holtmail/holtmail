import {
  archivesQuery,
  type CategoryOverride,
  categoryOverridesQuery,
  deleteCategoryOverride,
} from '@holtmail/api-client'
import { Badge, Button, Skeleton } from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Trash2Icon } from 'lucide-react'
import { useState } from 'react'

import { overrideSummary } from '../lib/categories.ts'
import { formatFullDate } from '../lib/format.ts'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { Picker } from './aliases-page.tsx'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.categories

function OverrideRow({ item, archiveId }: { item: CategoryOverride; archiveId: string }) {
  const queryClient = useQueryClient()
  const [removing, setRemoving] = useState(false)
  const summary = overrideSummary(item)
  return (
    <li className="flex flex-wrap items-center gap-x-4 gap-y-2 py-3">
      <div className="grid min-w-0 flex-1 gap-1">
        <span className="flex min-w-0 items-baseline gap-2">
          <Badge variant="outline">{summary.scope}</Badge>
          <span className="truncate font-mono text-sm text-fg">{item.key}</span>
        </span>
        <span className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-fg-muted">
          <span>
            <span className="text-fg-subtle">{t.kind}: </span>
            {summary.kind}
          </span>
          <span>
            <span className="text-fg-subtle">{t.topicsHeading}: </span>
            {summary.topics}
          </span>
          <span className="text-fg-subtle">{t.created(formatFullDate(item.created_at))}</span>
        </span>
      </div>
      <Button
        variant="ghost"
        size="sm"
        aria-label={t.removeTitle(item.key)}
        onClick={() => setRemoving(true)}
      >
        <Trash2Icon />
        {t.remove}
      </Button>
      <ConfirmDialog
        open={removing}
        onOpenChange={setRemoving}
        title={t.removeTitle(item.key)}
        description={t.removeLead}
        action={t.remove}
        busyAction={t.deleting}
        onConfirm={async () => {
          await deleteCategoryOverride(archiveId, item.id)
          await Promise.all([
            queryClient.invalidateQueries({ queryKey: ['archives', archiveId] }),
            queryClient.invalidateQueries({
              queryKey: ['messages'],
              predicate: (query) => query.queryKey.length === 2,
            }),
            queryClient.invalidateQueries({ queryKey: ['threads'] }),
          ])
        }}
      />
    </li>
  )
}

function ArchiveOverrides({ archiveId }: { archiveId: string }) {
  const overrides = useQuery(categoryOverridesQuery(archiveId))
  if (overrides.isPending) return <Skeleton className="h-32" />
  if (overrides.isError) {
    return (
      <p role="alert" className="text-sm text-danger">
        {overrides.error.message}
      </p>
    )
  }
  if (overrides.data.length === 0) return <p className="text-sm text-fg-muted">{t.empty}</p>
  return (
    <section className="grid gap-2" aria-label={t.overrideCount(overrides.data.length)}>
      <p className="text-xs text-fg-subtle">{t.overrideCount(overrides.data.length)}</p>
      <ul className="grid divide-y divide-border border-y border-border">
        {overrides.data.map((item) => (
          <OverrideRow key={item.id} item={item} archiveId={archiveId} />
        ))}
      </ul>
    </section>
  )
}

export function CategoriesPage() {
  const archives = useQuery(archivesQuery)
  const [chosen, setChosen] = useState<string>()
  const list = archives.data ?? []
  const archiveId = chosen ?? list[0]?.id
  return (
    <SettingsPage
      title={strings.settings.categories}
      lead={strings.settings.categoriesLead}
      action={
        list.length > 1 && archiveId ? (
          <Picker
            label={strings.aliases.archive}
            value={archiveId}
            choices={list.map((a) => ({ value: a.id, label: a.name }))}
            onChange={setChosen}
          />
        ) : undefined
      }
    >
      {archives.isPending ? (
        <Skeleton className="h-48" />
      ) : archiveId ? (
        <ArchiveOverrides key={archiveId} archiveId={archiveId} />
      ) : (
        <p className="text-sm text-fg-muted">{t.noArchives}</p>
      )}
    </SettingsPage>
  )
}
