import { XIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { Dialog as DialogPrimitive } from 'radix-ui'
import { type ComponentProps, createContext, use } from 'react'

import { cn } from '../lib/cn.ts'
import { fade, rise } from '../lib/motion.tsx'
import { useControllableState } from '../lib/use-controllable-state.ts'
import { focusRing } from './focus.ts'

const DialogOpen = createContext(false)

export function Dialog({
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
    <DialogOpen value={open}>
      <DialogPrimitive.Root data-slot="dialog" open={open} onOpenChange={setOpen} {...props} />
    </DialogOpen>
  )
}

export function DialogTrigger(props: ComponentProps<typeof DialogPrimitive.Trigger>) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

export function DialogClose(props: ComponentProps<typeof DialogPrimitive.Close>) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

export type DialogContentProps = ComponentProps<typeof DialogPrimitive.Content> & {
  showClose?: boolean
}

export function DialogContent({
  className,
  children,
  showClose = true,
  ...props
}: DialogContentProps) {
  const open = use(DialogOpen)
  return (
    <AnimatePresence>
      {open && (
        <DialogPrimitive.Portal key="dialog" forceMount>
          <DialogPrimitive.Overlay forceMount asChild>
            <m.div data-slot="dialog-overlay" className="fixed inset-0 z-50 bg-overlay" {...fade} />
          </DialogPrimitive.Overlay>
          <DialogPrimitive.Content data-slot="dialog-content" forceMount asChild {...props}>
            <m.div
              className={cn(
                'fixed top-1/2 left-1/2 z-50 grid w-[calc(100%-var(--spacing)*8)] max-w-lg -translate-x-1/2 -translate-y-1/2',
                'gap-4 border border-border bg-bg p-6 text-fg outline-none',
                className,
              )}
              {...rise}
            >
              {children}
              {showClose && (
                <DialogPrimitive.Close
                  data-slot="dialog-close"
                  className={cn(
                    'absolute top-3 right-3 inline-flex size-8 items-center justify-center text-fg-muted',
                    'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover hover:text-fg',
                    '[&_svg]:size-4',
                    focusRing,
                  )}
                >
                  <XIcon />
                  <span className="sr-only">Close</span>
                </DialogPrimitive.Close>
              )}
            </m.div>
          </DialogPrimitive.Content>
        </DialogPrimitive.Portal>
      )}
    </AnimatePresence>
  )
}

export function DialogHeader({ className, ...props }: ComponentProps<'div'>) {
  return <div data-slot="dialog-header" className={cn('grid gap-1 pr-8', className)} {...props} />
}

export function DialogFooter({ className, ...props }: ComponentProps<'div'>) {
  return (
    <div
      data-slot="dialog-footer"
      className={cn('flex flex-col-reverse gap-2 sm:flex-row sm:justify-end', className)}
      {...props}
    />
  )
}

export function DialogTitle({ className, ...props }: ComponentProps<typeof DialogPrimitive.Title>) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn('text-lg font-normal text-fg', className)}
      {...props}
    />
  )
}

export function DialogDescription({
  className,
  ...props
}: ComponentProps<typeof DialogPrimitive.Description>) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn('text-sm text-fg-muted', className)}
      {...props}
    />
  )
}
