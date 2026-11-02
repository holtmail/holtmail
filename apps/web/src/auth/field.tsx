import { Button, cn, Input } from '@holtmail/ui'
import { ArrowRightIcon, CircleAlertIcon, EyeIcon, EyeOffIcon } from 'lucide-react'
import { type ComponentProps, type ReactNode, type Ref, useId, useState } from 'react'

import { strings } from '../strings.ts'

const inputClass =
  'h-10 border-transparent border-b-border-strong px-4 hover:border-transparent hover:border-b-fg-subtle'

export function Field({
  label,
  hint,
  className,
  ref,
  ...props
}: ComponentProps<'input'> & { label: string; hint?: ReactNode; ref?: Ref<HTMLInputElement> }) {
  const id = useId()
  const hintId = `${id}-hint`
  return (
    <div className={cn('grid gap-2', className)}>
      <label htmlFor={id} className="text-xs text-fg-muted">
        {label}
      </label>
      <Input
        id={id}
        ref={ref}
        aria-describedby={hint ? hintId : undefined}
        className={inputClass}
        {...props}
      />
      {hint && (
        <div id={hintId} className="text-xs text-fg-subtle">
          {hint}
        </div>
      )}
    </div>
  )
}

export function PasswordField({
  label,
  hint,
  ref,
  ...props
}: Omit<ComponentProps<'input'>, 'type'> & {
  label: string
  hint?: ReactNode
  ref?: Ref<HTMLInputElement>
}) {
  const id = useId()
  const hintId = `${id}-hint`
  const [visible, setVisible] = useState(false)
  return (
    <div className="grid gap-2">
      <label htmlFor={id} className="text-xs text-fg-muted">
        {label}
      </label>
      <div className="relative">
        <Input
          id={id}
          ref={ref}
          type={visible ? 'text' : 'password'}
          aria-describedby={hint ? hintId : undefined}
          className={cn(inputClass, 'pr-12')}
          spellCheck={false}
          autoCapitalize="none"
          autoCorrect="off"
          {...props}
        />
        <Button
          variant="ghost"
          size="icon"
          aria-label={visible ? strings.auth.hidePassword : strings.auth.showPassword}
          aria-pressed={visible}
          aria-controls={id}
          onClick={() => setVisible((value) => !value)}
          className="absolute inset-y-1 right-1 h-auto text-fg-muted"
        >
          {visible ? <EyeOffIcon /> : <EyeIcon />}
        </Button>
      </div>
      {hint && (
        <div id={hintId} aria-live="polite" className="text-xs text-fg-subtle">
          {hint}
        </div>
      )}
    </div>
  )
}

export function FormError({ message }: { message: string | undefined }) {
  return (
    <div role="alert" className="min-h-5 text-sm text-danger">
      {message && (
        <p className="flex items-start gap-2">
          <CircleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
          <span>{message}</span>
        </p>
      )}
    </div>
  )
}

export function SubmitButton({
  busy,
  label,
  busyLabel,
}: {
  busy: boolean
  label: string
  busyLabel: string
}) {
  return (
    <Button
      type="submit"
      variant="primary"
      disabled={busy}
      aria-busy={busy}
      className="h-12 w-full justify-between px-4 text-sm"
    >
      {busy ? busyLabel : label}
      <ArrowRightIcon />
    </Button>
  )
}
