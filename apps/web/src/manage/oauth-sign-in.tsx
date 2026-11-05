import {
  type Account,
  finishOAuth,
  type OAuthProvider,
  type OAuthStart,
  type OAuthStartRequest,
  startOAuth,
} from '@holtmail/api-client'
import {
  Button,
  cn,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  ArrowRightIcon,
  ArrowUpRightIcon,
  CircleAlertIcon,
  CircleCheckIcon,
  XIcon,
} from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { looksLikeSignInAnswer, type OAuthReturn, oauthProviderNames } from '../lib/oauth.ts'
import { strings } from '../strings.ts'

const t = strings.oauth

export interface OAuthSignIn {
  begin: (request: Omit<OAuthStartRequest, 'return_to'>) => void
  started: OAuthStart | undefined
  busy: boolean
  error: string | undefined
  finish: (pasted: string) => void
  finishing: boolean
  finishError: string | undefined
  reset: () => void
}

export function useOAuthSignIn(onConnected: (account: Account) => void): OAuthSignIn {
  const queryClient = useQueryClient()
  const [started, setStarted] = useState<OAuthStart>()
  const [leaving, setLeaving] = useState(false)

  const start = useMutation({
    mutationFn: startOAuth,
    onSuccess: (result) => {
      if (!result.needs_paste) {
        setLeaving(true)
        window.location.assign(result.authorize_url)
        return
      }
      window.open(result.authorize_url, '_blank', 'noopener')
      setStarted(result)
    },
  })
  const finish = useMutation({
    mutationFn: finishOAuth,
    onSuccess: async (account) => {
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
      onConnected(account)
    },
  })

  return {
    begin: (request) => start.mutate({ ...request, return_to: window.location.pathname }),
    started,
    busy: start.isPending || leaving,
    error: start.isError ? authErrorMessage(start.error) : undefined,
    finish: (pasted) => finish.mutate(pasted.trim()),
    finishing: finish.isPending,
    finishError: finish.isError ? authErrorMessage(finish.error) : undefined,
    reset: () => {
      setStarted(undefined)
      start.reset()
      finish.reset()
    },
  }
}

export function ThunderbirdNote() {
  return <p className="text-xs text-fg-subtle">{t.thunderbird}</p>
}

const formatTime = (time: number) =>
  new Intl.DateTimeFormat(undefined, { timeStyle: 'short' }).format(new Date(time))

export function OAuthPasteStep({
  flow,
  provider,
}: {
  flow: OAuthSignIn & { started: OAuthStart }
  provider: OAuthProvider
}) {
  const [pasted, setPasted] = useState('')
  const name = oauthProviderNames[provider]
  const ready = looksLikeSignInAnswer(pasted)

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (ready) flow.finish(pasted)
  }

  return (
    <form className="grid gap-6" onSubmit={submit} noValidate>
      <div className="grid gap-2 text-sm">
        <h3 className="text-base font-normal text-fg">{t.pasteTitle}</h3>
        <p className="text-fg-muted">
          {t.opened(name)}{' '}
          <a
            href={flow.started.authorize_url}
            target="_blank"
            rel="noreferrer"
            className="inline-flex items-center gap-1 text-accent hover:underline"
          >
            {t.openAgain}
            <ArrowUpRightIcon aria-hidden className="size-3" />
          </a>
        </p>
        <p className="text-fg-muted">{t.pasteSteps}</p>
      </div>
      <Field
        label={t.pasteLabel}
        name="pasted_url"
        autoComplete="off"
        autoCapitalize="none"
        spellCheck={false}
        autoFocus
        required
        value={pasted}
        onChange={(event) => setPasted(event.target.value)}
        aria-invalid={pasted.trim() !== '' && !ready}
        hint={
          <span className="grid gap-1">
            <span className={pasted.trim() !== '' && !ready ? 'text-warning' : undefined}>
              {pasted.trim() !== '' && !ready ? t.pasteWrong : t.pasteHint}
            </span>
            <span>{t.expires(formatTime(flow.started.expires_at))}</span>
          </span>
        }
      />
      {flow.started.thunderbird_client && <ThunderbirdNote />}
      <FormError message={flow.finishError} />
      <div className="flex items-center gap-2">
        <Button
          type="button"
          variant="ghost"
          className="h-12 px-4"
          disabled={flow.finishing}
          onClick={flow.reset}
        >
          {t.again}
        </Button>
        <Button
          type="submit"
          variant="primary"
          disabled={!ready || flow.finishing}
          aria-busy={flow.finishing}
          className="h-12 flex-1 justify-between px-4 text-sm"
        >
          {flow.finishing ? t.finishing : t.finish}
          <ArrowRightIcon />
        </Button>
      </div>
    </form>
  )
}

export function OAuthReturnNote({
  result,
  onDismiss,
}: {
  result: OAuthReturn
  onDismiss: () => void
}) {
  const failed = result.kind === 'error'
  const Icon = failed ? CircleAlertIcon : CircleCheckIcon
  return (
    <div
      role={failed ? 'alert' : 'status'}
      className={cn(
        'flex items-start gap-3 border px-4 py-3 text-sm',
        failed ? 'border-danger text-danger' : 'border-success text-success',
      )}
    >
      <Icon aria-hidden className="mt-0.5 size-4 shrink-0" />
      <p className="flex-1">{failed ? t.failed(result.message) : t.done}</p>
      <Button
        variant="ghost"
        size="icon"
        aria-label={t.dismiss}
        title={t.dismiss}
        className="-my-1.5 text-fg-muted"
        onClick={onDismiss}
      >
        <XIcon />
      </Button>
    </div>
  )
}

export function OAuthSignInDialog({
  account,
  provider,
  reason,
  open,
  onOpenChange,
}: {
  account: Account
  provider: OAuthProvider
  reason: 'again' | 'switch'
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const name = oauthProviderNames[provider]
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {reason === 'again' ? t.signInAgainTitle(account.name) : t.switchTo(name)}
          </DialogTitle>
          <DialogDescription>
            {reason === 'again' ? t.signInAgainLead(name) : t.switchLead(name)}
          </DialogDescription>
        </DialogHeader>
        {open && (
          <SignInAgain account={account} provider={provider} onDone={() => onOpenChange(false)} />
        )}
      </DialogContent>
    </Dialog>
  )
}

function SignInAgain({
  account,
  provider,
  onDone,
}: {
  account: Account
  provider: OAuthProvider
  onDone: () => void
}) {
  const flow = useOAuthSignIn(onDone)
  const name = oauthProviderNames[provider]
  const email = account.connection?.username ?? account.address ?? ''
  const { started } = flow
  if (started) return <OAuthPasteStep flow={{ ...flow, started }} provider={provider} />
  return (
    <div className="grid gap-6">
      {email && <p className="font-mono text-xs text-fg-subtle">{email}</p>}
      <ThunderbirdNote />
      <FormError message={flow.error} />
      <Button
        variant="primary"
        disabled={flow.busy}
        aria-busy={flow.busy}
        className="h-12 w-full justify-between px-4 text-sm"
        onClick={() => flow.begin({ provider, account_id: account.id, email })}
      >
        {flow.busy ? t.starting : t.signIn(name)}
        <ArrowRightIcon />
      </Button>
    </div>
  )
}
