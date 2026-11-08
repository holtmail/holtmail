import { cn } from '@holtmail/ui'
import { type ComponentProps, type ReactNode, useId } from 'react'

const control =
  'border border-border bg-bg text-sm text-fg hover:border-border-strong focus-visible:outline-2 focus-visible:-outline-offset-1 focus-visible:outline-solid focus-visible:outline-focus disabled:text-fg-subtle'

export function NativeSelect({
  label,
  value,
  choices,
  onChange,
  hideLabel = false,
  className,
}: {
  label: string
  value: string
  choices: readonly { value: string; label: string }[]
  onChange: (value: string) => void
  hideLabel?: boolean
  className?: string
}) {
  const id = useId()
  return (
    <div className={cn('grid gap-2', className)}>
      <label htmlFor={id} className={cn('text-xs text-fg-muted', hideLabel && 'sr-only')}>
        {label}
      </label>
      <select
        id={id}
        className={cn(control, 'h-8 max-w-full px-1')}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      >
        {choices.map((choice) => (
          <option key={choice.value} value={choice.value}>
            {choice.label}
          </option>
        ))}
      </select>
    </div>
  )
}

export function TextArea({
  className,
  onChange,
  ...props
}: Omit<ComponentProps<'textarea'>, 'onChange'> & { onChange: (value: string) => void }) {
  return (
    <textarea
      className={cn(control, 'min-h-20 p-2', className)}
      onChange={(event) => onChange(event.target.value)}
      {...props}
    />
  )
}

export function SmallInput({
  label,
  className,
  onChange,
  ...props
}: Omit<ComponentProps<'input'>, 'onChange'> & {
  label: string
  onChange: (value: string) => void
}) {
  const id = useId()
  return (
    <div className={cn('grid min-w-0 gap-1', className)}>
      <label htmlFor={id} className="text-xs text-fg-muted">
        {label}
      </label>
      <input
        id={id}
        className={cn(control, 'h-8 min-w-0 px-2')}
        onChange={(event) => onChange(event.target.value)}
        {...props}
      />
    </div>
  )
}

export function Labeled({
  label,
  children,
}: {
  label: string
  children: (id: string) => ReactNode
}) {
  const id = useId()
  return (
    <div className="grid gap-1">
      <label htmlFor={id} className="text-xs text-fg-muted">
        {label}
      </label>
      {children(id)}
    </div>
  )
}
