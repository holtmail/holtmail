import { type Account, addImapAccount } from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import { Button, cn, ToggleGroup, ToggleGroupItem } from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ArrowUpRightIcon, CheckIcon, ChevronDownIcon, TriangleAlertIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { type FormEvent, useId, useRef, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { oauthProviderFor, oauthProviderNames } from '../lib/oauth.ts'
import {
  defaultPort,
  guessServer,
  providerFor,
  providers,
  type Security,
} from '../lib/providers.ts'
import { strings } from '../strings.ts'
import { SwitchLink } from './connect-jmap.tsx'
import { OAuthPasteStep, ThunderbirdNote, useOAuthSignIn } from './oauth-sign-in.tsx'

const t = strings.connect
const securities: Security[] = ['tls', 'starttls', 'plain']

const isSecurity = (value: string): value is Security =>
  value === 'tls' || value === 'starttls' || value === 'plain'

export function ConnectImapForm({
  archiveId,
  onConnected,
  onCancel,
  onUseJmap,
  onUseGraph,
}: {
  archiveId: string
  onConnected: (account: Account) => void
  onCancel?: () => void
  onUseJmap?: () => void
  onUseGraph?: () => void
}) {
  const queryClient = useQueryClient()
  const email = useRef<HTMLInputElement>(null)
  const advancedId = useId()
  const [address, setAddress] = useState('')
  const [password, setPassword] = useState('')
  const [advanced, setAdvanced] = useState(false)
  const [custom, setCustom] = useState(false)
  const [host, setHost] = useState('')
  const [port, setPort] = useState('')
  const [security, setSecurity] = useState<Security>('tls')
  const [username, setUsername] = useState('')
  const [name, setName] = useState('')
  const [usePassword, setUsePassword] = useState(false)

  const provider = providerFor(address)
  const oauthProvider = oauthProviderFor(address)
  const oauth = oauthProvider !== undefined && !usePassword
  const oauthName = oauthProvider ? oauthProviderNames[oauthProvider] : ''
  const flow = useOAuthSignIn(onConnected)
  const guess = guessServer(address)
  const server = custom
    ? { host: host.trim(), port: Number(port) || defaultPort(security), security }
    : guess

  const mutation = useMutation({
    mutationFn: () =>
      addImapAccount(archiveId, {
        host: server?.host ?? '',
        port: server?.port,
        security: server?.security,
        username: (custom && username.trim()) || address.trim(),
        password,
        name: name.trim() || null,
      }),
    onSuccess: async (account) => {
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
      onConnected(account)
    },
    onError: () => setAdvanced((open) => open || !provider),
  })

  const openAdvanced = () => {
    if (!custom) {
      setHost(guess?.host ?? '')
      setPort(String(guess?.port ?? 993))
      setSecurity(guess?.security ?? 'tls')
      setUsername(address.trim())
      setCustom(true)
    }
    setAdvanced((open) => !open)
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (oauth) {
      flow.begin({
        provider: oauthProvider,
        archive_id: archiveId,
        email: address.trim(),
        name: name.trim() || null,
      })
      return
    }
    if (!server?.host) {
      openAdvanced()
      return
    }
    mutation.mutate()
  }

  const error = mutation.isError ? authErrorMessage(mutation.error) : undefined

  const { started } = flow
  if (started && oauthProvider) {
    return <OAuthPasteStep flow={{ ...flow, started }} provider={oauthProvider} />
  }

  const switchMode = (
    <button
      type="button"
      onClick={() => setUsePassword((value) => !value)}
      className="w-fit text-sm text-fg-muted underline-offset-4 outline-none hover:text-fg hover:underline focus-visible:outline-2 focus-visible:outline-focus"
    >
      {oauth ? strings.oauth.usePassword : strings.oauth.useOAuth(oauthName)}
    </button>
  )

  return (
    <form className="grid gap-6" onSubmit={submit} noValidate>
      <Field
        ref={email}
        label={t.email}
        type="email"
        name="email"
        autoComplete="email"
        inputMode="email"
        autoCapitalize="none"
        spellCheck={false}
        required
        autoFocus
        value={address}
        onChange={(event) => setAddress(event.target.value)}
        hint={
          <AnimatePresence initial={false} mode="popLayout">
            {!address.includes('@') && (
              <m.span
                key="known"
                className="block"
                initial={{ opacity: 0 }}
                animate={{ opacity: 1, transition: transition.productive.moderate01 }}
                exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
              >
                {t.known(providers.map((known) => known.name).join(', '))}
              </m.span>
            )}
            {oauth && (
              <m.span
                key="oauth"
                className="flex items-center gap-1 text-success"
                initial={{ opacity: 0, y: -4 }}
                animate={{ opacity: 1, y: 0, transition: transition.productive.moderate01 }}
                exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
              >
                <CheckIcon aria-hidden className="size-3" />
                {strings.oauth.lead(oauthName)}
              </m.span>
            )}
            {!oauth && provider && !custom && (
              <m.span
                key={provider.id}
                className="flex items-center gap-1 text-success"
                initial={{ opacity: 0, y: -4 }}
                animate={{ opacity: 1, y: 0, transition: transition.productive.moderate01 }}
                exit={{ opacity: 0, transition: transition.productiveExit.fast02 }}
              >
                <CheckIcon aria-hidden className="size-3" />
                {t.detected(provider.name)}
              </m.span>
            )}
          </AnimatePresence>
        }
      />
      {oauth ? (
        <>
          <Field
            label={t.name}
            name="name"
            value={name}
            onChange={(event) => setName(event.target.value)}
            placeholder={address.trim() || undefined}
          />
          <ThunderbirdNote />
        </>
      ) : (
        <>
          <PasswordField
            label={t.password}
            name="password"
            autoComplete="off"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            hint={
              provider?.note ? (
                <span className="grid gap-1">
                  <span>{provider.note}</span>
                  {provider.help && (
                    <a
                      href={provider.help}
                      target="_blank"
                      rel="noreferrer"
                      className="flex w-fit items-center gap-1 text-accent hover:underline"
                    >
                      {t.help}
                      <ArrowUpRightIcon aria-hidden className="size-3" />
                    </a>
                  )}
                </span>
              ) : undefined
            }
          />

          <div className="grid gap-4">
            <button
              type="button"
              aria-expanded={advanced}
              aria-controls={advancedId}
              onClick={openAdvanced}
              className="flex w-fit items-center gap-2 text-sm text-fg-muted outline-none hover:text-fg focus-visible:outline-2 focus-visible:outline-focus"
            >
              <ChevronDownIcon
                aria-hidden
                className={cn(
                  'size-4 transition-transform duration-moderate-01 ease-productive',
                  advanced && 'rotate-180',
                )}
              />
              {t.advanced}
              {!advanced && server?.host && (
                <span className="font-mono text-xs text-fg-subtle">
                  {server.host}:{server.port}
                </span>
              )}
            </button>
            <AnimatePresence initial={false}>
              {advanced && (
                <m.div
                  id={advancedId}
                  className="overflow-hidden"
                  initial={{ height: 0, opacity: 0 }}
                  animate={{
                    height: 'auto',
                    opacity: 1,
                    transition: transition.expressive.moderate02,
                  }}
                  exit={{ height: 0, opacity: 0, transition: transition.expressiveExit.moderate02 }}
                >
                  <div className="grid gap-6 border-l-2 border-border pb-1 pl-4">
                    <div className="grid grid-cols-[1fr_6rem] gap-4">
                      <Field
                        label={t.host}
                        value={host}
                        onChange={(event) => setHost(event.target.value)}
                        autoCapitalize="none"
                        spellCheck={false}
                        placeholder="imap.example.com"
                      />
                      <Field
                        label={t.port}
                        value={port}
                        inputMode="numeric"
                        onChange={(event) =>
                          setPort(event.target.value.replace(/\D/g, '').slice(0, 5))
                        }
                        placeholder={String(defaultPort(security))}
                      />
                    </div>
                    <div className="grid gap-2">
                      <span className="text-xs text-fg-muted">{t.security}</span>
                      <ToggleGroup
                        type="single"
                        value={security}
                        onValueChange={(value) => {
                          if (!isSecurity(value)) return
                          setSecurity(value)
                          if (port === String(defaultPort(security)))
                            setPort(String(defaultPort(value)))
                        }}
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
                      onChange={(event) => setUsername(event.target.value)}
                      autoCapitalize="none"
                      spellCheck={false}
                    />
                    <Field
                      label={t.name}
                      value={name}
                      onChange={(event) => setName(event.target.value)}
                      placeholder={address.trim() || undefined}
                    />
                  </div>
                </m.div>
              )}
            </AnimatePresence>
          </div>
        </>
      )}

      <FormError message={oauth ? flow.error : error} />
      <div className="flex items-center gap-2">
        {onCancel && (
          <Button type="button" variant="ghost" className="h-12 px-4" onClick={onCancel}>
            {t.cancel}
          </Button>
        )}
        {oauth ? (
          <SubmitButton
            busy={flow.busy}
            label={strings.oauth.signIn(oauthName)}
            busyLabel={strings.oauth.starting}
          />
        ) : (
          <SubmitButton busy={mutation.isPending} label={t.submit} busyLabel={t.busy} />
        )}
      </div>
      {oauthProvider && switchMode}
      {onUseGraph && <SwitchLink label={t.useGraph} onClick={onUseGraph} />}
      {onUseJmap && <SwitchLink label={t.useJmap} onClick={onUseJmap} />}
    </form>
  )
}
