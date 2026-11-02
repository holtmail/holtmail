import { cva, type VariantProps } from 'class-variance-authority'
import { Slot } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'
import { focusRing } from './focus.ts'

export const buttonVariants = cva(
  [
    'inline-flex shrink-0 cursor-default items-center justify-center gap-2 border font-normal whitespace-nowrap select-none',
    'transition-colors duration-fast-02 ease-productive',
    'disabled:pointer-events-none disabled:border-transparent disabled:bg-bg-subtle disabled:text-fg-subtle',
    '[&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
    focusRing,
  ],
  {
    variants: {
      variant: {
        primary: 'border-transparent bg-accent text-accent-fg hover:bg-accent-hover',
        secondary: 'border-border-strong bg-bg text-fg hover:bg-bg-hover',
        ghost: 'border-transparent bg-transparent text-fg hover:bg-bg-hover',
        danger: 'border-transparent bg-danger text-danger-fg hover:bg-danger-hover',
      },
      size: {
        sm: 'h-7 px-3 text-xs',
        md: 'h-8 px-4 text-sm',
        icon: 'size-8 p-0',
      },
    },
    defaultVariants: {
      variant: 'secondary',
      size: 'md',
    },
  },
)

export type ButtonProps = ComponentProps<'button'> &
  VariantProps<typeof buttonVariants> & {
    asChild?: boolean
  }

export function Button({
  className,
  variant,
  size,
  asChild = false,
  type = 'button',
  ...props
}: ButtonProps) {
  const Comp = asChild ? Slot.Root : 'button'
  return (
    <Comp
      data-slot="button"
      type={asChild ? undefined : type}
      className={cn(buttonVariants({ variant, size }), className)}
      {...props}
    />
  )
}
