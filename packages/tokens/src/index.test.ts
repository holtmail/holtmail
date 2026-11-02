import { describe, expect, test } from 'bun:test'

import { colorVar, cssCubicBezier, duration, ease, semanticColors, transition } from './index.ts'

const css = await Bun.file(new URL('./tokens.css', import.meta.url)).text()
const theme = await Bun.file(new URL('./theme.css', import.meta.url)).text()

const blocks = {
  light: css.slice(0, css.indexOf('@media (prefers-color-scheme: dark)')),
  system: css.slice(
    css.indexOf('@media (prefers-color-scheme: dark)'),
    css.indexOf(":root[data-theme='dark']"),
  ),
  dark: css.slice(css.indexOf(":root[data-theme='dark']"), css.indexOf('@media (prefers-reduced')),
}

const kebab = (name: string) =>
  name.replace(/(\d+)$/, '-$1').replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)

const decls = (block: string) => block.match(/--hm-color-[\w-]+: [^;]+;/g)

const reverse = ([x1, y1, x2, y2]: readonly [number, number, number, number]) =>
  [1 - x2, 1 - y2, 1 - x1, 1 - y1].map((n) => Number(n.toFixed(2)))

describe('tokens.css', () => {
  for (const [name, block] of Object.entries(blocks)) {
    test(`${name} theme declares every semantic color`, () => {
      for (const token of semanticColors) expect(block).toContain(`--hm-color-${token}:`)
    })
  }

  test('system dark and forced dark are identical', () => {
    expect(decls(blocks.system)).toEqual(decls(blocks.dark))
  })

  test('durations match the TS constants', () => {
    for (const [name, ms] of Object.entries(duration)) {
      expect(css).toContain(`--hm-duration-${kebab(name)}: ${ms}ms;`)
    }
  })

  test('easings match the TS constants', () => {
    for (const [name, curve] of Object.entries(ease)) {
      expect(css).toContain(`--hm-ease-${kebab(name)}: ${cssCubicBezier(curve)};`)
    }
  })

  test('reduced motion zeroes every duration', () => {
    const reduced = css.slice(css.indexOf('@media (prefers-reduced-motion: reduce)'))
    for (const name of Object.keys(duration)) {
      expect(reduced).toContain(`--hm-duration-${kebab(name)}: 0ms;`)
    }
  })

  test('no raw hex colors outside the primitive scale', () => {
    const semantic = css.match(/--hm-color-[\w-]+: [^;]+;/g) ?? []
    for (const decl of semantic) expect(decl).not.toMatch(/#[0-9a-f]{3,8}/i)
  })
})

describe('theme.css', () => {
  test('maps every semantic color to a utility', () => {
    for (const token of semanticColors) {
      expect(theme).toContain(`--color-${token}: var(--hm-color-${token});`)
    }
  })

  test('every radius resolves to the zero token', () => {
    const radii = theme.match(/--radius[\w-]*: [^;]+;/g) ?? []
    expect(radii.length).toBeGreaterThan(0)
    for (const decl of radii) expect(decl).toMatch(/: (var\(--hm-radius\)|initial);$/)
  })
})

describe('colorVar', () => {
  test('points at the semantic custom property', () => {
    expect(colorVar('fg-muted')).toBe('var(--hm-color-fg-muted)')
  })
})

describe('transition', () => {
  test('presets carry the design.md curves and durations in seconds', () => {
    expect(transition.productive.fast01).toEqual({ duration: 0.07, ease: [0.2, 0, 0.38, 0.9] })
    expect(transition.productive.fast02.duration).toBe(0.11)
    expect(transition.productive.moderate01.duration).toBe(0.15)
    expect(transition.expressive.moderate02).toEqual({ duration: 0.24, ease: [0.4, 0.14, 0.3, 1] })
    expect(transition.expressive.slow01.duration).toBe(0.4)
  })

  test('exit curves are the time-reversed entrance curves', () => {
    expect(reverse(ease.productive)).toEqual([...ease.productiveExit])
    expect(reverse(ease.expressive)).toEqual([...ease.expressiveExit])
  })
})
