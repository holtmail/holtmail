import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'

export function Skeleton({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-slot="skeleton"
      aria-hidden
      className={cn('animate-pulse bg-bg-hover motion-reduce:animate-none', className)}
      {...props}
    />
  )
}
