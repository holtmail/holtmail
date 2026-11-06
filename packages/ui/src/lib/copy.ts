/** Which path put the text on the clipboard, or `manual` when none could and the user has to copy it. */
export type CopyResult = 'clipboard' | 'exec' | 'manual'

/** The browser features `copyText` may use; a missing one is skipped. */
export interface CopyEnv {
  clipboard?: { writeText(text: string): Promise<void> } | null
  document?: Document | null
}

/** The clipboard and document of the current page, or none of them outside a browser. */
export function browserCopyEnv(): CopyEnv {
  return {
    clipboard: typeof navigator === 'undefined' ? null : (navigator.clipboard ?? null),
    document: typeof document === 'undefined' ? null : document,
  }
}

/**
 * Copies `text` with the first path that works: the async Clipboard API, then `execCommand('copy')`
 * on a hidden textarea (which also works on plain HTTP), else `manual`. Never rejects, and leaves
 * the page's focus where it was.
 */
export async function copyText(text: string, env: CopyEnv = browserCopyEnv()): Promise<CopyResult> {
  if (env.clipboard) {
    try {
      await env.clipboard.writeText(text)
      return 'clipboard'
    } catch {}
  }
  if (env.document && execCopy(text, env.document)) return 'exec'
  return 'manual'
}

function execCopy(text: string, doc: Document): boolean {
  const body = doc.body
  if (!body) return false
  const active = doc.activeElement
  const area = doc.createElement('textarea')
  area.value = text
  area.readOnly = true
  area.setAttribute('aria-hidden', 'true')
  area.tabIndex = -1
  area.style.position = 'fixed'
  area.style.top = '0'
  area.style.left = '0'
  area.style.opacity = '0'
  area.style.pointerEvents = 'none'
  body.appendChild(area)
  try {
    area.select()
    area.setSelectionRange(0, text.length)
    return doc.execCommand('copy')
  } catch {
    return false
  } finally {
    body.removeChild(area)
    if (active && 'focus' in active && typeof active.focus === 'function') active.focus()
  }
}

/** The hint shown next to a value the user has to copy by hand: `⌘C` on Apple platforms, `Ctrl+C` elsewhere. */
export function manualCopyHint(platform: string): string {
  return /Mac|iPhone|iPad|iPod/i.test(platform) ? 'Press ⌘C' : 'Press Ctrl+C'
}
