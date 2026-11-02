import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'
import { focusRing } from './focus.ts'

export function Input({ className, type = 'text', ...props }: ComponentProps<'input'>) {
  return (
    <input
      data-slot="input"
      type={type}
      className={cn(
        'h-8 w-full min-w-0 border border-border bg-bg-subtle px-3 text-sm text-fg',
        'transition-colors duration-fast-02 ease-productive',
        'placeholder:text-fg-subtle hover:border-border-strong',
        'disabled:cursor-not-allowed disabled:text-fg-subtle',
        'aria-invalid:border-danger',
        'file:border-0 file:bg-transparent file:text-sm file:text-fg',
        focusRing,
        className,
      )}
      {...props}
    />
  )
}
