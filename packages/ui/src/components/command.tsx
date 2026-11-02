import { Command as CommandPrimitive } from 'cmdk'
import { SearchIcon } from 'lucide-react'
import { VisuallyHidden } from 'radix-ui'
import type { ComponentProps } from 'react'

import { cn } from '../lib/cn.ts'
import { Dialog, DialogContent, DialogDescription, DialogTitle } from './dialog.tsx'

export function Command({ className, ...props }: ComponentProps<typeof CommandPrimitive>) {
  return (
    <CommandPrimitive
      data-slot="command"
      className={cn('flex size-full flex-col overflow-hidden bg-bg text-fg', className)}
      {...props}
    />
  )
}

export type CommandDialogProps = ComponentProps<typeof Dialog> & {
  title?: string
  description?: string
  className?: string
  commandProps?: ComponentProps<typeof Command>
}

export function CommandDialog({
  title = 'Command palette',
  description = 'Search for a command to run',
  className,
  commandProps,
  children,
  ...props
}: CommandDialogProps) {
  return (
    <Dialog {...props}>
      <DialogContent
        showClose={false}
        className={cn('top-[20%] max-w-xl translate-y-0 gap-0 p-0', className)}
      >
        <VisuallyHidden.Root>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </VisuallyHidden.Root>
        <Command {...commandProps}>{children}</Command>
      </DialogContent>
    </Dialog>
  )
}

export function CommandInput({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Input>) {
  return (
    <div
      data-slot="command-input-wrapper"
      className="flex h-12 items-center gap-3 border-b border-border px-4 text-fg-subtle [&_svg]:size-4 [&_svg]:shrink-0"
    >
      <SearchIcon />
      <CommandPrimitive.Input
        data-slot="command-input"
        className={cn(
          'h-full w-full bg-transparent text-base text-fg outline-none placeholder:text-fg-subtle disabled:cursor-not-allowed',
          className,
        )}
        {...props}
      />
    </div>
  )
}

export function CommandList({ className, ...props }: ComponentProps<typeof CommandPrimitive.List>) {
  return (
    <CommandPrimitive.List
      data-slot="command-list"
      className={cn(
        'max-h-96 scroll-py-1 overflow-x-hidden overflow-y-auto py-1',
        'h-(--cmdk-list-height) transition-[height] duration-fast-02 ease-productive',
        className,
      )}
      {...props}
    />
  )
}

export function CommandEmpty({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Empty>) {
  return (
    <CommandPrimitive.Empty
      data-slot="command-empty"
      className={cn('px-4 py-6 text-sm text-fg-subtle', className)}
      {...props}
    />
  )
}

export function CommandGroup({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Group>) {
  return (
    <CommandPrimitive.Group
      data-slot="command-group"
      className={cn(
        'overflow-hidden text-fg',
        '**:[[cmdk-group-heading]]:flex **:[[cmdk-group-heading]]:h-8 **:[[cmdk-group-heading]]:items-center **:[[cmdk-group-heading]]:px-4 **:[[cmdk-group-heading]]:text-xs **:[[cmdk-group-heading]]:text-fg-subtle',
        className,
      )}
      {...props}
    />
  )
}

export function CommandSeparator({
  className,
  ...props
}: ComponentProps<typeof CommandPrimitive.Separator>) {
  return (
    <CommandPrimitive.Separator
      data-slot="command-separator"
      className={cn('my-1 h-px bg-border', className)}
      {...props}
    />
  )
}

export function CommandItem({ className, ...props }: ComponentProps<typeof CommandPrimitive.Item>) {
  return (
    <CommandPrimitive.Item
      data-slot="command-item"
      className={cn(
        'relative flex h-10 cursor-default items-center gap-3 px-4 text-sm outline-none select-none',
        'data-[selected=true]:bg-bg-hover data-[disabled=true]:pointer-events-none data-[disabled=true]:text-fg-subtle',
        '[&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-fg-muted',
        className,
      )}
      {...props}
    />
  )
}

export function CommandShortcut({ className, ...props }: ComponentProps<'span'>) {
  return (
    <span
      data-slot="command-shortcut"
      className={cn('ml-auto font-mono text-xs text-fg-subtle', className)}
      {...props}
    />
  )
}
