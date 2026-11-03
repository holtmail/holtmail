# @holtmail/ui

The Holtmail component library: shadcn/ui components copied into the repository and restyled to `@holtmail/tokens`. Radix primitives (the `radix-ui` package) handle accessibility, focus and keyboard; cmdk powers the command palette; Motion (`motion/react`, through `LazyMotion` and `m`) animates enter and exit. Components use only token utilities: no raw hex, no radius, no shadows. The package exports TypeScript source; the app bundles it.

## Setup

```css
/* app.css, after the tokens */
@import '@holtmail/ui/styles.css';
```

`styles.css` adds this package to Tailwind's source scan. Wrap the app once:

```tsx
<MotionProvider>
  <TooltipProvider>{app}</TooltipProvider>
</MotionProvider>
```

`MotionProvider` is `LazyMotion` with `domAnimation` in strict mode (so only `m.*` components are allowed) and `MotionConfig reducedMotion="user"`: with `prefers-reduced-motion: reduce`, movement is dropped and only opacity fades remain.

## Helpers

| Export                 | Signature                                               | Guarantee                                                                                                                                                                                       |
| ---------------------- | ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cn`                   | `(...ClassValue[]) => string`                           | `clsx` then `tailwind-merge`, aware of token utilities: later classes win per group, including `duration-*`, `ease-*`, `w-pane-*`, `h-topbar`; font size and text color never cancel each other |
| `useControllableState` | `({ value?, defaultValue, onChange? }) => [value, set]` | Controlled when `value` is defined, uncontrolled otherwise; `set` always calls `onChange`                                                                                                       |
| `MotionProvider`       | `{ children }`                                          | See above                                                                                                                                                                                       |

## Components

All components accept the props of the element or Radix part they wrap, plus `className`. Each sets `data-slot` for targeting.

### Button

`variant`: `primary` (accent), `secondary` (default, hairline outline), `ghost`, `danger`. `size`: `sm` (28px), `md` (default, 32px), `icon` (32px square). `asChild` renders the child element instead of a `<button>`. `type` defaults to `button`. `buttonVariants({ variant, size })` returns the classes for other elements.

### Input

A text field with a hairline border on the subtle background. Sets a danger border when `aria-invalid` is true.

### Kbd

A key cap for shortcuts, in Plex Mono.

### Badge

`variant`: `neutral` (default), `outline`, `accent`, `danger`, `warning`, `success`. `asChild` supported. `badgeVariants` exported.

### Switch

An on/off control: a 36×20px square track with a hairline border and a square thumb that slides 16px. Checked fills the track with the accent. Props of the Radix `Switch.Root` (`checked`, `defaultChecked`, `onCheckedChange`, `disabled`). Pair it with a `<label htmlFor>`.

### RadioGroup

`RadioGroup` (`value`, `defaultValue`, `onValueChange`; a grid with 8px gaps) and `RadioGroupItem` (`value`): a 16px square with a hairline border and an 8px accent square when checked. Arrow keys move between items.

### Separator

A 1px hairline. `orientation`: `horizontal` (default) or `vertical`; `decorative` defaults to `true`.

### Tooltip

`TooltipProvider` (`delayDuration` defaults to 400ms), `Tooltip` (`open`, `defaultOpen`, `onOpenChange`), `TooltipTrigger`, `TooltipContent` (`sideOffset` defaults to 4). Inverse colors; fades with `productive` fast-02 in, fast-01 out.

### DropdownMenu

`DropdownMenu` (`open`, `defaultOpen`, `onOpenChange`), `DropdownMenuTrigger`, `DropdownMenuContent` (`align` defaults to `start`, `sideOffset` to 4), `DropdownMenuGroup`, `DropdownMenuItem` (`variant`: `default` or `danger`; `inset`), `DropdownMenuCheckboxItem`, `DropdownMenuRadioGroup`, `DropdownMenuRadioItem`, `DropdownMenuLabel` (`inset`), `DropdownMenuSeparator`, `DropdownMenuShortcut`, `DropdownMenuSub` (`open`, `defaultOpen`, `onOpenChange`), `DropdownMenuSubTrigger` (`inset`), `DropdownMenuSubContent`. Menus fade and drop 4px in with `productive` fast-02 and fade out with fast-01.

### Dialog

`Dialog` (`open`, `defaultOpen`, `onOpenChange`), `DialogTrigger`, `DialogClose`, `DialogContent` (`showClose` defaults to `true`), `DialogHeader`, `DialogFooter`, `DialogTitle`, `DialogDescription`. The overlay fades; the panel fades and rises 8px with `productive` moderate-01 in and the exit curve with fast-02 out. Every dialog needs a `DialogTitle` for screen readers.

### Command

A command palette on cmdk: `Command`, `CommandDialog` (Dialog props plus `title`, `description` for screen readers, `className` for the panel, `commandProps` for the cmdk root), `CommandInput`, `CommandList`, `CommandEmpty`, `CommandGroup` (`heading`), `CommandItem` (`onSelect`, `value`, `keywords`, `disabled`), `CommandSeparator`, `CommandShortcut`. `CommandDialog` opens near the top of the screen and animates like `Dialog`.

### Sheet

A side panel on the Radix dialog: `Sheet` (`open`, `defaultOpen`, `onOpenChange`) and `SheetContent` (`side`: `left` (default) or `right`; `title` and optional `description`, both visually hidden for screen readers). The panel is `w-pane-folders` wide with a hairline on its inner edge; it slides in from its side with the `expressive` moderate-02 curve and out with the exit curve while the overlay fades. Under reduced motion only the fade remains.

### ScrollArea

`ScrollArea` wraps content in a Radix viewport with a square, hairline-thin vertical scrollbar. `ScrollBar` (`orientation`) adds a horizontal one.

### Skeleton

A placeholder block with a pulse; the pulse stops under reduced motion.

### ToggleGroup

`ToggleGroup` (Radix `type="single" | "multiple"`, `value`, `onValueChange`, `size`: `sm`, `md`, `icon`) and `ToggleGroupItem` (`value`, optional `size` overriding the group). Pressed items use the selected background. `toggleVariants` exported.

## Adding a component

Copy it from shadcn/ui (or Animate UI, Motion Primitives), then: replace every color with a semantic token utility, remove `rounded-*` and `shadow-*`, use `focusRing` from `components/focus.ts` for focus, and route enter and exit through `m` with presets from `lib/motion.tsx`. Export it from `src/index.ts` and list it here.

## Tests

`bun test packages/ui` covers `cn`.
