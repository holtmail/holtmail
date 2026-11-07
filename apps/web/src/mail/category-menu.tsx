import {
  categoryOverridesQuery,
  deleteCategoryOverride,
  type MessageDetail,
  type OverrideScope,
  putCategoryOverride,
  type CategoryOverrideRequest,
  type Thread,
} from '@holtmail/api-client'
import {
  Button,
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@holtmail/ui'
import { type QueryClient, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ShapesIcon } from 'lucide-react'
import { useState } from 'react'

import {
  applyOverride,
  type Draft,
  heldBackText,
  initialDraft,
  kindLabel,
  kinds,
  overrideRequest,
  overrideSender,
  scopeKey,
  scopeText,
  senderOverrides,
  shownTopics,
  topicLabel,
} from '../lib/categories.ts'
import { strings } from '../strings.ts'

const t = strings.categories
const scopes: readonly OverrideScope[] = ['address', 'domain']
const keep = (event: Event) => event.preventDefault()

function patchMessage(message: MessageDetail, id: string, request: CategoryOverrideRequest) {
  return message.id === id && message.insights
    ? { ...message, insights: applyOverride(message.insights, request) }
    : message
}

function optimistic(queryClient: QueryClient, id: string, request: CategoryOverrideRequest) {
  queryClient.setQueryData<MessageDetail>(['messages', id], (message) =>
    message ? patchMessage(message, id, request) : message,
  )
  queryClient.setQueriesData<Thread>({ queryKey: ['threads'] }, (thread) =>
    thread?.messages.some((message) => message.id === id)
      ? { ...thread, messages: thread.messages.map((m) => patchMessage(m, id, request)) }
      : thread,
  )
}

async function refresh(queryClient: QueryClient, archiveId: string) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'messages'] }),
    queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'categories'] }),
    queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'category-overrides'] }),
    queryClient.invalidateQueries({
      queryKey: ['messages'],
      predicate: (query) => query.queryKey.length === 2,
    }),
    queryClient.invalidateQueries({ queryKey: ['threads'] }),
  ])
}

/** The chip menu: kinds, topics and the scope of an override for the message's sender. */
export function CategoryMenu({ message }: { message: MessageDetail }) {
  const archiveId = message.archive_id
  const sender = overrideSender(message)
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const [scope, setScope] = useState<OverrideScope>('address')
  const [initial, setInitial] = useState<Draft>(() => initialDraft(message.insights))
  const [draft, setDraft] = useState<Draft>(initial)
  const overrides = useQuery({ ...categoryOverridesQuery(archiveId), enabled: open })
  const existing = sender ? senderOverrides(overrides.data, sender) : undefined
  const replaced = existing?.[scope]

  const save = useMutation({
    mutationFn: (request: CategoryOverrideRequest) => putCategoryOverride(archiveId, request),
    onMutate: async (request) => {
      await queryClient.cancelQueries({ queryKey: ['messages', message.id], exact: true })
      const before = {
        message: queryClient.getQueryData<MessageDetail>(['messages', message.id]),
        threads: queryClient.getQueriesData<Thread>({ queryKey: ['threads'] }),
      }
      optimistic(queryClient, message.id, request)
      return before
    },
    onError: (_error, _request, before) => {
      if (!before) return
      queryClient.setQueryData(['messages', message.id], before.message)
      for (const [key, data] of before.threads) queryClient.setQueryData(key, data)
    },
    onSettled: () => refresh(queryClient, archiveId),
  })
  const reset = useMutation({
    mutationFn: (id: string) => deleteCategoryOverride(archiveId, id),
    onSettled: () => refresh(queryClient, archiveId),
  })

  if (!sender) return null
  const request = overrides.isSuccess
    ? overrideRequest(scope, scopeKey(scope, sender, existing), initial, draft, replaced)
    : undefined
  const held = heldBackText(message.insights?.topics)

  const onOpenChange = (next: boolean) => {
    if (next) {
      const start = initialDraft(message.insights)
      setInitial(start)
      setDraft(start)
      save.reset()
      reset.reset()
    }
    setOpen(next)
  }

  const toggleTopic = (topic: string, on: boolean) =>
    setDraft((previous) => ({
      ...previous,
      topics: on
        ? [...previous.topics.filter((item) => item !== topic), topic]
        : previous.topics.filter((item) => item !== topic),
    }))

  return (
    <>
      <DropdownMenu open={open} onOpenChange={onOpenChange}>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="sm"
            aria-label={t.change}
            title={t.change}
            data-insight-category=""
            onClick={(event) => {
              if (event.detail === 0) onOpenChange(true)
            }}
          >
            <ShapesIcon />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="w-72">
          <DropdownMenuLabel>{t.kind}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={draft.kind ?? ''}
            onValueChange={(kind) => setDraft((previous) => ({ ...previous, kind }))}
          >
            {kinds.map((kind) => (
              <DropdownMenuRadioItem key={kind} value={kind} onSelect={keep}>
                {kindLabel(kind)}
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          <DropdownMenuLabel>{t.topicsHeading}</DropdownMenuLabel>
          {shownTopics.map((topic) => (
            <DropdownMenuCheckboxItem
              key={topic}
              checked={draft.topics.includes(topic)}
              onCheckedChange={(on) => toggleTopic(topic, on)}
              onSelect={keep}
            >
              {topicLabel(topic)}
            </DropdownMenuCheckboxItem>
          ))}
          <DropdownMenuSeparator />
          <DropdownMenuLabel>{t.applyTo}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={scope}
            onValueChange={(value) => setScope(value === 'domain' ? 'domain' : 'address')}
          >
            {scopes.map((item) => (
              <DropdownMenuRadioItem key={item} value={item} onSelect={keep}>
                <span className="truncate font-mono text-xs">
                  {scopeText(item, sender, existing)}
                </span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          {held && (
            <>
              <DropdownMenuSeparator />
              <DropdownMenuLabel className="h-auto py-2 leading-4 text-warning">
                {held}
              </DropdownMenuLabel>
            </>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem disabled={!request} onSelect={() => request && save.mutate(request)}>
            {t.save}
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!replaced}
            title={t.resetHint(scopeText(scope, sender, existing))}
            onSelect={() => replaced && reset.mutate(replaced.id)}
          >
            {t.reset}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
      {(save.isError || reset.isError) && (
        <span role="alert" className="text-danger">
          {t.saveFailed}
        </span>
      )}
    </>
  )
}
