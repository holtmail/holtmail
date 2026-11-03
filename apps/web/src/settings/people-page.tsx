import {
  type Archive,
  archivesQuery,
  createUser,
  deleteUser,
  type Person,
  type Role,
  sessionQuery,
  updateUser,
  usersQuery,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Badge,
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
  Skeleton,
  ToggleGroup,
  ToggleGroupItem,
} from '@holtmail/ui'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { EllipsisIcon, KeyRoundIcon, LibraryIcon, Trash2Icon, UserPlusIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { type FormEvent, useState } from 'react'

import { Field, FormError, PasswordField, SubmitButton } from '../auth/field.tsx'
import { authErrorMessage, initials, minPasswordLength, passwordShortBy } from '../lib/auth.ts'
import { formatFullDate } from '../lib/format.ts'
import { manages } from '../lib/roles.ts'
import { ConfirmDialog, TextDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings
const roles: Role[] = ['member', 'admin', 'owner']

const isRole = (value: string): value is Role =>
  value === 'owner' || value === 'admin' || value === 'member'

export function PeoplePage() {
  const people = useQuery(usersQuery)
  const me = useQuery(sessionQuery).data
  const archives = useQuery(archivesQuery).data ?? []
  const [adding, setAdding] = useState(false)

  return (
    <SettingsPage
      title={t.people}
      lead={t.peopleLead}
      action={
        <Button variant="secondary" onClick={() => setAdding(true)}>
          <UserPlusIcon />
          {t.addPerson}
        </Button>
      }
    >
      {people.isPending && <Skeleton className="h-32" />}
      <ul className="grid divide-y divide-border border-y border-border">
        <AnimatePresence initial={false}>
          {people.data?.map((person) => (
            <m.li
              key={person.user.id}
              layout
              initial={{ opacity: 0 }}
              animate={{ opacity: 1, transition: transition.productive.moderate01 }}
              exit={{ opacity: 0, transition: transition.productiveExit.moderate01 }}
            >
              <PersonRow
                person={person}
                isMe={person.user.id === me?.id}
                actorIsOwner={me?.role === 'owner'}
                archives={archives}
              />
            </m.li>
          ))}
        </AnimatePresence>
      </ul>
      <AddPersonDialog
        open={adding}
        onOpenChange={setAdding}
        archives={archives}
        actorIsOwner={me?.role === 'owner'}
      />
    </SettingsPage>
  )
}

function PersonRow({
  person,
  isMe,
  actorIsOwner,
  archives,
}: {
  person: Person
  isMe: boolean
  actorIsOwner: boolean
  archives: Archive[]
}) {
  const queryClient = useQueryClient()
  const [dialog, setDialog] = useState<'access' | 'password' | 'remove' | null>(null)
  const { user } = person
  const role: Role = isRole(user.role) ? user.role : 'member'
  const locked = role === 'owner' && !actorIsOwner
  const refresh = () => queryClient.invalidateQueries({ queryKey: ['users'] })
  const changeRole = useMutation({
    mutationFn: (next: Role) => updateUser(user.id, { role: next }),
    onSettled: refresh,
  })
  const access = manages(role)
    ? t.allArchives
    : person.archives.length === 0
      ? t.noArchives
      : t.archivesCount(person.archives.length)

  return (
    <div className="flex min-h-16 items-center gap-4 py-3">
      <span
        aria-hidden
        className="flex size-8 shrink-0 items-center justify-center bg-bg-selected text-sm font-semibold"
      >
        {initials(user.name, user.email)}
      </span>
      <div className="grid min-w-0 flex-1 gap-0.5">
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate text-sm text-fg">{user.name}</span>
          {isMe && <Badge variant="outline">{t.you}</Badge>}
        </span>
        <span className="truncate font-mono text-xs text-fg-subtle">{user.email}</span>
        <span className="truncate text-xs text-fg-subtle">
          {t.joined(formatFullDate(person.created_at))}
        </span>
      </div>
      <div className="grid justify-items-end gap-1 text-right max-sm:hidden">
        <Badge variant={role === 'member' ? 'neutral' : 'accent'}>{t.roles[role]}</Badge>
        <span className="text-xs text-fg-subtle">{access}</span>
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button
            variant="ghost"
            size="icon"
            aria-label={t.personActions(user.name)}
            disabled={locked}
          >
            <EllipsisIcon />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-64">
          <DropdownMenuLabel>{t.role}</DropdownMenuLabel>
          <DropdownMenuRadioGroup
            value={role}
            onValueChange={(value) => {
              if (isRole(value) && value !== role) changeRole.mutate(value)
            }}
          >
            {roles.map((value) => (
              <DropdownMenuRadioItem
                key={value}
                value={value}
                disabled={isMe || (value === 'owner' && !actorIsOwner)}
              >
                <span className="grid">
                  <span>{t.roles[value]}</span>
                  <span className="text-xs text-fg-subtle">{t.roleHints[value]}</span>
                </span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
          <DropdownMenuSeparator />
          {role === 'member' && (
            <DropdownMenuItem onSelect={() => setDialog('access')}>
              <LibraryIcon />
              {t.access}
            </DropdownMenuItem>
          )}
          {!isMe && (
            <DropdownMenuItem onSelect={() => setDialog('password')}>
              <KeyRoundIcon />
              {t.resetPassword}
            </DropdownMenuItem>
          )}
          {!isMe && (
            <DropdownMenuItem className="text-danger" onSelect={() => setDialog('remove')}>
              <Trash2Icon />
              {strings.accounts.remove}
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>

      <AccessDialog
        open={dialog === 'access'}
        onOpenChange={(open) => setDialog(open ? 'access' : null)}
        person={person}
        archives={archives}
      />
      <TextDialog
        open={dialog === 'password'}
        onOpenChange={(open) => setDialog(open ? 'password' : null)}
        title={t.resetPassword}
        description={t.resetLead}
        label={t.newPassword}
        password
        onSave={(password) => updateUser(user.id, { password })}
      />
      <ConfirmDialog
        open={dialog === 'remove'}
        onOpenChange={(open) => setDialog(open ? 'remove' : null)}
        title={t.removePerson(user.name)}
        description={t.removePersonLead}
        action={strings.accounts.remove}
        busyAction={t.deleting}
        onConfirm={async () => {
          await deleteUser(user.id)
          await refresh()
        }}
      />
    </div>
  )
}

function ArchiveChecklist({
  archives,
  selected,
  onChange,
}: {
  archives: Archive[]
  selected: string[]
  onChange: (ids: string[]) => void
}) {
  if (archives.length === 0) return <p className="text-sm text-fg-subtle">{t.noArchives}</p>
  return (
    <fieldset className="grid gap-px border border-border bg-border">
      <legend className="sr-only">{t.access}</legend>
      {archives.map((archive) => {
        const checked = selected.includes(archive.id)
        return (
          <label
            key={archive.id}
            className="flex h-10 cursor-default items-center gap-3 bg-bg px-3 text-sm hover:bg-bg-hover"
          >
            <input
              type="checkbox"
              className="size-4 accent-accent"
              checked={checked}
              onChange={() =>
                onChange(
                  checked ? selected.filter((id) => id !== archive.id) : [...selected, archive.id],
                )
              }
            />
            <span className="truncate">{archive.name}</span>
          </label>
        )
      })}
    </fieldset>
  )
}

function AccessDialog({
  open,
  onOpenChange,
  person,
  archives,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  person: Person
  archives: Archive[]
}) {
  const queryClient = useQueryClient()
  const [selected, setSelected] = useState(person.archives)
  const mutation = useMutation({
    mutationFn: () => updateUser(person.user.id, { archives: selected }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['users'] })
      onOpenChange(false)
    },
  })
  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) setSelected(person.archives)
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t.access}</DialogTitle>
          <DialogDescription>{person.user.name}</DialogDescription>
        </DialogHeader>
        <ArchiveChecklist archives={archives} selected={selected} onChange={setSelected} />
        <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            {strings.connect.cancel}
          </Button>
          <Button variant="primary" disabled={mutation.isPending} onClick={() => mutation.mutate()}>
            {mutation.isPending ? strings.accounts.saving : strings.accounts.save}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function AddPersonDialog({
  open,
  onOpenChange,
  archives,
  actorIsOwner,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  archives: Archive[]
  actorIsOwner: boolean
}) {
  const queryClient = useQueryClient()
  const [email, setEmail] = useState('')
  const [name, setName] = useState('')
  const [password, setPassword] = useState('')
  const [role, setRole] = useState<Role>('member')
  const [selected, setSelected] = useState<string[]>([])
  const shortBy = passwordShortBy(password)

  const reset = () => {
    setEmail('')
    setName('')
    setPassword('')
    setRole('member')
    setSelected([])
    mutation.reset()
  }

  const mutation = useMutation({
    mutationFn: () =>
      createUser({
        email: email.trim(),
        name: name.trim(),
        password,
        role,
        archives: role === 'member' ? selected : [],
      }),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ['users'] })
      onOpenChange(false)
      reset()
    },
  })

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    mutation.mutate()
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) reset()
        onOpenChange(next)
      }}
    >
      <DialogContent className="max-h-[calc(100dvh-var(--spacing)*8)] overflow-y-auto">
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.addPerson}</DialogTitle>
            <DialogDescription>{t.addPersonLead}</DialogDescription>
          </DialogHeader>
          <Field
            label={strings.auth.email}
            type="email"
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            required
            autoFocus
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
          <Field
            label={strings.auth.name}
            required
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
          <PasswordField
            label={strings.auth.password}
            autoComplete="new-password"
            required
            minLength={minPasswordLength}
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            hint={
              shortBy > 0
                ? strings.auth.setup.passwordShort(shortBy)
                : strings.auth.setup.passwordOk
            }
          />
          <div className="grid gap-2">
            <span className="text-xs text-fg-muted">{t.role}</span>
            <ToggleGroup
              type="single"
              value={role}
              onValueChange={(value) => isRole(value) && setRole(value)}
              className="w-fit"
              aria-label={t.role}
            >
              {roles
                .filter((value) => value !== 'owner' || actorIsOwner)
                .map((value) => (
                  <ToggleGroupItem key={value} value={value}>
                    {t.roles[value]}
                  </ToggleGroupItem>
                ))}
            </ToggleGroup>
            <span className="text-xs text-fg-subtle">{t.roleHints[role]}</span>
          </div>
          <AnimatePresence initial={false}>
            {role === 'member' && (
              <m.div
                className="grid gap-2 overflow-hidden"
                initial={{ height: 0, opacity: 0 }}
                animate={{
                  height: 'auto',
                  opacity: 1,
                  transition: transition.expressive.moderate02,
                }}
                exit={{ height: 0, opacity: 0, transition: transition.expressiveExit.moderate02 }}
              >
                <span className="text-xs text-fg-muted">{t.access}</span>
                <ArchiveChecklist archives={archives} selected={selected} onChange={setSelected} />
              </m.div>
            )}
          </AnimatePresence>
          <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
          <SubmitButton
            busy={mutation.isPending}
            label={t.addPerson}
            busyLabel={strings.accounts.saving}
          />
        </form>
      </DialogContent>
    </Dialog>
  )
}
