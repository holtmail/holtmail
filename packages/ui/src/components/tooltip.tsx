import { AnimatePresence, m } from 'motion/react'
import { Tooltip as TooltipPrimitive } from 'radix-ui'
import { type ComponentProps, createContext, use } from 'react'

import { cn } from '../lib/cn.ts'
import { hint } from '../lib/motion.tsx'
import { useControllableState } from '../lib/use-controllable-state.ts'

const TooltipOpen = createContext(false)

export function TooltipProvider({
  delayDuration = 400,
  ...props
}: ComponentProps<typeof TooltipPrimitive.Provider>) {
  return <TooltipPrimitive.Provider delayDuration={delayDuration} {...props} />
}

export function Tooltip({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  ...props
}: ComponentProps<typeof TooltipPrimitive.Root>) {
  const [open, setOpen] = useControllableState({
    value: openProp,
    defaultValue: defaultOpen,
    onChange: onOpenChange,
  })
  return (
    <TooltipOpen value={open}>
      <TooltipPrimitive.Root data-slot="tooltip" open={open} onOpenChange={setOpen} {...props} />
    </TooltipOpen>
  )
}

export function TooltipTrigger(props: ComponentProps<typeof TooltipPrimitive.Trigger>) {
  return <TooltipPrimitive.Trigger data-slot="tooltip-trigger" {...props} />
}

export function TooltipContent({
  className,
  sideOffset = 4,
  children,
  ...props
}: ComponentProps<typeof TooltipPrimitive.Content>) {
  const open = use(TooltipOpen)
  return (
    <AnimatePresence>
      {open && (
        <TooltipPrimitive.Portal key="tooltip" forceMount>
          <TooltipPrimitive.Content
            data-slot="tooltip-content"
            sideOffset={sideOffset}
            forceMount
            asChild
            {...props}
          >
            <m.div
              className={cn(
                'z-50 max-w-xs bg-fg px-2 py-1 text-xs text-bg select-none',
                'origin-(--radix-tooltip-content-transform-origin)',
                className,
              )}
              {...hint}
            >
              {children}
            </m.div>
          </TooltipPrimitive.Content>
        </TooltipPrimitive.Portal>
      )}
    </AnimatePresence>
  )
}
