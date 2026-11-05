import type { Account } from '@holtmail/api-client'
import { Button } from '@holtmail/ui'
import { type FormEvent, useState } from 'react'

import { Field, FormError, SubmitButton } from '../auth/field.tsx'
import { graphSignIn, oauthProviderNames } from '../lib/oauth.ts'
import { strings } from '../strings.ts'
import { SwitchLink } from './connect-jmap.tsx'
import { OAuthPasteStep, ThunderbirdNote, useOAuthSignIn } from './oauth-sign-in.tsx'

const t = strings.graph
const c = strings.connect

export function ConnectGraphForm({
  archiveId,
  onConnected,
  onCancel,
  onUseImap,
}: {
  archiveId: string
  onConnected: (account: Account) => void
  onCancel?: () => void
  onUseImap?: () => void
}) {
  const [address, setAddress] = useState('')
  const [name, setName] = useState('')
  const [missing, setMissing] = useState<string>()
  const flow = useOAuthSignIn(onConnected)

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const request = graphSignIn(archiveId, address, name)
    if (!request) {
      setMissing(t.missing)
      return
    }
    flow.begin(request)
  }

  const { started } = flow
  if (started) return <OAuthPasteStep flow={{ ...flow, started }} provider="microsoft" />

  return (
    <form className="grid gap-6" onSubmit={submit} noValidate>
      <Field
        label={c.email}
        type="email"
        name="email"
        autoComplete="email"
        inputMode="email"
        autoCapitalize="none"
        spellCheck={false}
        required
        autoFocus
        value={address}
        onChange={(event) => {
          setMissing(undefined)
          setAddress(event.target.value)
        }}
        hint={t.lead}
      />
      <Field
        label={c.name}
        name="name"
        value={name}
        onChange={(event) => setName(event.target.value)}
        placeholder={address.trim() || undefined}
      />
      <ThunderbirdNote />
      <FormError message={missing ?? flow.error} />
      <div className="flex items-center gap-2">
        {onCancel && (
          <Button type="button" variant="ghost" className="h-12 px-4" onClick={onCancel}>
            {c.cancel}
          </Button>
        )}
        <SubmitButton
          busy={flow.busy}
          label={strings.oauth.signIn(oauthProviderNames.microsoft)}
          busyLabel={strings.oauth.starting}
        />
      </div>
      {onUseImap && <SwitchLink label={t.useImap} onClick={onUseImap} />}
    </form>
  )
}
