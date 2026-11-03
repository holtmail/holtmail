import { changePassword, sessionQuery, updateProfile, type User } from '@holtmail/api-client'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { CheckIcon } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage, minPasswordLength, passwordShortBy } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings

export function ProfilePage() {
  const user = useQuery(sessionQuery).data
  return (
    <SettingsPage title={t.profile} lead={t.profileLead}>
      {user && <ProfileForm key={user.id} user={user} />}
      <hr className="border-border" />
      <PasswordForm />
    </SettingsPage>
  )
}

function Saved({ show, message = t.saved }: { show: boolean; message?: string }) {
  return (
    <p role="status" className="flex min-h-5 items-center gap-1 text-sm text-success">
      {show && (
        <>
          <CheckIcon aria-hidden className="size-4" />
          {message}
        </>
      )}
    </p>
  )
}

function ProfileForm({ user }: { user: User }) {
  const queryClient = useQueryClient()
  const [name, setName] = useState(user.name)
  const [email, setEmail] = useState(user.email)
  const mutation = useMutation({
    mutationFn: () => updateProfile({ name: name.trim(), email: email.trim() }),
    onSuccess: (updated) => {
      queryClient.setQueryData(sessionQuery.queryKey, updated)
      void queryClient.invalidateQueries({ queryKey: ['users'] })
    },
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate()
  }
  return (
    <form className="grid max-w-md gap-6" onSubmit={submit}>
      <Field
        label={strings.auth.name}
        autoComplete="name"
        required
        value={name}
        onChange={(event) => setName(event.target.value)}
      />
      <Field
        label={strings.auth.email}
        type="email"
        autoComplete="username"
        autoCapitalize="none"
        spellCheck={false}
        required
        value={email}
        onChange={(event) => setEmail(event.target.value)}
      />
      <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
      <Saved show={mutation.isSuccess} />
      <SubmitButton
        busy={mutation.isPending}
        label={strings.accounts.save}
        busyLabel={strings.accounts.saving}
      />
    </form>
  )
}

function PasswordForm() {
  const queryClient = useQueryClient()
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const shortBy = passwordShortBy(next)
  const mutation = useMutation({
    mutationFn: () => changePassword({ current, new: next }),
    onSuccess: (user) => {
      queryClient.setQueryData(sessionQuery.queryKey, user)
      setCurrent('')
      setNext('')
    },
  })
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate()
  }
  return (
    <form className="grid max-w-md gap-6" onSubmit={submit}>
      <h3 className="text-lg font-normal">{t.changePassword}</h3>
      <PasswordField
        label={t.currentPassword}
        autoComplete="current-password"
        required
        value={current}
        onChange={(event) => setCurrent(event.target.value)}
      />
      <PasswordField
        label={t.newPassword}
        autoComplete="new-password"
        required
        minLength={minPasswordLength}
        value={next}
        onChange={(event) => setNext(event.target.value)}
        hint={
          next
            ? shortBy > 0
              ? strings.auth.setup.passwordShort(shortBy)
              : strings.auth.setup.passwordOk
            : undefined
        }
      />
      <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
      <Saved show={mutation.isSuccess} message={t.passwordChanged} />
      <SubmitButton
        busy={mutation.isPending}
        label={t.changePassword}
        busyLabel={strings.accounts.saving}
      />
    </form>
  )
}
