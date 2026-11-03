import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@holtmail/ui'
import { useMutation } from '@tanstack/react-query'
import { type FormEvent, type ReactNode, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { strings } from '../strings.ts'

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  action,
  busyAction,
  typeToConfirm,
  confirmLabel,
  onConfirm,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: ReactNode
  action: string
  busyAction: string
  typeToConfirm?: string
  confirmLabel?: string
  onConfirm: () => Promise<unknown>
}) {
  const [typed, setTyped] = useState('')
  const mutation = useMutation({
    mutationFn: onConfirm,
    onSuccess: () => onOpenChange(false),
  })
  const ready = !typeToConfirm || typed.trim() === typeToConfirm

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    if (ready) mutation.mutate()
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setTyped('')
          mutation.reset()
        }
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            <DialogDescription>{description}</DialogDescription>
          </DialogHeader>
          {typeToConfirm && (
            <Field
              label={confirmLabel ?? typeToConfirm}
              value={typed}
              onChange={(event) => setTyped(event.target.value)}
              autoComplete="off"
              spellCheck={false}
              autoFocus
            />
          )}
          <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {strings.connect.cancel}
            </Button>
            <Button
              type="submit"
              variant="danger"
              disabled={!ready || mutation.isPending}
              aria-busy={mutation.isPending}
              autoFocus={!typeToConfirm}
            >
              {mutation.isPending ? busyAction : action}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}

export function TextDialog({
  open,
  onOpenChange,
  title,
  description,
  label,
  initial,
  password,
  onSave,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description?: ReactNode
  label: string
  initial?: string
  password?: boolean
  onSave: (value: string) => Promise<unknown>
}) {
  const [value, setValue] = useState(initial ?? '')
  const mutation = useMutation({
    mutationFn: () => onSave(password ? value : value.trim()),
    onSuccess: () => onOpenChange(false),
  })

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (next) setValue(initial ?? '')
        else mutation.reset()
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form
          className="grid gap-6"
          onSubmit={(event) => {
            event.preventDefault()
            if (value.trim()) mutation.mutate()
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          <Field
            label={label}
            type={password ? 'password' : 'text'}
            autoComplete={password ? 'new-password' : 'off'}
            value={value}
            onChange={(event) => setValue(event.target.value)}
            autoFocus
            required
          />
          <FormError message={mutation.isError ? authErrorMessage(mutation.error) : undefined} />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {strings.connect.cancel}
            </Button>
            <Button
              type="submit"
              variant="primary"
              disabled={!value.trim() || mutation.isPending}
              aria-busy={mutation.isPending}
            >
              {mutation.isPending ? strings.accounts.saving : strings.accounts.save}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
