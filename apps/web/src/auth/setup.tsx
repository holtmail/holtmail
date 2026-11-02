import { setup } from '@holtmail/api-client'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { CheckIcon } from 'lucide-react'
import { type FormEvent, useRef, useState } from 'react'

import { authErrorMessage, formText, minPasswordLength, passwordShortBy } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { AuthScreen } from './auth-screen.tsx'
import { Field, FormError, PasswordField, SubmitButton } from './field.tsx'
import { signedIn } from './session.ts'

export function SetupScreen() {
  const { token } = useSearch({ from: '/setup' })
  if (!token) {
    return (
      <AuthScreen
        title={strings.auth.setup.needsLinkTitle}
        lead={
          <div className="grid gap-4">
            <p>{strings.auth.setup.needsLink}</p>
            <p className="grid gap-2 border-l-2 border-accent pl-4">
              <span className="text-xs text-fg-subtle">{strings.auth.setup.needsLinkHint}</span>
              <code className="font-mono text-xs break-all text-fg">/setup?token=…</code>
            </p>
          </div>
        }
      />
    )
  }
  return <SetupForm token={token} />
}

function SetupForm({ token }: { token: string }) {
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const first = useRef<HTMLInputElement>(null)
  const [password, setPassword] = useState('')
  const shortBy = passwordShortBy(password)
  const mutation = useMutation({
    mutationFn: setup,
    onSuccess: async (user) => {
      signedIn(queryClient, user)
      await navigate({ to: '/', replace: true })
    },
    onError: () => first.current?.focus(),
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    mutation.mutate({
      token,
      email: formText(form, 'email').trim(),
      name: formText(form, 'name').trim(),
      password,
    })
  }

  const error = mutation.isError ? authErrorMessage(mutation.error) : undefined

  return (
    <AuthScreen title={strings.auth.setup.title} lead={strings.auth.setup.lead}>
      <form className="grid gap-6" onSubmit={submit}>
        <Field
          ref={first}
          label={strings.auth.email}
          name="email"
          type="email"
          autoComplete="username"
          inputMode="email"
          autoCapitalize="none"
          spellCheck={false}
          required
          autoFocus
        />
        <Field label={strings.auth.name} name="name" autoComplete="name" required />
        <PasswordField
          label={strings.auth.password}
          name="password"
          autoComplete="new-password"
          required
          minLength={minPasswordLength}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          hint={
            shortBy > 0 ? (
              strings.auth.setup.passwordShort(shortBy)
            ) : (
              <span className="flex items-center gap-1 text-success">
                <CheckIcon aria-hidden className="size-3" />
                {strings.auth.setup.passwordOk}
              </span>
            )
          }
        />
        <FormError message={error} />
        <SubmitButton
          busy={mutation.isPending}
          label={strings.auth.setup.submit}
          busyLabel={strings.auth.setup.busy}
        />
      </form>
    </AuthScreen>
  )
}
