import { transition } from '@holtmail/tokens'
import { AnimatePresence, m } from 'motion/react'
import { Dialog as DialogPrimitive, VisuallyHidden } from 'radix-ui'
import { type ComponentProps, createContext, use } from 'react'

import { cn } from '../lib/cn.ts'
import { fade } from '../lib/motion.tsx'
import { useControllableState } from '../lib/use-controllable-state.ts'

const SheetOpen = createContext(false)

export function Sheet({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  ...props
}: ComponentProps<typeof DialogPrimitive.Root>) {
  const [open, setOpen] = useControllableState({
    value: openProp,
    defaultValue: defaultOpen,
    onChange: onOpenChange,
  })
  return (
    <SheetOpen value={open}>
      <DialogPrimitive.Root data-slot="sheet" open={open} onOpenChange={setOpen} {...props} />
    </SheetOpen>
  )
}

export type SheetContentProps = ComponentProps<typeof DialogPrimitive.Content> & {
  side?: 'left' | 'right'
  title: string
  description?: string
}

export function SheetContent({
  className,
  children,
  side = 'left',
  title,
  description,
  ...props
}: SheetContentProps) {
  const open = use(SheetOpen)
  const offset = side === 'left' ? '-100%' : '100%'
  return (
    <AnimatePresence>
      {open && (
        <DialogPrimitive.Portal key="sheet" forceMount>
          <DialogPrimitive.Overlay forceMount asChild>
            <m.div data-slot="sheet-overlay" className="fixed inset-0 z-50 bg-overlay" {...fade} />
          </DialogPrimitive.Overlay>
          <DialogPrimitive.Content
            data-slot="sheet-content"
            forceMount
            asChild
            {...(description ? {} : { 'aria-describedby': undefined })}
            {...props}
          >
            <m.div
              className={cn(
                'fixed inset-y-0 z-50 flex w-pane-folders max-w-[calc(100%-var(--spacing)*12)] flex-col bg-bg text-fg outline-none',
                side === 'left'
                  ? 'left-0 border-r border-border'
                  : 'right-0 border-l border-border',
                className,
              )}
              initial={{ x: offset }}
              animate={{ x: 0, transition: transition.expressive.moderate02 }}
              exit={{ x: offset, transition: transition.expressiveExit.moderate02 }}
            >
              <VisuallyHidden.Root>
                <DialogPrimitive.Title>{title}</DialogPrimitive.Title>
                {description && (
                  <DialogPrimitive.Description>{description}</DialogPrimitive.Description>
                )}
              </VisuallyHidden.Root>
              {children}
            </m.div>
          </DialogPrimitive.Content>
        </DialogPrimitive.Portal>
      )}
    </AnimatePresence>
  )
}
