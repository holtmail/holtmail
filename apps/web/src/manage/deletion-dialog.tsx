import {
  confirmDeletion,
  type DeletionRequest,
  type DeletionReview,
  reviewDeletion,
} from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Skeleton,
} from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { type FormEvent, useEffect, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { formatSize } from '../lib/format.ts'
import { strings } from '../strings.ts'

const t = strings.deletion

const day = (time: number) =>
  new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(new Date(time))

function Summary({ review }: { review: DeletionReview }) {
  const span =
    review.oldest != null && review.newest != null
      ? t.span(day(review.oldest), day(review.newest))
      : ''
  const hidden = review.count - review.sample.length
  return (
    <div className="grid gap-3 text-sm">
      <p className="text-fg">
        {t.summary(review.count, formatSize(review.bytes))}
        {span && <span className="text-fg-muted"> · {span}</span>}
      </p>
      <p className="text-fg-muted">{t.lead}</p>
      <div className="grid gap-1">
        <p className="text-xs text-fg-subtle">{t.sample}</p>
        <ul className="max-h-48 overflow-y-auto border border-border">
          {review.sample.map((message) => (
            <li
              key={message.id}
              className="flex gap-3 border-b border-border px-3 py-1.5 last:border-b-0"
            >
              <span className="w-24 shrink-0 text-fg-subtle tabular-nums">{day(message.date)}</span>
              <span className="truncate text-fg">
                {message.subject?.trim() || strings.list.noSubject}
              </span>
            </li>
          ))}
        </ul>
        {hidden > 0 && <p className="text-xs text-fg-subtle">{t.more(hidden)}</p>}
      </div>
    </div>
  )
}

export function DeletionDialog({
  archiveId,
  request,
  open,
  onOpenChange,
  onDeleted,
}: {
  archiveId: string
  request: DeletionRequest
  open: boolean
  onOpenChange: (open: boolean) => void
  onDeleted?: () => void
}) {
  const queryClient = useQueryClient()
  const [typed, setTyped] = useState('')
  const review = useMutation({ mutationFn: () => reviewDeletion(archiveId, request) })
  const confirm = useMutation({
    mutationFn: (current: DeletionReview) => confirmDeletion(current.id, current.count),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['tasks'] })
      void queryClient.invalidateQueries({ queryKey: ['archives'] })
      onOpenChange(false)
      onDeleted?.()
    },
  })
  const { mutate: startReview, reset: resetReview } = review
  const { reset: resetConfirm } = confirm

  useEffect(() => {
    if (open) startReview()
    else {
      setTyped('')
      resetReview()
      resetConfirm()
    }
  }, [open, startReview, resetReview, resetConfirm])

  const current = review.data
  const ready = current !== undefined && typed.trim() === String(current.count)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (current && ready) confirm.mutate(current)
  }
  const error = review.isError ? review.error : confirm.isError ? confirm.error : undefined

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.title}</DialogTitle>
            <DialogDescription>{current ? '' : t.reviewing}</DialogDescription>
          </DialogHeader>
          {review.isPending && <Skeleton className="h-32" />}
          {current && (
            <>
              <Summary review={current} />
              <Field
                label={t.type(current.count)}
                value={typed}
                inputMode="numeric"
                onChange={(event) => setTyped(event.target.value)}
                autoComplete="off"
                spellCheck={false}
                autoFocus
              />
            </>
          )}
          <FormError message={error ? authErrorMessage(error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {t.cancel}
            </Button>
            <Button
              type="submit"
              variant="danger"
              disabled={!ready || confirm.isPending}
              aria-busy={confirm.isPending}
            >
              {t.confirm}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
