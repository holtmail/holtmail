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
