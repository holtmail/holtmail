# Interface design

Holtmail should feel light, quiet and precise: a well-set page of text rather than an app full of widgets. People spend hours reading mail, so the interface stays out of the way.

## Principles

- **Square corners.** Every radius is `0`. Shapes are drawn by hairlines and whitespace, not by rounded boxes.
- **Hairlines, not shadows.** `1px` borders in a low-contrast neutral separate areas. No drop shadows except a focus ring; no gradients.
- **Typography carries the design.** One sans serif for the interface, one monospace for ids, headers and raw source. A strict type scale and a baseline grid; weight and size do the work color would otherwise do.
- **Few colors.** A neutral gray scale and one accent, used for focus, selection and links only. Light and dark themes are designed together, both first-class.
- **Density on purpose.** A message list shows many rows without feeling cramped: tight rows, generous margins, aligned columns.
- **Fast and keyboard-first.** Every action has a shortcut; lists are virtualized; navigation never waits on a spinner when cached data exists.
- **Tokens only.** Components take colors, sizes, spacing and motion from `@holtmail/tokens`. A raw hex value or pixel size in a component is a bug.

## Stack

| Layer | Choice | Notes |
| --- | --- | --- |
| Look | IBM Carbon as the reference | Square corners, IBM Plex, a 2x grid, Carbon's motion curves; we borrow the language, not the `@carbon/react` package |
| Components | shadcn/ui, copied into `@holtmail/ui` | Restyled to tokens; owned code, not a dependency |
| Primitives | Radix | Accessibility, focus management and keyboard handling |
| Styling | Tailwind CSS v4 | `@theme` reads CSS variables from `@holtmail/tokens`; `--radius` is `0` |
| Motion | Motion (`motion/react`) through `LazyMotion` and `m` | Layout animations, `AnimatePresence`, springs, gestures |
| Animated components | Animate UI, Motion Primitives as a source of ideas | Taken one by one through the shadcn CLI and restyled; nothing glowing or particle-based |
| Page transitions | View Transitions API | List → message and folder changes |
| Type | IBM Plex Sans, IBM Plex Mono | Self-hosted, no font CDN |

## Motion

Two modes, as in Carbon, both defined as tokens in `@holtmail/tokens`:

| Mode | Use | Easing | Durations |
| --- | --- | --- | --- |
| Productive | Everyday actions: selecting a message, menus, hovers, toggles | `cubic-bezier(0.2, 0, 0.38, 0.9)` | 70, 110, 150 ms |
| Expressive | Moments that matter: opening a message, sync finished, empty states, first run | `cubic-bezier(0.4, 0.14, 0.3, 1)` | 240, 400 ms |

Exits use the same curves with entrance and exit swapped. Rules:

- Animate only `transform` and `opacity`. Layout animations go through Motion's `layout`, never through animating `width`, `height` or `top`.
- No animation inside a virtualized list while it scrolls.
- `prefers-reduced-motion: reduce` replaces every movement with an instant change or a short fade.
- Motion explains what changed: where a message went, what opened, what finished. An animation that does not explain anything is removed.

## Mail clients to learn from

Holtmail should feel like a mail client people love, so the interaction model comes from mail clients and the visual language from Carbon.

| Client | Open source | What to take |
| --- | --- | --- |
| Zero | Yes, https://github.com/Mail-0/Zero | A modern mail client on React and shadcn/ui: proof that this stack can look like a real mail app |
| Thunderbird | Yes, https://www.thunderbird.net | Folder trees, multiple accounts, power-user settings done without clutter |
| Apple Mail | No | The calm three-pane layout and a list everyone already understands |
| Superhuman | No | Keyboard-first flow, the command palette, speed as a feature |
| HEY | No | Bold reading typography and a strong opinion about what a list should show |
| Mimestream | No | A native-feeling Gmail client: labels, density and polish |

## Open source references

Products and design systems whose look is close to what we want, and what to take from each.

| Reference | Link | What to take |
| --- | --- | --- |
| IBM Carbon Design System | https://carbondesignsystem.com | Zero radius as a system rule, a 2x grid, IBM Plex, dense data tables that stay calm |
| Oxide Computer and its web console | https://oxide.computer, https://github.com/oxidecomputer/console | Sharp, technical elegance; monospace labels; a dark theme with one accent; tables for real data |
| Zed | https://zed.dev | Editorial layout, hairline dividers, restrained color, very fast feel |
| Ghostty | https://ghostty.org | A quiet product page where typography and spacing do everything |
| Tufte CSS | https://edwardtufte.github.io/tufte-css/ | Reading typography: measure, sidenotes, a page that is all text and still beautiful |
| sourcehut | https://sr.ht | How light a useful interface can be: no ornament, instant pages |
| htmx | https://htmx.org | Confident plainness and sharp boxes, with personality kept to the copy |
| Radix Colors | https://www.radix-ui.com/colors | Accessible neutral and accent scales for light and dark, as a base for our tokens |
| shadcn/ui | https://ui.shadcn.com | Accessible component patterns we can restyle with `--radius: 0` |
| Stalwart | https://stalw.art | The mail domain: how a modern Rust mail server presents accounts, queues and settings |

Typefaces to try: IBM Plex Sans and Mono, Geist and Geist Mono, Inter with JetBrains Mono. All are under the SIL Open Font License.
