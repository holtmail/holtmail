# @holtmail/tokens

Design tokens for the Holtmail interface: color, type, spacing, radius, layout and motion as CSS custom properties, the same values as TypeScript constants for code that animates or draws, and a Tailwind CSS v4 `@theme` that turns them into utilities. Every color, size and duration in `@holtmail/ui` and `apps/web` comes from here. The look follows IBM Carbon: square corners, hairline borders, IBM Plex, Carbon's motion curves. See [docs/design.md](../../../docs/design.md).

## Files

| Export                        | File             | What it holds                                                                                 |
| ----------------------------- | ---------------- | --------------------------------------------------------------------------------------------- |
| `@holtmail/tokens/fonts.css`  | `src/fonts.css`  | Self-hosted IBM Plex Sans and Mono, weights 400, 500 and 600 (Fontsource)                     |
| `@holtmail/tokens/tokens.css` | `src/tokens.css` | All `--hm-*` custom properties and the light, dark and reduced-motion blocks                  |
| `@holtmail/tokens/theme.css`  | `src/theme.css`  | Tailwind v4 `@theme inline` mapping, the `dark` variant, `duration-*` utility and base styles |
| `@holtmail/tokens`            | `src/index.ts`   | TypeScript constants                                                                          |

An app imports them after Tailwind:

```css
@import 'tailwindcss';
@import '@holtmail/tokens/fonts.css';
@import '@holtmail/tokens/tokens.css';
@import '@holtmail/tokens/theme.css';
```

## Themes

- `:root` holds the light theme.
- `@media (prefers-color-scheme: dark)` applies the dark theme to `:root:not([data-theme='light'])`, so the system setting wins unless the user forced light.
- `:root[data-theme='dark']` forces dark.
- No `data-theme` attribute means "follow the system".

The Tailwind `dark:` variant follows the same rules. Components should not need it: semantic tokens already switch with the theme.

## Color

Primitive scales (`--hm-gray-*`, `--hm-blue-*`, `--hm-red-*`, `--hm-yellow-*`, `--hm-green-*`) are the only place raw hex values live. Use them only to define semantic tokens.

| Semantic token             | Utility examples           | Use                                                     |
| -------------------------- | -------------------------- | ------------------------------------------------------- |
| `--hm-color-bg`            | `bg-bg`                    | Page and panel background                               |
| `--hm-color-bg-subtle`     | `bg-bg-subtle`             | Fields, secondary surfaces                              |
| `--hm-color-bg-hover`      | `hover:bg-bg-hover`        | Hovered rows and controls                               |
| `--hm-color-bg-selected`   | `bg-bg-selected`           | Selected rows, pressed toggles                          |
| `--hm-color-fg`            | `text-fg`                  | Primary text                                            |
| `--hm-color-fg-muted`      | `text-fg-muted`            | Secondary text                                          |
| `--hm-color-fg-subtle`     | `text-fg-subtle`           | Placeholders, hints, disabled text                      |
| `--hm-color-border`        | `border-border`            | Hairline dividers                                       |
| `--hm-color-border-strong` | `border-border-strong`     | Field and secondary button outlines                     |
| `--hm-color-accent`        | `bg-accent`, `text-accent` | The one accent: focus, selection, links, primary action |
| `--hm-color-accent-hover`  | `hover:bg-accent-hover`    | Hovered accent surfaces                                 |
| `--hm-color-accent-fg`     | `text-accent-fg`           | Text on accent                                          |
| `--hm-color-focus`         | `outline-focus`            | Focus ring                                              |
| `--hm-color-danger`        | `bg-danger`, `text-danger` | Destructive actions and errors                          |
| `--hm-color-danger-hover`  | `hover:bg-danger-hover`    | Hovered danger surfaces                                 |
| `--hm-color-danger-fg`     | `text-danger-fg`           | Text on danger                                          |
| `--hm-color-warning`       | `text-warning`             | Warnings                                                |
| `--hm-color-success`       | `text-success`             | Success states                                          |
| `--hm-color-overlay`       | `bg-overlay`               | Backdrop behind dialogs                                 |

Tailwind's default palette is removed (`--color-*: initial`), so `bg-white` or `text-gray-500` do not exist.

## Typography

| Token                                                | Value                                                           |
| ---------------------------------------------------- | --------------------------------------------------------------- |
| `--hm-font-sans`                                     | IBM Plex Sans with system fallbacks (`font-sans`)               |
| `--hm-font-mono`                                     | IBM Plex Mono with system fallbacks (`font-mono`)               |
| `--hm-font-weight-regular` / `-medium` / `-semibold` | 400 / 500 / 600 (`font-normal`, `font-medium`, `font-semibold`) |

The type scale is strict; Tailwind's default sizes are removed. Each size carries its own line height and tracking.

