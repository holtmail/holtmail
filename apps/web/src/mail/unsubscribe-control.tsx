import { type MessageDetail, unsubscribe } from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@holtmail/ui'
import { type QueryClient, useMutation, useQueryClient } from '@tanstack/react-query'
import { CircleAlertIcon, MailMinusIcon } from 'lucide-react'
import { useState } from 'react'

import { overrideSender } from '../lib/categories.ts'
import type { CardView } from '../lib/insights.ts'
import {
  linkHost,
  oneClickOffered,
  outcomeNote,
  requestedOnText,
  unsubscribeDomain,
  unsubscribeErrorText,
} from '../lib/unsubscribe.ts'
import { strings } from '../strings.ts'

const linkClass =
  'flex items-center gap-1 text-accent underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus'

async function refresh(queryClient: QueryClient, id: string) {
  await Promise.all([
    queryClient.invalidateQueries({ queryKey: ['messages', id], exact: true }),
    queryClient.invalidateQueries({ queryKey: ['threads'] }),
  ])
}

function PageLink({ href, host }: { href: string; host: string | undefined }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      referrerPolicy="no-referrer"
      className={linkClass}
    >
      <span>{strings.unsubscribe.openPage}</span>
      {host && (
        <>
          <span aria-hidden>→</span>
          <span className="font-mono break-all">{host}</span>
        </>
      )}
    </a>
  )
}

/** The footer's unsubscribe control: the one-click consent dialog when offered, else the link with
 * the reason in a tooltip, and the last request's date. */
export function UnsubscribeControl({
  message,
  view,
}: {
  message: MessageDetail
  view: CardView | undefined
}) {
  const t = strings.unsubscribe
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  const mutation = useMutation({
    mutationFn: () => unsubscribe(message.id),
    onSettled: () => refresh(queryClient, message.id),
  })
  const insights = message.insights
  const link = view?.unsubscribe
  const offered = oneClickOffered(insights)
  const lastRequest = insights?.unsubscribe?.last_request
  const domain = unsubscribeDomain(insights, overrideSender(message)) ?? ''
  const https = insights?.unsubscribe?.https ?? undefined
  const host = linkHost(https)
  if (!link && !offered && !lastRequest && !open) return null

  const note = mutation.data ? outcomeNote(mutation.data, domain) : undefined
  const error = mutation.isError ? unsubscribeErrorText(mutation.error) : undefined
  const settled = Boolean(note || error)

  const onOpenChange = (next: boolean) => {
    if (!next) mutation.reset()
    setOpen(next)
  }

  const anchor = link && (
    <a
      href={link.href}
      target="_blank"
      rel="noopener noreferrer"
      referrerPolicy="no-referrer"
      data-insight-unsubscribe={offered ? undefined : ''}
      className={linkClass}
    >
      <MailMinusIcon aria-hidden className="size-3" />
      {link.kind === 'mailto' ? strings.insights.unsubscribeMail : strings.insights.unsubscribe}
    </a>
  )

  return (
    <span className="flex flex-wrap items-center gap-2">
      {lastRequest && <span className="text-fg-subtle">{requestedOnText(lastRequest)}</span>}
      {offered ? (
        <Button variant="ghost" size="sm" data-insight-unsubscribe="" onClick={() => setOpen(true)}>
          <MailMinusIcon aria-hidden />
          {t.button}
        </Button>
      ) : (
        anchor &&
        (view?.reason ? (
          <Tooltip>
            <TooltipTrigger asChild>{anchor}</TooltipTrigger>
            <TooltipContent>{view.reason}</TooltipContent>
          </Tooltip>
        ) : (
          anchor
        ))
      )}
      <Dialog open={open} onOpenChange={onOpenChange}>
        <DialogContent>
          <form
            className="grid gap-6"
            onSubmit={(event) => {
              event.preventDefault()
              if (settled) onOpenChange(false)
              else if (!mutation.isPending) mutation.mutate()
            }}
          >
            <DialogHeader>
              <DialogTitle>{t.title}</DialogTitle>
              <DialogDescription className="wrap-break-word">{t.body(domain)}</DialogDescription>
            </DialogHeader>
            <div role="status" aria-live="polite" className="grid gap-2 text-sm">
              {note && (
                <p className={note.tone === 'success' ? 'text-success' : 'text-warning'}>
                  {note.text}
                </p>
              )}
              {error && (
                <p className="flex items-start gap-2 text-danger">
                  <CircleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
                  <span>{error}</span>
                </p>
              )}
              {(note?.offerLink || error) && https && <PageLink href={https} host={host} />}
            </div>
            <DialogFooter>
              {settled ? (
                <Button type="submit" variant="primary" autoFocus>
                  {t.close}
                </Button>
              ) : (
                <>
                  <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
                    {t.cancel}
                  </Button>
                  <Button
                    type="submit"
                    variant="primary"
                    disabled={mutation.isPending}
                    aria-busy={mutation.isPending}
                    autoFocus
                  >
                    {mutation.isPending ? t.sending : t.confirm}
                  </Button>
                </>
              )}
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </span>
  )
}
