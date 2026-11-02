import { type ClassValue, clsx } from 'clsx'
import { extendTailwindMerge } from 'tailwind-merge'

const twMerge = extendTailwindMerge({
  extend: {
    theme: {
      ease: ['productive', 'productive-exit', 'expressive', 'expressive-exit'],
      container: ['pane-folders', 'pane-list'],
      spacing: ['topbar'],
    },
    classGroups: {
      duration: [{ duration: ['fast-01', 'fast-02', 'moderate-01', 'moderate-02', 'slow-01'] }],
    },
  },
})

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs))
}
