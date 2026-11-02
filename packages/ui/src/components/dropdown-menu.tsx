import { CheckIcon, ChevronRightIcon, DotIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'
import { DropdownMenu as DropdownMenuPrimitive } from 'radix-ui'
import { type ComponentProps, createContext, use } from 'react'

import { cn } from '../lib/cn.ts'
import { drop } from '../lib/motion.tsx'
import { useControllableState } from '../lib/use-controllable-state.ts'

const MenuOpen = createContext(false)
const SubMenuOpen = createContext(false)

const panel = cn(
  'z-50 min-w-48 overflow-x-hidden overflow-y-auto border border-border bg-bg py-1 text-fg',
  'max-h-(--radix-dropdown-menu-content-available-height) origin-(--radix-dropdown-menu-content-transform-origin)',
)

const item = cn(
  'relative flex h-8 cursor-default items-center gap-2 px-3 text-sm outline-none select-none',
  'data-highlighted:bg-bg-hover data-disabled:pointer-events-none data-disabled:text-fg-subtle',
  '[&_svg]:pointer-events-none [&_svg]:size-4 [&_svg]:shrink-0',
)

export function DropdownMenu({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Root>) {
  const [open, setOpen] = useControllableState({
    value: openProp,
    defaultValue: defaultOpen,
    onChange: onOpenChange,
  })
  return (
    <MenuOpen value={open}>
      <DropdownMenuPrimitive.Root
        data-slot="dropdown-menu"
        open={open}
        onOpenChange={setOpen}
        {...props}
      />
    </MenuOpen>
  )
}

export function DropdownMenuTrigger(props: ComponentProps<typeof DropdownMenuPrimitive.Trigger>) {
  return <DropdownMenuPrimitive.Trigger data-slot="dropdown-menu-trigger" {...props} />
}

export function DropdownMenuContent({
  className,
  sideOffset = 4,
  align = 'start',
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Content>) {
  const open = use(MenuOpen)
  return (
    <AnimatePresence>
      {open && (
        <DropdownMenuPrimitive.Portal key="menu" forceMount>
          <DropdownMenuPrimitive.Content
            data-slot="dropdown-menu-content"
            sideOffset={sideOffset}
            align={align}
            forceMount
            asChild
            {...props}
          >
            <m.div className={cn(panel, className)} {...drop}>
              {children}
            </m.div>
          </DropdownMenuPrimitive.Content>
        </DropdownMenuPrimitive.Portal>
      )}
    </AnimatePresence>
  )
}

export function DropdownMenuGroup(props: ComponentProps<typeof DropdownMenuPrimitive.Group>) {
  return <DropdownMenuPrimitive.Group data-slot="dropdown-menu-group" {...props} />
}

export type DropdownMenuItemProps = ComponentProps<typeof DropdownMenuPrimitive.Item> & {
  inset?: boolean
  variant?: 'default' | 'danger'
}

export function DropdownMenuItem({
  className,
  inset,
  variant = 'default',
  ...props
}: DropdownMenuItemProps) {
  return (
    <DropdownMenuPrimitive.Item
      data-slot="dropdown-menu-item"
      data-variant={variant}
      className={cn(
        item,
        inset && 'pl-9',
        variant === 'danger' &&
          'text-danger data-highlighted:bg-danger data-highlighted:text-danger-fg',
        className,
      )}
      {...props}
    />
  )
}

export function DropdownMenuCheckboxItem({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.CheckboxItem>) {
  return (
    <DropdownMenuPrimitive.CheckboxItem
      data-slot="dropdown-menu-checkbox-item"
      className={cn(item, 'pl-9', className)}
      {...props}
    >
      <span className="absolute left-3 flex size-4 items-center justify-center">
        <DropdownMenuPrimitive.ItemIndicator>
          <CheckIcon />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {children}
    </DropdownMenuPrimitive.CheckboxItem>
  )
}

export function DropdownMenuRadioGroup(
  props: ComponentProps<typeof DropdownMenuPrimitive.RadioGroup>,
) {
  return <DropdownMenuPrimitive.RadioGroup data-slot="dropdown-menu-radio-group" {...props} />
}

export function DropdownMenuRadioItem({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.RadioItem>) {
  return (
    <DropdownMenuPrimitive.RadioItem
      data-slot="dropdown-menu-radio-item"
      className={cn(item, 'pl-9', className)}
      {...props}
    >
      <span className="absolute left-3 flex size-4 items-center justify-center">
        <DropdownMenuPrimitive.ItemIndicator>
          <DotIcon />
        </DropdownMenuPrimitive.ItemIndicator>
      </span>
      {children}
    </DropdownMenuPrimitive.RadioItem>
  )
}

export function DropdownMenuLabel({
  className,
  inset,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Label> & { inset?: boolean }) {
  return (
    <DropdownMenuPrimitive.Label
      data-slot="dropdown-menu-label"
      className={cn(
        'flex h-8 items-center px-3 text-xs text-fg-subtle',
        inset && 'pl-9',
        className,
      )}
      {...props}
    />
  )
}

export function DropdownMenuSeparator({
  className,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Separator>) {
  return (
    <DropdownMenuPrimitive.Separator
      data-slot="dropdown-menu-separator"
      className={cn('my-1 h-px bg-border', className)}
      {...props}
    />
  )
}

export function DropdownMenuShortcut({ className, ...props }: ComponentProps<'span'>) {
  return (
    <span
      data-slot="dropdown-menu-shortcut"
      className={cn('ml-auto pl-4 font-mono text-xs text-fg-subtle', className)}
      {...props}
    />
  )
}

export function DropdownMenuSub({
  open: openProp,
  defaultOpen = false,
  onOpenChange,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.Sub>) {
  const [open, setOpen] = useControllableState({
    value: openProp,
    defaultValue: defaultOpen,
    onChange: onOpenChange,
  })
  return (
    <SubMenuOpen value={open}>
      <DropdownMenuPrimitive.Sub
        data-slot="dropdown-menu-sub"
        open={open}
        onOpenChange={setOpen}
        {...props}
      />
    </SubMenuOpen>
  )
}

export function DropdownMenuSubTrigger({
  className,
  inset,
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.SubTrigger> & { inset?: boolean }) {
  return (
    <DropdownMenuPrimitive.SubTrigger
      data-slot="dropdown-menu-sub-trigger"
      className={cn(item, 'data-[state=open]:bg-bg-hover', inset && 'pl-9', className)}
      {...props}
    >
      {children}
      <ChevronRightIcon className="ml-auto" />
    </DropdownMenuPrimitive.SubTrigger>
  )
}

export function DropdownMenuSubContent({
  className,
  children,
  ...props
}: ComponentProps<typeof DropdownMenuPrimitive.SubContent>) {
  const open = use(SubMenuOpen)
  return (
    <AnimatePresence>
      {open && (
        <DropdownMenuPrimitive.Portal key="submenu" forceMount>
          <DropdownMenuPrimitive.SubContent
            data-slot="dropdown-menu-sub-content"
            forceMount
            asChild
            {...props}
          >
            <m.div className={cn(panel, className)} {...drop}>
              {children}
            </m.div>
          </DropdownMenuPrimitive.SubContent>
        </DropdownMenuPrimitive.Portal>
      )}
    </AnimatePresence>
  )
}
