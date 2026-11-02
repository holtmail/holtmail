import { useCallback, useState } from 'react'

export interface ControllableStateOptions<T> {
  value?: T | undefined
  defaultValue: T
  onChange?: ((value: T) => void) | undefined
}

export function useControllableState<T>({
  value,
  defaultValue,
  onChange,
}: ControllableStateOptions<T>): readonly [T, (next: T) => void] {
  const [internal, setInternal] = useState(defaultValue)
  const controlled = value !== undefined
  const set = useCallback(
    (next: T) => {
      if (!controlled) setInternal(next)
      onChange?.(next)
    },
    [controlled, onChange],
  )
  return [controlled ? value : internal, set] as const
}
