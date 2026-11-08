import {
  collectionsQuery,
  contactsExportUrl,
  joinPeople,
  peopleInfiniteQuery,
  type PersonDetail,
  sessionQuery,
  splitPerson,
  undoPersonOverride,
} from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  Input,
} from '@holtmail/ui'
import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { DownloadIcon, MergeIcon, PencilIcon, SearchIcon, SplitIcon, Undo2Icon } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { FormError } from '../auth/field.tsx'
import { EditDialog } from '../items/edit-dialog.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { cardEditable, contactDraft } from '../lib/edits.ts'
import { formatFullDate } from '../lib/format.ts'
import {
  canSplit,
  joinCandidates,
  overrideNotes,
  personLabel,
  sourceLabel,
  toggleChoice,
} from '../lib/people.ts'
import { manages } from '../lib/roles.ts'
import { strings } from '../strings.ts'

const t = strings.people

function useAfterChange(archiveId: string) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  return async (personId: string | undefined) => {
    await Promise.all([
      queryClient.invalidateQueries({ queryKey: ['people'] }),
      queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'people'] }),
      queryClient.invalidateQueries({ queryKey: ['archives', archiveId, 'people-overrides'] }),
    ])
    if (personId) {
      await navigate({
        to: '/a/$archiveId/people',
        params: { archiveId },
        search: (previous) => ({ ...previous, p: personId }),
      })
    }
  }
}

export function PersonActions({ detail }: { detail: PersonDetail }) {
  const user = useQuery(sessionQuery).data
  const [splitting, setSplitting] = useState(false)
  const [joining, setJoining] = useState(false)
  const [editing, setEditing] = useState(false)
  const { person } = detail
  const canManage = manages(user?.role)
  const collections = useQuery(collectionsQuery(person.archive_id)).data
  const editable = cardEditable(detail, collections)
  return (
    <div className="flex flex-wrap gap-2" role="group" aria-label={t.actions}>
      {editable && (
        <Button size="sm" onClick={() => setEditing(true)}>
          <PencilIcon />
          {strings.edits.actions.editCard}
        </Button>
      )}
      {editing && (
        <EditDialog
          target={{
            mode: 'person',
            archiveId: person.archive_id,
            personId: person.id,
            itemId: detail.primary_card,
            contact: contactDraft({
              full_name: person.name ?? null,
              emails: person.emails.map((value) => ({ value, kinds: [] })),
              phones: person.phones.map((value) => ({ value, kinds: [] })),
            }),
          }}
          onClose={() => setEditing(false)}
        />
      )}
      {canManage && detail.cards.length > 1 && (
        <Button size="sm" onClick={() => setSplitting(true)}>
          <SplitIcon />
          {t.split}
        </Button>
      )}
      {canManage && (
        <Button size="sm" onClick={() => setJoining(true)}>
          <MergeIcon />
          {t.join}
        </Button>
      )}
      {person.cards > 0 && (
        <Button asChild size="sm" variant="ghost">
          <a
            href={contactsExportUrl(person.archive_id, 'vcf', { person: person.id, removed: true })}
            download
          >
            <DownloadIcon />
            {t.exportCards}
          </a>
        </Button>
      )}
      {canManage && (
        <>
          <SplitDialog detail={detail} open={splitting} onOpenChange={setSplitting} />
          <JoinDialog detail={detail} open={joining} onOpenChange={setJoining} />
        </>
      )}
    </div>
  )
}

function SplitDialog({
  detail,
  open,
  onOpenChange,
}: {
  detail: PersonDetail
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const [chosen, setChosen] = useState<string[]>([])
  const after = useAfterChange(detail.person.archive_id)
  const mutation = useMutation({
    mutationFn: () => splitPerson(detail.person.id, chosen),
    onSuccess: async (result) => {
      onOpenChange(false)
      await after(result.people[0])
    },
  })
  const ready = canSplit(detail.cards.length, chosen.length)
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (ready) mutation.mutate()
  }
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setChosen([])
          mutation.reset()
        }
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.splitTitle}</DialogTitle>
            <DialogDescription>{t.splitLead}</DialogDescription>
          </DialogHeader>
          <fieldset className="grid gap-px border border-border bg-border">
            <legend className="sr-only">{t.cardsHeading}</legend>
            {detail.cards.map((card) => (
              <label
                key={card.item_id}
                aria-label={`${card.title ?? t.noName}, ${sourceLabel(card)}`}
                className="flex min-h-10 cursor-default items-center gap-3 bg-bg px-3 py-2 text-sm hover:bg-bg-hover"
              >
                <input
                  type="checkbox"
                  className="size-4 accent-accent"
                  checked={chosen.includes(card.item_id)}
                  onChange={() => setChosen((previous) => toggleChoice(previous, card.item_id))}
                />
                <span className="grid min-w-0">
                  <span className="truncate">{card.title ?? t.noName}</span>
                  <span className="truncate text-xs text-fg-subtle">{sourceLabel(card)}</span>
                </span>
              </label>
            ))}
          </fieldset>
          {chosen.length > 0 && !ready && (
            <p className="text-sm text-fg-muted">{t.splitNeedsTwo}</p>
          )}
          <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {strings.connect.cancel}
            </Button>
            <Button
              type="submit"
              variant="primary"
              disabled={!ready || mutation.isPending}
              aria-busy={mutation.isPending}
            >
              {mutation.isPending ? t.splitting : t.splitAction}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

