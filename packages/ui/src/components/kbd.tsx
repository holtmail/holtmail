import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'

export function Kbd({ className, ...props }: ComponentProps<'kbd'>) {
  return (
    <kbd
      data-slot="kbd"
      className={cn(
        'inline-flex h-5 min-w-5 items-center justify-center border border-border bg-bg-subtle px-1 font-mono text-xs text-fg-muted select-none',
        className,
      )}
      {...props}
    />
  )
}
