import { type Account, setTrustedAuthservIds } from '@holtmail/api-client'
import {
  Button,
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ShieldCheckIcon, XIcon } from 'lucide-react'
import { type FormEvent, useState } from 'react'

import { Field, FormError } from '../auth/field.tsx'
import { authErrorMessage } from '../lib/auth.ts'
import { trustedIdError } from '../lib/categories.ts'
import { strings } from '../strings.ts'

const t = strings.trustedServers

/** Edits an account's trusted authentication servers; every change is saved at once. */
export function TrustedServersDialog({
  account,
  open,
  onOpenChange,
}: {
  account: Account
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const queryClient = useQueryClient()
  const [value, setValue] = useState('')
  const [problem, setProblem] = useState<string>()
  const list = account.trusted_authserv_ids ?? []
  const suggested = account.suggested_authserv_id
  const save = useMutation({
    mutationFn: (ids: readonly string[]) => setTrustedAuthservIds(account.id, ids),
    onSuccess: () => setValue(''),
    onSettled: () => queryClient.invalidateQueries({ queryKey: ['archives'] }),
  })

  const add = (id: string) => {
    const error = trustedIdError(id, list)
    setProblem(error)
    if (!error) save.mutate([...list, id.trim()])
  }

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    add(value)
  }

  return (
    <Dialog
      open={open}
      onOpenChange={(next) => {
        if (!next) {
          setValue('')
          setProblem(undefined)
          save.reset()
        }
        onOpenChange(next)
      }}
    >
      <DialogContent>
        <form className="grid gap-6" onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t.title}</DialogTitle>
            <DialogDescription>{t.lead}</DialogDescription>
          </DialogHeader>
          {list.length > 0 ? (
            <ul className="grid divide-y divide-border border-y border-border">
              {list.map((id) => (
                <li key={id} className="flex h-10 items-center gap-2">
                  <ShieldCheckIcon aria-hidden className="size-4 shrink-0 text-fg-subtle" />
                  <span className="min-w-0 flex-1 truncate font-mono text-sm">{id}</span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon"
                    aria-label={t.remove(id)}
                    title={t.remove(id)}
                    disabled={save.isPending}
                    onClick={() => save.mutate(list.filter((item) => item !== id))}
                  >
                    <XIcon />
                  </Button>
                </li>
              ))}
            </ul>
          ) : (
            <p className="text-sm text-fg-muted">{t.none}</p>
          )}
          {suggested && list.length === 0 && (
            <div className="flex flex-wrap items-center justify-between gap-3 border border-border bg-bg-subtle p-3">
              <span className="min-w-0 text-sm text-fg-muted">{t.suggested(suggested)}</span>
              <Button
                type="button"
                variant="secondary"
                size="sm"
                disabled={save.isPending}
                onClick={() => add(suggested)}
              >
                {t.trust}
              </Button>
            </div>
          )}
          <div className="flex items-end gap-2">
            <Field
              className="min-w-0 flex-1"
              label={t.label}
              placeholder={t.placeholder}
              autoComplete="off"
              spellCheck={false}
              value={value}
              onChange={(event) => {
                setValue(event.target.value)
                setProblem(undefined)
              }}
            />
            <Button type="submit" variant="secondary" disabled={save.isPending || !value.trim()}>
              {t.add}
            </Button>
          </div>
          <FormError
            message={problem ?? (save.isError ? authErrorMessage(save.error) : undefined)}
          />
          <DialogFooter>
            <Button type="button" variant="ghost" onClick={() => onOpenChange(false)}>
              {t.close}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
