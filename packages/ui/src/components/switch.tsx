import { Switch as SwitchPrimitive } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'
import { focusRing } from './focus.ts'

export function Switch({ className, ...props }: ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      className={cn(
        'relative inline-flex h-5 w-9 shrink-0 cursor-default items-center border border-border-strong bg-bg-subtle',
        'transition-colors duration-fast-02 ease-productive',
        'data-[state=checked]:border-accent data-[state=checked]:bg-accent',
        'disabled:pointer-events-none disabled:border-border disabled:bg-bg-subtle',
        focusRing,
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        data-slot="switch-thumb"
        className={cn(
          'pointer-events-none block size-3.5 translate-x-0.5 bg-fg-muted',
          'transition-transform duration-fast-02 ease-productive',
          'data-[state=checked]:translate-x-[1.125rem] data-[state=checked]:bg-accent-fg',
        )}
      />
    </SwitchPrimitive.Root>
  )
}
