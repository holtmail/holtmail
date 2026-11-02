export const semanticColors = [
  'bg',
  'bg-subtle',
  'bg-hover',
  'bg-selected',
  'fg',
  'fg-muted',
  'fg-subtle',
  'border',
  'border-strong',
  'accent',
  'accent-hover',
  'accent-fg',
  'focus',
  'danger',
  'danger-hover',
  'danger-fg',
  'warning',
  'success',
  'overlay',
] as const

export type SemanticColor = (typeof semanticColors)[number]

export const colorVar = (name: SemanticColor): string => `var(--hm-color-${name})`

export const themes = ['light', 'dark', 'system'] as const

export type Theme = (typeof themes)[number]

export const font = {
  sans: 'var(--hm-font-sans)',
  mono: 'var(--hm-font-mono)',
} as const

export const fontWeight = {
  regular: 400,
  medium: 500,
  semibold: 600,
} as const

export const fontSize = {
  xs: { size: '0.75rem', lineHeight: '1rem', tracking: '0.02rem' },
  sm: { size: '0.875rem', lineHeight: '1.25rem', tracking: '0.01rem' },
  base: { size: '1rem', lineHeight: '1.5rem', tracking: '0' },
  lg: { size: '1.25rem', lineHeight: '1.75rem', tracking: '0' },
  xl: { size: '1.5rem', lineHeight: '2rem', tracking: '0' },
  '2xl': { size: '2rem', lineHeight: '2.5rem', tracking: '0' },
} as const

export const space = {
  1: '0.25rem',
  2: '0.5rem',
  3: '0.75rem',
  4: '1rem',
  6: '1.5rem',
  8: '2rem',
  10: '2.5rem',
  12: '3rem',
  16: '4rem',
} as const

export const radius = 0

export type CubicBezier = readonly [number, number, number, number]

export const ease = {
  productive: [0.2, 0, 0.38, 0.9],
  productiveExit: [0.62, 0.1, 0.8, 1],
  expressive: [0.4, 0.14, 0.3, 1],
  expressiveExit: [0.7, 0, 0.6, 0.86],
} as const satisfies Record<string, CubicBezier>

export const duration = {
  fast01: 70,
  fast02: 110,
  moderate01: 150,
  moderate02: 240,
  slow01: 400,
} as const

export type Duration = keyof typeof duration

export const distance = {
  xs: 4,
  sm: 8,
} as const

export interface Transition {
  readonly duration: number
  readonly ease: CubicBezier
}

const preset = (ms: number, curve: CubicBezier): Transition => ({
  duration: ms / 1000,
  ease: curve,
})

export const transition = {
  productive: {
    fast01: preset(duration.fast01, ease.productive),
    fast02: preset(duration.fast02, ease.productive),
    moderate01: preset(duration.moderate01, ease.productive),
  },
  productiveExit: {
    fast01: preset(duration.fast01, ease.productiveExit),
    fast02: preset(duration.fast02, ease.productiveExit),
    moderate01: preset(duration.moderate01, ease.productiveExit),
  },
  expressive: {
    moderate02: preset(duration.moderate02, ease.expressive),
    slow01: preset(duration.slow01, ease.expressive),
  },
  expressiveExit: {
    moderate02: preset(duration.moderate02, ease.expressiveExit),
    slow01: preset(duration.slow01, ease.expressiveExit),
  },
} as const

export const cssCubicBezier = (curve: CubicBezier): string => `cubic-bezier(${curve.join(', ')})`
