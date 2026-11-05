import { type Account, addJmapAccount } from '@holtmail/api-client'
import { Button, ToggleGroup, ToggleGroupItem } from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { type FormEvent, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import {
  isJmapAuth,
  type JmapForm,
  type JmapPreset,
  jmapMissing,
  jmapRequest,
} from '../lib/jmap.ts'
import { strings } from '../strings.ts'

const t = strings.jmap
const c = strings.connect
const presets: JmapPreset[] = ['fastmail', 'other']

const isPreset = (value: string): value is JmapPreset => value === 'fastmail' || value === 'other'

export function ConnectJmapForm({
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
  const queryClient = useQueryClient()
  const [form, setForm] = useState<JmapForm>({
    preset: 'fastmail',
    url: '',
    auth: 'password',
    username: '',
    secret: '',
    name: '',
  })
  const [missing, setMissing] = useState<string>()
  const set = (change: Partial<JmapForm>) => {
    setMissing(undefined)
    setForm((current) => ({ ...current, ...change }))
  }
  const fastmail = form.preset === 'fastmail'
  const token = fastmail || form.auth === 'token'

  const mutation = useMutation({
    mutationFn: () => addJmapAccount(archiveId, jmapRequest(form)),
    onSuccess: async (account) => {
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
      onConnected(account)
    },
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const empty = jmapMissing(form)
    if (empty) {
      setMissing(t.missing[empty])
      return
    }
    mutation.mutate()
  }

  return (
    <form className="grid gap-6" onSubmit={submit} noValidate>
      <div className="grid gap-2">
        <span className="text-xs text-fg-muted">{t.preset}</span>
        <ToggleGroup
          type="single"
          value={form.preset}
          onValueChange={(value) => isPreset(value) && set({ preset: value })}
          className="w-fit"
          aria-label={t.preset}
        >
          {presets.map((value) => (
            <ToggleGroupItem key={value} value={value}>
              {t.presets[value]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>
      {!fastmail && (
        <>
          <Field
            label={t.url}
            hint={t.urlHint}
            name="url"
            type="url"
            inputMode="url"
            autoCapitalize="none"
            spellCheck={false}
            placeholder="https://jmap.example.com"
            required
            value={form.url}
            onChange={(event) => set({ url: event.target.value })}
          />
          <div className="grid gap-2">
            <span className="text-xs text-fg-muted">{t.auth}</span>
            <ToggleGroup
              type="single"
              value={form.auth}
              onValueChange={(value) => isJmapAuth(value) && set({ auth: value })}
              className="w-fit"
              aria-label={t.auth}
            >
              <ToggleGroupItem value="password">{t.auths.password}</ToggleGroupItem>
              <ToggleGroupItem value="token">{t.auths.token}</ToggleGroupItem>
            </ToggleGroup>
          </div>
          {!token && (
            <Field
              label={c.username}
              hint={c.usernameHint}
              name="username"
              autoComplete="username"
              autoCapitalize="none"
              spellCheck={false}
              required
              value={form.username}
              onChange={(event) => set({ username: event.target.value })}
            />
          )}
        </>
      )}
      <PasswordField
        label={token ? t.token : c.password}
        name="secret"
        autoComplete="off"
        required
        value={form.secret}
        onChange={(event) => set({ secret: event.target.value })}
        hint={fastmail ? t.fastmailToken : token ? t.tokenHint : undefined}
      />
      <Field
        label={c.name}
        name="name"
        value={form.name}
        onChange={(event) => set({ name: event.target.value })}
        placeholder={fastmail ? t.presets.fastmail : form.username.trim() || undefined}
      />

      <FormError
        message={missing ?? (mutation.isError ? authErrorMessage(mutation.error) : undefined)}
      />
      <div className="flex items-center gap-2">
        {onCancel && (
          <Button type="button" variant="ghost" className="h-12 px-4" onClick={onCancel}>
            {c.cancel}
          </Button>
        )}
        <SubmitButton busy={mutation.isPending} label={c.submit} busyLabel={c.busy} />
      </div>
      {onUseImap && <SwitchLink label={t.useImap} onClick={onUseImap} />}
    </form>
  )
}

export function SwitchLink({ label, onClick }: { label: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="w-fit text-sm text-fg-muted underline-offset-4 outline-none hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-focus"
    >
      {label}
    </button>
  )
}
