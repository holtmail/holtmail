import { RadioGroup as RadioGroupPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'
import { focusRing } from './focus.ts'

export function RadioGroup({
  className,
  ...props
}: ComponentProps<typeof RadioGroupPrimitive.Root>) {
  return (
    <RadioGroupPrimitive.Root
      data-slot="radio-group"
      className={cn('grid gap-2', className)}
      {...props}
    />
  )
}

export function RadioGroupItem({
  className,
  ...props
}: ComponentProps<typeof RadioGroupPrimitive.Item>) {
  return (
    <RadioGroupPrimitive.Item
      data-slot="radio-group-item"
      className={cn(
        'flex size-4 shrink-0 cursor-default items-center justify-center border border-border-strong bg-bg',
        'transition-colors duration-fast-02 ease-productive',
        'data-[state=checked]:border-accent',
        'disabled:pointer-events-none disabled:border-border',
        focusRing,
        className,
      )}
      {...props}
    >
      <RadioGroupPrimitive.Indicator
        data-slot="radio-group-indicator"
        className="size-2 bg-accent"
      />
    </RadioGroupPrimitive.Item>
  )
}
