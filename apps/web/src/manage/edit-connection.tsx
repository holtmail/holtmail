import { type Account, updateAccount } from '@holtmail/api-client'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  ToggleGroup,
  ToggleGroupItem,
} from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { TriangleAlertIcon } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { defaultPort, type Security } from '../lib/providers.ts'
import { parseExcluded } from '../lib/schedule.ts'
import { strings } from '../strings.ts'

const t = strings.connect
const a = strings.accounts
const securities: Security[] = ['tls', 'starttls', 'plain']

const isSecurity = (value: string): value is Security =>
  value === 'tls' || value === 'starttls' || value === 'plain'

export function EditConnectionDialog({
  account,
  open,
  onOpenChange,
}: {
  account: Account
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{a.editTitle}</DialogTitle>
          <DialogDescription>{a.editLead}</DialogDescription>
        </DialogHeader>
        {open && <EditConnectionForm account={account} onDone={() => onOpenChange(false)} />}
      </DialogContent>
    </Dialog>
  )
}

function EditConnectionForm({ account, onDone }: { account: Account; onDone: () => void }) {
  const queryClient = useQueryClient()
  const current = account.connection
  const startSecurity = current && isSecurity(current.security) ? current.security : 'tls'
  const [host, setHost] = useState(current?.host ?? '')
  const [security, setSecurity] = useState<Security>(startSecurity)
  const [port, setPort] = useState(
    current && current.port !== defaultPort(startSecurity) ? String(current.port) : '',
  )
  const [username, setUsername] = useState(current?.username ?? '')
  const [excluded, setExcluded] = useState(current?.exclude.join(', ') ?? '')
  const [password, setPassword] = useState('')

  const mutation = useMutation({
    mutationFn: () =>
      updateAccount(account.id, {
        connection: {
          host: host.trim(),
          port: Number(port) || null,
          security,
          username: username.trim(),
          exclude: parseExcluded(excluded),
        },
        password: password || null,
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
      onDone()
    },
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate()
  }

  return (
    <form className="grid gap-6" onSubmit={submit}>
      <div className="grid grid-cols-[1fr_6rem] gap-4">
        <Field
          label={t.host}
          value={host}
          required
          autoCapitalize="none"
          spellCheck={false}
          placeholder="imap.example.com"
          onChange={(event) => setHost(event.target.value)}
        />
        <Field
          label={t.port}
          value={port}
          inputMode="numeric"
          placeholder={String(defaultPort(security))}
          onChange={(event) => setPort(event.target.value.replace(/\D/g, '').slice(0, 5))}
        />
      </div>
      <div className="grid gap-2">
        <span className="text-xs text-fg-muted">{t.security}</span>
        <ToggleGroup
          type="single"
          value={security}
          onValueChange={(value) => isSecurity(value) && setSecurity(value)}
          className="w-fit"
          aria-label={t.security}
        >
          {securities.map((value) => (
            <ToggleGroupItem key={value} value={value}>
              {t.securityNames[value]}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
        {security === 'plain' && (
          <p className="flex items-start gap-2 text-xs text-warning">
            <TriangleAlertIcon aria-hidden className="mt-0.5 size-3 shrink-0" />
            {t.plainWarning}
          </p>
        )}
      </div>
      <Field
        label={t.username}
        hint={t.usernameHint}
        value={username}
        required
        autoCapitalize="none"
        spellCheck={false}
        onChange={(event) => setUsername(event.target.value)}
      />
      <PasswordField
        label={t.password}
        autoComplete="off"
        hint={a.keepPassword}
        value={password}
        onChange={(event) => setPassword(event.target.value)}
      />
      <Field
        label={a.excluded}
        hint={a.excludedHint}
        value={excluded}
        autoCapitalize="none"
        spellCheck={false}
        onChange={(event) => setExcluded(event.target.value)}
      />
      <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
      <SubmitButton busy={mutation.isPending} label={a.save} busyLabel={t.busy} />
    </form>
  )
}