function JoinDialog({
  detail,
  open,
  onOpenChange,
}: {
  detail: PersonDetail
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { person } = detail
  const [query, setQuery] = useState('')
  const people = useInfiniteQuery({
    ...peopleInfiniteQuery(person.archive_id, { q: query.trim() || undefined }),
    enabled: open,
  })
  const candidates = joinCandidates(people.data?.pages[0]?.items ?? [], person.id)
  const after = useAfterChange(person.archive_id)
  const mutation = useMutation({
    mutationFn: (other: string) => joinPeople([person.id, other]),
    onSuccess: async (result) => {
      onOpenChange(false)
      await after(result.people[0])
    },
  })
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setQuery('')
          mutation.reset()
        }
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <div className="grid gap-6">
          <DialogHeader>
            <DialogTitle>{t.joinTitle}</DialogTitle>
            <DialogDescription>{t.joinLead}</DialogDescription>
          </DialogHeader>
          <div className="relative">
            <SearchIcon
              aria-hidden
              className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-fg-subtle"
            />
            <Input
              type="search"
              aria-label={t.joinSearch}
              placeholder={t.joinSearch}
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              className="pl-9"
              autoFocus
            />
          </div>
          <ul className="grid max-h-72 gap-px overflow-y-auto border border-border bg-border">
            {candidates.length === 0 ? (
              <li className="bg-bg px-3 py-2 text-sm text-fg-subtle">
                {people.isPending ? strings.people.loadingMore : t.joinNone}
              </li>
            ) : (
              candidates.map((candidate) => {
                const label = personLabel(candidate, t.noName)
                return (
                  <li key={candidate.id} className="bg-bg">
                    <button
                      type="button"
                      disabled={mutation.isPending}
                      aria-label={t.joinAction(label)}
                      onClick={() => mutation.mutate(candidate.id)}
                      className="grid w-full cursor-default px-3 py-2 text-left text-sm outline-none hover:bg-bg-hover focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-solid focus-visible:outline-focus disabled:opacity-60"
                    >
                      <span className="truncate">{label}</span>
                      <span className="truncate font-mono text-xs text-fg-subtle">
                        {[candidate.email, t.cards(candidate.cards)].filter(Boolean).join(' · ')}
                      </span>
                    </button>
                  </li>
                )
              })
            )}
          </ul>
          {mutation.isPending && <p className="text-sm text-fg-muted">{t.joining}</p>}
          <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {strings.connect.cancel}
            </Button>
          </DialogFooter>
        </div>
      </DialogContent>
    </Dialog>
  )
}

export function OverrideNotes({ detail }: { detail: PersonDetail }) {
  const user = useQuery(sessionQuery).data
  const { person } = detail
  const after = useAfterChange(person.archive_id)
  const undo = useMutation({
    mutationFn: (id: string) => undoPersonOverride(id),
    onSuccess: async (result) => {
      await after(result.people.includes(person.id) ? undefined : result.people[0])
    },
  })
  const notes = overrideNotes(detail.overrides, person.id)
  if (notes.length === 0) return null
  const canManage = manages(user?.role)
  return (
    <section aria-label={t.byHand} className="grid gap-1 border border-border px-3 py-2 text-sm">
      {notes.map((note) => (
        <p key={note.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-fg-muted">
          <span className="text-fg">{note.kind === 'joined' ? t.joinedByHand : t.splitByHand}</span>
          <span className="font-mono text-xs text-fg-subtle">
            {t.overrideOn(formatFullDate(note.createdAt))}
          </span>
          {note.others[0] && (
            <Link
              to="/a/$archiveId/people"
              params={{ archiveId: person.archive_id }}
              search={(previous) => ({ ...previous, p: note.others[0] })}
              className="text-accent underline-offset-2 hover:underline"
            >
              {t.otherSide}
            </Link>
          )}
          {canManage && (
            <Button
              size="sm"
              variant="ghost"
              className="ml-auto"
              disabled={undo.isPending}
              aria-busy={undo.isPending && undo.variables === note.id}
              onClick={() => undo.mutate(note.id)}
            >
              <Undo2Icon />
              {undo.isPending && undo.variables === note.id ? t.undoing : t.undo}
            </Button>
          )}
        </p>
      ))}
      {undo.isError && <p className="text-sm text-danger">{t.undoFailed}</p>}
    </section>
  )
}
