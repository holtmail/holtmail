import {
  ApiError,
  type Collection,
  collectionsQuery,
  createItem,
  editItem,
  editPersonCard,
  type EventForm,
  idempotencyKey,
  type ItemContentRequest,
  type ItemEdit,
  itemEditQuery,
  organizingQuery,
} from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Skeleton,
  Switch,
  ToggleGroup,
  ToggleGroupItem,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { type FormEvent, useId, useState } from 'react'

import { FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import {
  type ContactDraft,
  contactDraft,
  contactRequest,
  type EditKind,
  editableCollections,
  type EventDraft,
  eventDraft,
  eventRequest,
  localDay,
  newEventDraft,
  rawTemplate,
} from '../lib/edits.ts'
import { viewerZone } from '../lib/events.ts'
import {
  canChooseInvitations,
  invitationsFlag,
  organizingNote,
  rawHasAttendees,
  reachesPeople,
} from '../lib/rsvp.ts'
import { strings } from '../strings.ts'
import { ContactFields } from './contact-fields.tsx'
import { EventFields } from './event-fields.tsx'
import { NativeSelect, TextArea } from './inputs.tsx'

const t = strings.edits

/** What the dialog edits: a new item in one of the archive's editable collections, an existing
 * item, or a person's card. */
export type EditTarget =
  | {
      mode: 'create'
      kind: EditKind
      archiveId: string
      collectionId?: string
      day?: string
      contact?: ContactDraft
      fromMessage?: string
    }
  | { mode: 'edit'; archiveId: string; itemId: string }
  | {
      mode: 'person'
      archiveId: string
      personId: string
      itemId?: string | null
      contact?: ContactDraft
    }

export function EditDialog({ target, onClose }: { target: EditTarget; onClose: () => void }) {
  const itemId = target.mode === 'create' ? undefined : (target.itemId ?? undefined)
  const edit = useQuery({ ...itemEditQuery(itemId ?? ''), enabled: Boolean(itemId) })
  const title =
    target.mode === 'create'
      ? target.kind === 'contact'
        ? t.dialog.newContact
        : t.dialog.newEvent
      : target.mode === 'person' || edit.data?.content_type === 'text/vcard'
        ? t.dialog.editContact
        : t.dialog.editEvent
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        {itemId && edit.isPending ? (
          <div className="grid gap-4" aria-busy>
            <DialogTitle>{title}</DialogTitle>
            <Skeleton className="h-64" />
          </div>
        ) : itemId && edit.isError ? (
          <div className="grid gap-4">
            <DialogHeader>
              <DialogTitle>{title}</DialogTitle>
              <DialogDescription>{t.dialog.loadFailed}</DialogDescription>
            </DialogHeader>
            <FormError message={authErrorMessage(edit.error)} />
            <DialogFooter>
              <Button variant="ghost" onClick={onClose}>
                {t.dialog.cancel}
              </Button>
            </DialogFooter>
          </div>
        ) : (
          <EditForm target={target} edit={edit.data} title={title} onClose={onClose} />
        )}
      </DialogContent>
    </Dialog>
  )
}

type Tab = 'form' | 'raw'

