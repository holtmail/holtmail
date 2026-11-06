import { CheckIcon, CopyIcon } from 'lucide-react'
import { type ReactNode, useEffect, useId, useRef } from 'react'

import { type CopyEnv, type CopyResult, manualCopyHint } from '../lib/copy.ts'
import { useCopy } from '../lib/use-copy.ts'
import { Button, type ButtonProps } from './button.tsx'
import { Input } from './input.tsx'
import { Tooltip, TooltipContent, TooltipTrigger } from './tooltip.tsx'

export type CopyButtonProps = Omit<ButtonProps, 'asChild' | 'children' | 'onClick'> & {
  /** The value to copy, or a function read at click time. */
  text: string | (() => string)
  /** The button's label; defaults to `Copy`. */
  label?: string
  /** Shown and announced after a successful copy; defaults to `Copied`. */
  copiedLabel?: string
  /** Shown next to the value when the user has to copy it by hand; defaults by platform. */
  manualHint?: string
  /** Replaces the copy icon in the idle state. */
  icon?: ReactNode
  /** Shows only the icon, with the label as its accessible name and tooltip. */
  iconOnly?: boolean
  /** Called with the outcome of each copy. */
  onCopied?: (result: CopyResult) => void
  /** Overrides the browser features used, for tests. */
  env?: CopyEnv
}

function platformHint(): string {
  if (typeof navigator === 'undefined') return manualCopyHint('')
  return manualCopyHint(navigator.platform || navigator.userAgent)
}

/**
 * A button that copies `text`. Success swaps the icon to a check and announces `copiedLabel`
 * through a polite live region for 1.5 s. When neither automatic path works, the value appears
 * selected in a read-only field with a keyboard hint until it loses focus or Escape is pressed.
 */
export function CopyButton({
  text,
  label = 'Copy',
  copiedLabel = 'Copied',
  manualHint,
  icon,
  iconOnly = false,
  onCopied,
  env,
  variant = 'ghost',
  size,
  className,
  ...props
}: CopyButtonProps) {
  const { copied, manual, copy, reset } = useCopy(env)
  const field = useRef<HTMLInputElement>(null)
  const hintId = useId()

  useEffect(() => {
    if (manual === null) return
    field.current?.focus()
    field.current?.select()
  }, [manual])

  const onClick = () => {
    void copy(typeof text === 'function' ? text() : text).then((result) => onCopied?.(result))
  }
  const shown = copied ? copiedLabel : label
  const glyph = copied ? <CheckIcon /> : (icon ?? <CopyIcon />)

  const button = iconOnly ? (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant={variant}
          size={size ?? 'icon'}
          aria-label={shown}
          className={className}
          onClick={onClick}
          {...props}
        >
          {glyph}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{shown}</TooltipContent>
    </Tooltip>
  ) : (
    <Button
      variant={variant}
      size={size ?? 'sm'}
      className={className}
      onClick={onClick}
      {...props}
    >
      {glyph}
      {shown}
    </Button>
  )

  return (
    <>
      {button}
      <span aria-live="polite" className="sr-only">
        {copied ? copiedLabel : ''}
      </span>
      {manual !== null && (
        <span data-slot="copy-manual" className="inline-flex min-w-0 items-center gap-2">
          <Input
            ref={field}
            readOnly
            value={manual}
            aria-label={label}
            aria-describedby={hintId}
            className="w-56 font-mono text-xs"
            onFocus={(event) => event.currentTarget.select()}
            onBlur={reset}
            onKeyDown={(event) => {
              if (event.key === 'Escape') reset()
            }}
          />
          <span id={hintId} className="text-xs whitespace-nowrap text-fg-subtle">
            {manualHint ?? platformHint()}
          </span>
        </span>
      )}
    </>
  )
}
