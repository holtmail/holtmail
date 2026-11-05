import {
  type Account,
  type JmapAuth,
  type JmapConnection,
  updateAccount,
} from '@holtmail/api-client'
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
import { effectivePort, imapMoves, jmapMoves } from '../lib/connection.ts'
import { isJmapAuth } from '../lib/jmap.ts'
import { isOAuthProvider, oauthProviderNames } from '../lib/oauth.ts'
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
        {open &&
          (account.jmap ? (
            <EditJmapForm
              account={account}
              current={account.jmap}
              onDone={() => onOpenChange(false)}
            />
          ) : (
            <EditConnectionForm account={account} onDone={() => onOpenChange(false)} />
          ))}
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
  const oauth = account.oauth_provider
  const fixedBy = oauth && (isOAuthProvider(oauth) ? oauthProviderNames[oauth] : oauth)
  const moved =
    !oauth &&
    (!current || imapMoves(current, { host, port: effectivePort(port, security), security }))

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
        password: oauth ? null : password || null,
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
          readOnly={Boolean(oauth)}
          autoCapitalize="none"
          spellCheck={false}
          placeholder="imap.example.com"
          onChange={(event) => setHost(event.target.value)}
        />
        <Field
          label={t.port}
          value={port}
          inputMode="numeric"
          readOnly={Boolean(oauth)}
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
          disabled={Boolean(oauth)}
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
        {fixedBy && <p className="text-xs text-fg-subtle">{a.fixedServer(fixedBy)}</p>}
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
      {!oauth && (
        <PasswordField
          label={t.password}
          autoComplete="off"
          required={moved}
          hint={moved ? a.passwordForServer : a.keepPassword}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
        />
      )}
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

function EditJmapForm({
  account,
  current,
  onDone,
}: {
  account: Account
  current: JmapConnection
  onDone: () => void
}) {
  const j = strings.jmap
  const queryClient = useQueryClient()
  const [url, setUrl] = useState(current.url)
  const [auth, setAuth] = useState<JmapAuth>(isJmapAuth(current.auth) ? current.auth : 'password')
  const [username, setUsername] = useState(current.username)
  const [secret, setSecret] = useState('')
  const [excluded, setExcluded] = useState(current.exclude.join(', '))
  const authChanged = auth !== current.auth
  const moved = jmapMoves(current.url, url)

  const mutation = useMutation({
    mutationFn: () =>
      updateAccount(account.id, {
        jmap: {
          url: url.trim(),
          auth,
          username: auth === 'password' ? username.trim() : '',
          exclude: parseExcluded(excluded),
        },
        password: secret.trim() || null,
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
      <Field
        label={j.url}
        hint={j.urlHint}
        value={url}
        required
        type="url"
        inputMode="url"
        autoCapitalize="none"
        spellCheck={false}
        onChange={(event) => setUrl(event.target.value)}
      />
      <div className="grid gap-2">
        <span className="text-xs text-fg-muted">{j.auth}</span>
        <ToggleGroup
          type="single"
          value={auth}
          onValueChange={(value) => isJmapAuth(value) && setAuth(value)}
          className="w-fit"
          aria-label={j.auth}
        >
          <ToggleGroupItem value="password">{j.auths.password}</ToggleGroupItem>
          <ToggleGroupItem value="token">{j.auths.token}</ToggleGroupItem>
        </ToggleGroup>
      </div>
      {auth === 'password' && (
        <Field
          label={t.username}
          hint={t.usernameHint}
          value={username}
          required
          autoCapitalize="none"
          spellCheck={false}
          onChange={(event) => setUsername(event.target.value)}
        />
      )}
      <PasswordField
        label={auth === 'token' ? j.token : t.password}
        autoComplete="off"
        required={authChanged || moved}
        hint={authChanged ? j.newSecret : moved ? j.secretForServer : j.keepSecret}
        value={secret}
        onChange={(event) => setSecret(event.target.value)}
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