function EditForm({
  target,
  edit,
  title,
  onClose,
}: {
  target: EditTarget
  edit?: ItemEdit
  title: string
  onClose: () => void
}) {
  const queryClient = useQueryClient()
  const collections = useQuery(collectionsQuery(target.archiveId)).data
  const kind: EditKind =
    target.mode === 'create'
      ? target.kind
      : target.mode === 'person' || edit?.content_type === 'text/vcard'
        ? 'contact'
        : 'event'
  const choices = editableCollections(collections, kind)
  const [collectionId, setCollectionId] = useState(
    target.mode === 'create' ? (target.collectionId ?? '') : (edit?.collection_id ?? ''),
  )
  const chosen: Collection | undefined =
    collections?.find((c) => c.id === collectionId) ??
    (target.mode === 'create' ? choices[0] : undefined)
  const formShown = !edit || Boolean(kind === 'contact' ? edit.contact : edit.event)
  const [tab, setTab] = useState<Tab>(formShown ? 'form' : 'raw')
  const [event, setEvent] = useState<EventDraft>(() =>
    edit?.event
      ? eventDraft(edit.event)
      : newEventDraft(
          target.mode === 'create' && target.day ? target.day : localDay(new Date()),
          viewerZone(),
          new Date(),
        ),
  )
  const [contact, setContact] = useState<ContactDraft>(() =>
    edit?.contact
      ? contactDraft(edit.contact)
      : target.mode !== 'edit' && target.contact
        ? target.contact
        : contactDraft(),
  )
  const [raw, setRaw] = useState(() => edit?.raw ?? rawTemplate(kind, idempotencyKey(), new Date()))
  const [expect] = useState(() => (target.mode !== 'create' && edit ? edit.state : null))
  const [problem, setProblem] = useState<string>()
  const [sendInvitations, setSendInvitations] = useState(true)
  const rawId = useId()
  const sendId = useId()
  const organizingCheck = useQuery({
    ...organizingQuery(chosen?.id ?? '', target.mode === 'edit' ? target.itemId : undefined),
    enabled: kind === 'event' && Boolean(chosen?.account_id),
  })
  const organizing = organizingCheck.data
  const reaches =
    kind === 'event' &&
    (tab === 'raw'
      ? rawHasAttendees(raw) || (edit?.event?.attendees.length ?? 0) > 0
      : reachesPeople(edit?.event?.attendees, event.attendees))
  const checking = reaches && Boolean(chosen?.account_id) && !organizing

  const save = useMutation({
    mutationFn: async (request: ItemContentRequest) => {
      if (target.mode === 'person') return editPersonCard(target.personId, request)
      if (target.mode === 'edit') return editItem(target.itemId, request)
      if (!chosen) throw new Error(t.errors.collection)
      return createItem(chosen.id, request)
    },
    onSuccess: async () => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: ['items'] }),
        queryClient.invalidateQueries({ queryKey: ['archives', target.archiveId] }),
        queryClient.invalidateQueries({ queryKey: ['pim-changes'] }),
        queryClient.invalidateQueries({ queryKey: ['people'] }),
        queryClient.invalidateQueries({ queryKey: ['events'] }),
      ])
      onClose()
    },
  })

  const request = (): ItemContentRequest | string => {
    const base: ItemContentRequest = {
      event: null,
      contact: null,
      raw: null,
      from_message: target.mode === 'create' ? (target.fromMessage ?? null) : null,
      expect,
      invitations: kind === 'event' ? invitationsFlag(organizing, sendInvitations) : null,
    }
    if (tab === 'raw') return raw.trim() ? { ...base, raw } : t.errors.raw
    if (kind === 'contact') {
      const result = contactRequest(contact)
      if ('problem' in result) {
        return result.problem === 'email' ? t.errors.email(result.value ?? '') : t.errors.empty
      }
      return { ...base, contact: result.form }
    }
    const original: EventForm | undefined = edit?.event ?? undefined
    const result = eventRequest(event, original)
    if ('problem' in result) {
      return result.problem === 'email'
        ? t.errors.email(result.value ?? '')
        : t.errors[result.problem]
    }
    return { ...base, event: result.form }
  }

  const submit = (e: FormEvent) => {
    e.preventDefault()
    const built = request()
    if (typeof built === 'string') {
      setProblem(built)
      return
    }
    setProblem(undefined)
    save.mutate(built)
  }

  const account = chosen?.account_name ?? t.mark.account
  const note =
    chosen?.write_mode === 'write_through'
      ? t.dialog.through(account)
      : chosen?.write_mode === 'held'
        ? target.mode === 'create'
          ? t.dialog.heldCreate(account)
          : t.dialog.held(account)
        : undefined
  const failure = save.isError
    ? save.error instanceof ApiError && save.error.status === 409 && target.mode !== 'create'
      ? target.mode === 'person' && !expect
        ? t.dialog.noBook
        : t.dialog.stale
      : authErrorMessage(save.error)
    : undefined

  return (
    <form className="grid max-h-[80vh] gap-5 overflow-y-auto" onSubmit={submit} noValidate>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
        <DialogDescription>{note ?? t.dialog.rawHint}</DialogDescription>
      </DialogHeader>
      {target.mode === 'create' &&
        (choices.length === 0 ? (
          <p className="text-sm text-fg-muted">{t.dialog.noBook}</p>
        ) : (
          choices.length > 1 && (
            <NativeSelect
              label={kind === 'contact' ? t.dialog.addressBook : t.dialog.calendar}
              value={chosen?.id ?? ''}
              onChange={setCollectionId}
              choices={choices.map((c) => ({
                value: c.id,
                label: c.account_name ? `${c.name} · ${c.account_name}` : c.name,
              }))}
            />
          )
        ))}
      <ToggleGroup
        type="single"
        size="sm"
        className="justify-self-start"
        value={tab}
        onValueChange={(value) => {
          if (value === 'form' || value === 'raw') setTab(value)
        }}
      >
        <ToggleGroupItem value="form" disabled={!formShown}>
          {t.dialog.form}
        </ToggleGroupItem>
        <ToggleGroupItem value="raw">
          {kind === 'contact' ? t.dialog.rawContact : t.dialog.rawEvent}
        </ToggleGroupItem>
      </ToggleGroup>
      {target.mode === 'create' && target.fromMessage && target.contact?.emails[0] && (
        <p className="text-sm text-fg-muted">
          {t.dialog.fromSender(target.contact.emails[0].value)}
        </p>
      )}
      {!formShown && <p className="text-sm text-fg-muted">{t.dialog.formUnavailable}</p>}
      {tab === 'raw' ? (
        <div className="grid gap-2">
          <label htmlFor={rawId} className="text-xs text-fg-muted">
            {t.dialog.rawLabel}
          </label>
          <TextArea
            id={rawId}
            className="min-h-72 font-mono text-xs"
            spellCheck={false}
            value={raw}
            onChange={setRaw}
          />
          <span className="text-xs text-fg-subtle">{t.dialog.rawHint}</span>
        </div>
      ) : kind === 'contact' ? (
        <ContactFields draft={contact} onChange={setContact} />
      ) : (
        <EventFields draft={event} onChange={setEvent} />
      )}
      {reaches && checking && (
        <span className="text-xs text-fg-subtle">
          {organizingCheck.isError ? t.rsvp.invitations.failed : t.rsvp.invitations.loading}
        </span>
      )}
      {reaches && organizing && (
        <div className="grid gap-2">
          {canChooseInvitations(organizing) && (
            <div className="flex items-center gap-3">
              <Switch id={sendId} checked={sendInvitations} onCheckedChange={setSendInvitations} />
              <label htmlFor={sendId} className="text-sm">
                {t.rsvp.invitations.send}
              </label>
            </div>
          )}
          <span className="text-xs text-fg-subtle">
            {canChooseInvitations(organizing) && !sendInvitations
              ? t.rsvp.invitations.off
              : organizingNote(organizing)}
          </span>
        </div>
      )}
      <FormError message={problem ?? failure} />
      <DialogFooter>
        <Button variant="ghost" onClick={onClose}>
          {t.dialog.cancel}
        </Button>
        <Button
          type="submit"
          variant="primary"
          disabled={save.isPending || checking || (target.mode === 'create' && !chosen)}
        >
          {save.isPending ? t.dialog.saving : t.dialog.save}
        </Button>
      </DialogFooter>
    </form>
  )
}
