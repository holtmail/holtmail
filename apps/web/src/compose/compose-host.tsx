import {
  archivesQuery,
  createDraft,
  type Draft,
  draftQuery,
  forwardMessage,
  type ForwardKind,
  type MessageDetail,
  type Outbox,
  replyToMessage,
  type ReplyKind,
} from '@holtmail/api-client'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  createContext,
  lazy,
  type ReactNode,
  Suspense,
  use,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from 'react'

import type { Layout } from '../hooks/use-layout.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { canCompose, editAsNewPayload } from '../lib/writing.ts'
import { strings } from '../strings.ts'
import { UndoToast } from './undo-toast.tsx'

const ComposePane = lazy(async () => ({
  default: (await import('./compose-pane.tsx')).ComposePane,
}))

export interface ComposeController {
  /** Whether the user may compose in the shown archive. */
  available: boolean
  compose: () => void
  openDraft: (id: string) => void
  reply: (messageId: string, kind: ReplyKind) => void
  forward: (messageId: string, kind: ForwardKind) => void
  editAsNew: (message: MessageDetail) => void
}

const noop = () => undefined

const unavailable: ComposeController = {
  available: false,
  compose: noop,
  openDraft: noop,
  reply: noop,
  forward: noop,
  editAsNew: noop,
}

export const ComposeContext = createContext<ComposeController>(unavailable)

export const useCompose = (): ComposeController => use(ComposeContext)

/** Owns the compose pane and the undo toast of one archive: which draft is open, starting
 * replies and forwards, and what was just sent. */
export function useComposeHost(
  archiveId: string,
  layout: Layout,
): { controller: ComposeController; element: ReactNode } {
  const queryClient = useQueryClient()
  const archive = useQuery(archivesQuery).data?.find((a) => a.id === archiveId)
  const available = canCompose(archive)
  const [current, setCurrent] = useState<{ id: string; archive: string; fresh: boolean }>()
  const [problem, setProblem] = useState<string>()
  const [sent, setSent] = useState<Outbox>()
  const draftId = current?.archive === archiveId ? current.id : undefined

  useEffect(() => {
    setCurrent((open) => (open?.archive === archiveId ? open : undefined))
    setSent(undefined)
    setProblem(undefined)
  }, [archiveId])

  const open = useCallback(
    (draft: Draft, fresh: boolean) => {
      queryClient.setQueryData(draftQuery(draft.id).queryKey, draft)
      setProblem(undefined)
      setCurrent({ id: draft.id, archive: draft.archive, fresh })
    },
    [queryClient],
  )

  const failed = useCallback(
    (error: unknown) => setProblem(strings.writing.reader.failed(authErrorMessage(error))),
    [],
  )

  const start = useCallback(
    (make: () => Promise<Draft>) => {
      make().then((draft) => open(draft, true), failed)
    },
    [open, failed],
  )

  const openDraft = useCallback(
    (id: string) => {
      queryClient
        .fetchQuery({ ...draftQuery(id), staleTime: 0 })
        .then((draft) => open(draft, false), failed)
    },
    [queryClient, open, failed],
  )

  const dismiss = useCallback(() => {
    setSent(undefined)
    setProblem(undefined)
  }, [])

  const controller = useMemo<ComposeController>(
    () =>
      available
        ? {
            available,
            compose: () => start(() => createDraft(archiveId, {})),
            openDraft,
            reply: (messageId, kind) => start(() => replyToMessage(messageId, kind)),
            forward: (messageId, kind) => start(() => forwardMessage(messageId, kind)),
            editAsNew: (message) => start(() => createDraft(archiveId, editAsNewPayload(message))),
          }
        : unavailable,
    [available, archiveId, start, openDraft],
  )

  const element = (
    <>
      {draftId && current && (
        <Suspense>
          <ComposePane
            key={draftId}
            draftId={draftId}
            fresh={current.fresh}
            archiveId={archiveId}
            layout={layout}
            onClose={() => setCurrent(undefined)}
            onSent={(entry) => {
              setCurrent(undefined)
              setSent(entry)
              void queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'outbox'] })
              void queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'drafts'] })
            }}
          />
        </Suspense>
      )}
      {(sent || problem) && (
        <UndoToast
          key={sent?.id ?? 'problem'}
          archiveId={archiveId}
          entry={sent}
          problem={problem}
          onUndone={(draft) => {
            setSent(undefined)
            open(draft, false)
          }}
          onDismiss={dismiss}
        />
      )}
    </>
  )

  return { controller, element }
}
