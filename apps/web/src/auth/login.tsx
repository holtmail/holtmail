import { login } from '@holtmail/api-client'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate, useSearch } from '@tanstack/react-router'
import { type FormEvent, useRef } from 'react'

import { authErrorMessage, formText, safeNext } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { AuthScreen } from './auth-screen.tsx'
import { Field, FormError, PasswordField, SubmitButton } from './field.tsx'
import { signedIn } from './session.ts'

export function LoginScreen() {
  const { next } = useSearch({ from: '/login' })
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const password = useRef<HTMLInputElement>(null)
  const mutation = useMutation({
    mutationFn: login,
    onSuccess: async (user) => {
      signedIn(queryClient, user)
      await navigate({ href: safeNext(next), replace: true })
    },
    onError: () => {
      password.current?.focus()
      password.current?.select()
    },
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const form = new FormData(event.currentTarget)
    mutation.mutate({
      email: formText(form, 'email').trim(),
      password: formText(form, 'password'),
    })
  }

  const error = mutation.isError ? authErrorMessage(mutation.error) : undefined

  return (
    <AuthScreen title={strings.auth.login.title} lead={strings.auth.login.lead}>
      <form className="grid gap-6" onSubmit={submit}>
        <Field
          label={strings.auth.email}
          name="email"
          type="email"
          autoComplete="username"
          inputMode="email"
          autoCapitalize="none"
          spellCheck={false}
          required
          autoFocus
          aria-invalid={Boolean(error) || undefined}
        />
        <PasswordField
          ref={password}
          label={strings.auth.password}
          name="password"
          autoComplete="current-password"
          required
          aria-invalid={Boolean(error) || undefined}
        />
        <FormError message={error} />
        <SubmitButton
          busy={mutation.isPending}
          label={strings.auth.login.submit}
          busyLabel={strings.auth.login.busy}
        />
      </form>
    </AuthScreen>
  )
}
