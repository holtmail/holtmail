import { cva, type VariantProps } from 'class-variance-authority'
import { ToggleGroup as ToggleGroupPrimitive } from 'radix-ui'
import { type ComponentProps, createContext, use } from 'react'

import { cn } from '../lib/cn.ts'
import { focusRing } from './focus.ts'

export const toggleVariants = cva(
  [
    'inline-flex shrink-0 cursor-default items-center justify-center gap-2 text-sm text-fg-muted select-none',
    'transition-colors duration-fast-02 ease-productive',
    'hover:bg-bg-hover hover:text-fg',
    'data-[state=on]:bg-bg-selected data-[state=on]:text-fg',
    'disabled:pointer-events-none disabled:text-fg-subtle',
    '[&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
    focusRing,
  ],
  {
    variants: {
      size: {
        sm: 'h-7 min-w-7 px-2 text-xs',
        md: 'h-8 min-w-8 px-3',
        icon: 'size-8 p-0',
      },
    },
    defaultVariants: {
      size: 'md',
    },
  },
)

type ToggleSize = VariantProps<typeof toggleVariants>['size']

const ToggleGroupSize = createContext<ToggleSize>('md')

export function ToggleGroup({
  className,
  size = 'md',
  children,
  ...props
}: ComponentProps<typeof ToggleGroupPrimitive.Root> & { size?: ToggleSize }) {
  return (
    <ToggleGroupPrimitive.Root
      data-slot="toggle-group"
      className={cn('inline-flex items-center border border-border', className)}
      {...props}
    >
      <ToggleGroupSize value={size}>{children}</ToggleGroupSize>
    </ToggleGroupPrimitive.Root>
  )
}

export function ToggleGroupItem({
  className,
  size,
  ...props
}: ComponentProps<typeof ToggleGroupPrimitive.Item> & { size?: ToggleSize }) {
  const groupSize = use(ToggleGroupSize)
  return (
    <ToggleGroupPrimitive.Item
      data-slot="toggle-group-item"
      className={cn(toggleVariants({ size: size ?? groupSize }), className)}
      {...props}
    />
  )
}
