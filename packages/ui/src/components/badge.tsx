import { cva, type VariantProps } from 'class-variance-authority'
import { Slot } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'

export const badgeVariants = cva(
  'inline-flex h-5 shrink-0 items-center gap-1 border px-1.5 text-xs whitespace-nowrap [&_svg]:size-3',
  {
    variants: {
      variant: {
        neutral: 'border-transparent bg-bg-selected text-fg',
        outline: 'border-border-strong text-fg-muted',
        accent: 'border-transparent bg-accent text-accent-fg',
        danger: 'border-danger text-danger',
        warning: 'border-warning text-warning',
        success: 'border-success text-success',
      },
    },
    defaultVariants: {
      variant: 'neutral',
    },
  },
)

export type BadgeProps = ComponentProps<'span'> &
  VariantProps<typeof badgeVariants> & {
    asChild?: boolean
  }

export function Badge({ className, variant, asChild = false, ...props }: BadgeProps) {
  const Comp = asChild ? Slot.Root : 'span'
  return <Comp data-slot="badge" className={cn(badgeVariants({ variant }), className)} {...props} />
}
