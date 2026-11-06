import { type Account, updateAccount } from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Switch,
} from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { type FormEvent, useId, useState } from 'react'

import { Field, FormError, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { asksConsent, contactsSupport, davPreset, davRequest } from '../lib/contacts-calendars.ts'
import { domainOf } from '../lib/providers.ts'
import { strings } from '../strings.ts'

const t = strings.contacts

export function ContactsCalendarsDialog({
  account,
  open,
  onOpenChange,
  onSignIn,
}: {
  account: Account
  open: boolean
  onOpenChange: (open: boolean) => void
  onSignIn: () => void
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] max-w-xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t.title}</DialogTitle>
          <DialogDescription>{t.lead}</DialogDescription>
        </DialogHeader>
        {open && (
          <ContactsCalendarsForm
            account={account}
            onDone={() => onOpenChange(false)}
            onSignIn={() => {
              onOpenChange(false)
              onSignIn()
            }}
          />
        )}
      </DialogContent>
    </Dialog>
  )
}

function ContactsCalendarsForm({
  account,
  onDone,
  onSignIn,
}: {
  account: Account
  onDone: () => void
  onSignIn: () => void
}) {
  const queryClient = useQueryClient()
  const support = contactsSupport(account)
  const settings = account.contacts_calendars
  const address = account.address ?? account.connection?.username ?? account.jmap?.username ?? ''
  const preset = settings?.contacts_url || support === 'jmap' ? undefined : davPreset(address)
  const [enabled, setEnabled] = useState(settings?.enabled ?? true)
  const [contacts, setContacts] = useState(settings?.contacts_url ?? preset?.contacts ?? '')
  const [calendars, setCalendars] = useState(settings?.calendars_url ?? preset?.calendars ?? '')
  const [separate, setSeparate] = useState(
    Boolean(settings?.calendars_url && settings.calendars_url !== settings.contacts_url) ||
      Boolean(preset),
  )
  const switchId = useId()
  const separateId = useId()

  const mutation = useMutation({
    mutationFn: () =>
      updateAccount(account.id, {
        contacts_calendars: davRequest(enabled, contacts, separate ? calendars : ''),
      }),
    onSuccess: async (saved) => {
      await queryClient.invalidateQueries({ queryKey: ['archives'] })
      if (saved.contacts_calendars?.needs_consent) onSignIn()
      else onDone()
    },
  })

  if (support === 'microsoft-imap' || support === 'none') {
    return <p className="text-sm text-fg-muted">{t.microsoftImap}</p>
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate()
  }
  const wasOn = settings?.enabled ?? false
  const label = !enabled
    ? t.turnOff
    : asksConsent(support) && (settings?.needs_consent || !wasOn)
      ? t.turnOnAndSignIn
      : wasOn
        ? strings.accounts.save
        : t.turnOn

  return (
    <form className="grid gap-6" onSubmit={submit}>
      <div className="flex items-center justify-between gap-4 border-y border-border py-3">
        <label htmlFor={switchId} className="text-sm text-fg">
          {t.title}
        </label>
        <span className="flex items-center gap-3 text-xs text-fg-muted">
          {enabled ? t.on : t.off}
          <Switch id={switchId} checked={enabled} onCheckedChange={setEnabled} />
        </span>
      </div>
      {enabled && support === 'google' && <p className="text-sm text-fg-muted">{t.google}</p>}
      {enabled && support === 'graph' && <p className="text-sm text-fg-muted">{t.graph}</p>}
      {enabled && support === 'jmap' && settings?.protocol === 'jmap' && wasOn && (
        <p className="text-sm text-fg-muted">{t.jmapInUse}</p>
      )}
      {enabled && (support === 'dav' || support === 'jmap') && (
        <>
          <Field
            label={t.contactsUrl}
            hint={
              support === 'jmap'
                ? t.jmap
                : preset
                  ? t.preset(preset.name)
                  : t.contactsHint(domainOf(address))
            }
            value={contacts}
            type="url"
            inputMode="url"
            autoCapitalize="none"
            spellCheck={false}
            placeholder={`https://${domainOf(address) || 'dav.example.com'}`}
            onChange={(event) => setContacts(event.target.value)}
          />
          <label htmlFor={separateId} className="flex items-center gap-3 text-xs text-fg-muted">
            <Switch id={separateId} checked={separate} onCheckedChange={setSeparate} />
            {t.separate}
          </label>
          {separate && (
            <Field
              label={t.calendarsUrl}
              value={calendars}
              type="url"
              inputMode="url"
              autoCapitalize="none"
              spellCheck={false}
              onChange={(event) => setCalendars(event.target.value)}
            />
          )}
          <p className="text-xs text-fg-subtle">{t.signIn}</p>
        </>
      )}
      {!enabled && wasOn && <p className="text-xs text-fg-subtle">{t.turnOffLead}</p>}
      <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
      {!enabled && !wasOn ? (
        <Button type="button" variant="secondary" className="h-12 w-full" onClick={onDone}>
          {strings.accounts.save}
        </Button>
      ) : (
        <SubmitButton busy={mutation.isPending} label={label} busyLabel={t.saving} />
      )}
    </form>
  )
}
