import { type Theme, themes } from '@holtmail/tokens'
import { useSyncExternalStore } from 'react'

const key = 'holtmail.theme'

export function parseTheme(value: string | null | undefined): Theme {
  return themes.find((theme) => theme === value) ?? 'system'
}

function readTheme(): Theme {
  try {
    return parseTheme(localStorage.getItem(key))
  } catch {
    return 'system'
  }
}

let current: Theme | undefined
const listeners = new Set<() => void>()

function getTheme(): Theme {
  current ??= readTheme()
  return current
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener)
  return () => listeners.delete(listener)
}

function setTheme(theme: Theme): void {
  const root = document.documentElement
  if (theme === 'system') delete root.dataset.theme
  else root.dataset.theme = theme
  try {
    if (theme === 'system') localStorage.removeItem(key)
    else localStorage.setItem(key, theme)
  } catch {}
  current = theme
  for (const listener of listeners) listener()
}

export function toggleTheme(): void {
  const theme = getTheme()
  const dark =
    theme === 'dark' ||
    (theme === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches)
  setTheme(dark ? 'light' : 'dark')
}

export function useTheme(): readonly [Theme, (theme: Theme) => void] {
  const theme = useSyncExternalStore(subscribe, getTheme, () => 'system' as const)
  return [theme, setTheme] as const
}