| Utility     | Size | Line height | Tracking |
| ----------- | ---- | ----------- | -------- |
| `text-xs`   | 12px | 16px        | 0.32px   |
| `text-sm`   | 14px | 20px        | 0.16px   |
| `text-base` | 16px | 24px        | 0        |
| `text-lg`   | 20px | 28px        | 0        |
| `text-xl`   | 24px | 32px        | 0        |
| `text-2xl`  | 32px | 40px        | 0        |

## Spacing

A 4px grid. `--hm-space-unit` (0.25rem) is Tailwind's `--spacing`, so `p-1` is 4px, `p-2` 8px, `p-4` 16px. Named steps `--hm-space-1`, `-2`, `-3`, `-4`, `-6`, `-8`, `-10`, `-12`, `-16` match those utilities.

## Radius, borders and shadows

- `--hm-radius` is `0`, and every Tailwind radius (`rounded`, `rounded-sm` ... `rounded-4xl`) resolves to it.
- `--hm-border-width` is `1px`: borders are hairlines.
- `--hm-focus-width` is `2px`: the focus ring is an inset outline (`outline-2 -outline-offset-2 outline-focus`), the only "shadow" allowed.
- All shadow scales (`shadow-*`, `inset-shadow-*`, `drop-shadow-*`, `text-shadow-*`) are removed.

## Layout

| Token                      | Utility          | Value |
| -------------------------- | ---------------- | ----- |
| `--hm-layout-topbar`       | `h-topbar`       | 3rem  |
| `--hm-layout-pane-folders` | `w-pane-folders` | 15rem |
| `--hm-layout-pane-list`    | `w-pane-list`    | 26rem |

## Motion

As in [design.md](../../../docs/design.md#motion): two modes, five durations.

| Token                       | Utility                | Value                             |
| --------------------------- | ---------------------- | --------------------------------- |
| `--hm-ease-productive`      | `ease-productive`      | `cubic-bezier(0.2, 0, 0.38, 0.9)` |
| `--hm-ease-productive-exit` | `ease-productive-exit` | `cubic-bezier(0.62, 0.1, 0.8, 1)` |
| `--hm-ease-expressive`      | `ease-expressive`      | `cubic-bezier(0.4, 0.14, 0.3, 1)` |
| `--hm-ease-expressive-exit` | `ease-expressive-exit` | `cubic-bezier(0.7, 0, 0.6, 0.86)` |
| `--hm-duration-fast-01`     | `duration-fast-01`     | 70ms, productive                  |
| `--hm-duration-fast-02`     | `duration-fast-02`     | 110ms, productive                 |
| `--hm-duration-moderate-01` | `duration-moderate-01` | 150ms, productive                 |
| `--hm-duration-moderate-02` | `duration-moderate-02` | 240ms, expressive                 |
| `--hm-duration-slow-01`     | `duration-slow-01`     | 400ms, expressive                 |

Exit curves are the entrance curves reversed in time (`x1, y1, x2, y2` becomes `1-x2, 1-y2, 1-x1, 1-y1`): they start slowly where entrances end slowly. Under `prefers-reduced-motion: reduce` every `--hm-duration-*` is `0ms`.

## TypeScript API

| Export                                              | Type                            | Guarantee                                                                                                                                                                      |
| --------------------------------------------------- | ------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `semanticColors`                                    | readonly tuple of names         | Every name has a `--hm-color-<name>` in each theme block (checked by tests)                                                                                                    |
| `colorVar(name)`                                    | `(SemanticColor) => string`     | Returns `var(--hm-color-<name>)`, so it follows the active theme                                                                                                               |
| `themes`, `Theme`                                   | `'light' \| 'dark' \| 'system'` | The values `data-theme` and the theme switcher accept                                                                                                                          |
| `font`, `fontWeight`, `fontSize`, `space`, `radius` | constants                       | Same values as the CSS tokens                                                                                                                                                  |
| `ease`                                              | cubic-bezier tuples             | `productive`, `productiveExit`, `expressive`, `expressiveExit`; equal to the CSS easings (checked by tests)                                                                    |
| `duration`                                          | milliseconds                    | `fast01`, `fast02`, `moderate01`, `moderate02`, `slow01`; equal to the CSS durations (checked by tests)                                                                        |
| `distance`                                          | pixels                          | `xs` (4) and `sm` (8): offsets for enter and exit movement                                                                                                                     |
| `transition`                                        | `{ duration (s), ease }`        | Motion presets: `transition.productive.fast01 \| fast02 \| moderate01`, `transition.expressive.moderate02 \| slow01`, and the same under `productiveExit` and `expressiveExit` |
| `cssCubicBezier(curve)`                             | `string`                        | `cubic-bezier(...)` text for a curve                                                                                                                                           |

```tsx
import { transition } from '@holtmail/tokens'

;<m.div animate={{ opacity: 1, transition: transition.productive.moderate01 }} />
```

## Tests

`bun test packages/tokens` checks that the CSS and TypeScript agree: every semantic color in every theme, identical system-dark and forced-dark blocks, durations, easings, reduced motion, and that every radius is zero.
