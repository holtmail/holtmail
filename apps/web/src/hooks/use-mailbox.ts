import {
  accountsQuery,
  changeMessage,
  dismissMailChange,
  foldersQuery,
  type MailChange,
  type MailChangeOp,
  type MessageDetail,
  restoreMessage,
  retryMailChange,
} from '@holtmail/api-client'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useCallback, useMemo } from 'react'

import { authErrorMessage } from '../lib/auth.ts'
import { type PlannedChange, writableByAccount } from '../lib/mailbox.ts'
import { overlay } from '../lib/overlay.ts'

const leaves = (op: MailChangeOp, recorded: readonly MailChange[]): string[] =>
  op.kind === 'move' || op.kind === 'delete' ? recorded.map((change) => change.folder_id) : []

/** The archive's accounts and folders with what the user may change on each server, and the
 * mutations that record mailbox changes. Every change refreshes the message and its thread, so
 * the held mark shows at once, and lands on list rows through the overlay. */
export function useMailbox(archiveId: string) {
  const queryClient = useQueryClient()
  const accounts = useQuery(accountsQuery(archiveId))
  const folders = useQuery(foldersQuery(archiveId))
  const writable = useMemo(() => writableByAccount(accounts.data ?? []), [accounts.data])

  const refresh = (messageId: string) =>
    Promise.all([
      queryClient.invalidateQueries({ queryKey: ['messages', messageId] }),
      queryClient.invalidateQueries({ queryKey: ['threads'] }),
    ])

  const apply = useMutation({
    mutationFn: async ({
      message,
      plan,
    }: {
      message: Pick<MessageDetail, 'id' | 'locations'>
      plan: PlannedChange[]
    }) => {
      return Promise.all(
        plan.map(async ({ account, op }) => {
          const result = await changeMessage(message.id, op, account)
          if (result.changes.length > 0) {
            overlay.record(message.id, op, leaves(op, result.changes))
          }
          return result
        }),
      )
    },
    onSettled: (_data, _error, { message }) => refresh(message.id),
  })

  const restore = useMutation({
    mutationFn: ({ messageId, folder }: { messageId: string; folder: string }) =>
      restoreMessage(messageId, folder),
    onSettled: (_data, _error, { messageId }) => refresh(messageId),
  })

  const retry = useMutation({
    mutationFn: ({ changeId }: { changeId: string; messageId: string }) =>
      retryMailChange(changeId),
    onSettled: (_data, _error, { messageId }) => refresh(messageId),
  })

  const dismiss = useMutation({
    mutationFn: ({ changeId }: { changeId: string; messageId: string }) =>
      dismissMailChange(changeId),
    onSettled: (_data, _error, { messageId }) => refresh(messageId),
  })

  const { reset: resetApply } = apply
  const { reset: resetRestore } = restore
  const { reset: resetRetry } = retry
  const { reset: resetDismiss } = dismiss
  const reset = useCallback(() => {
    resetApply()
    resetRestore()
    resetRetry()
    resetDismiss()
  }, [resetApply, resetRestore, resetRetry, resetDismiss])

  const error = [apply, restore, retry, dismiss].find((m) => m.isError)?.error
  return {
    accounts: accounts.data ?? [],
    folders: folders.data ?? [],
    writable,
    apply,
    restore,
    retry,
    dismiss,
    busy: apply.isPending || restore.isPending || retry.isPending || dismiss.isPending,
    error: error ? authErrorMessage(error) : undefined,
    reset,
  }
}
