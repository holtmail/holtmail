import { Button, CopyButton } from '@holtmail/ui'
import { DownloadIcon, UploadIcon } from 'lucide-react'
import { type ChangeEvent, useId, useRef, useState } from 'react'

import { TextArea } from '../items/inputs.tsx'
import { exportFileName, exportText, importText } from '../lib/rules-json.ts'
import type { FlowDefinition } from '../lib/rules.ts'
import { strings } from '../strings.ts'

const t = strings.rules.json

function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: 'application/json' }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  link.click()
  setTimeout(() => URL.revokeObjectURL(url), 0)
}

/** Export of the editor's flow as `holtmail-rules` JSON and import of such a file into the
 * editor (not saved until Save). */
export function JsonPanel({
  name,
  flow,
  onImport,
}: {
  name: string
  flow: FlowDefinition
  onImport: (flow: FlowDefinition) => void
}) {
  const pasteId = useId()
  const file = useRef<HTMLInputElement>(null)
  const [pasted, setPasted] = useState('')
  const [notice, setNotice] = useState<{ error: boolean; text: string } | null>(null)
  const text = exportText(flow)

  const load = (source: string) => {
    const read = importText(source)
    if (read.error) {
      setNotice({ error: true, text: t.errors[read.error] ?? read.error })
      return
    }
    onImport(read.flow)
    setPasted('')
    setNotice({ error: false, text: t.loaded })
  }

  const chosen = (event: ChangeEvent<HTMLInputElement>) => {
    const picked = event.target.files?.[0]
    event.target.value = ''
    if (!picked) return
    picked.text().then(load, () => setNotice({ error: true, text: t.errors.read ?? '' }))
  }

  return (
    <div className="mx-auto grid w-full max-w-3xl content-start gap-6 p-4">
      <section aria-label={t.exportTitle} className="grid gap-2">
        <h2 className="text-base text-fg">{t.exportTitle}</h2>
        <p className="text-xs text-fg-subtle">{t.exportLead}</p>
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" onClick={() => download(exportFileName(name), text)}>
            <DownloadIcon />
            {t.download}
          </Button>
          <CopyButton variant="secondary" text={text} label={t.copy} />
        </div>
        <pre className="max-h-96 overflow-auto border border-border bg-bg-subtle p-2 font-mono text-xs">
          {text}
        </pre>
      </section>
      <section aria-label={t.importTitle} className="grid gap-2">
        <h2 className="text-base text-fg">{t.importTitle}</h2>
        <p className="text-xs text-fg-subtle">{t.importLead}</p>
        <input
          ref={file}
          type="file"
          accept="application/json,.json"
          className="sr-only"
          tabIndex={-1}
          onChange={chosen}
        />
        <Button
          variant="secondary"
          className="justify-self-start"
          onClick={() => file.current?.click()}
        >
          <UploadIcon />
          {t.file}
        </Button>
        <label htmlFor={pasteId} className="text-xs text-fg-muted">
          {t.paste}
        </label>
        <TextArea
          id={pasteId}
          value={pasted}
          rows={6}
          spellCheck={false}
          className="font-mono text-xs"
          onChange={setPasted}
        />
        <Button
          variant="secondary"
          className="justify-self-start"
          disabled={pasted.trim() === ''}
          onClick={() => load(pasted)}
        >
          {t.load}
        </Button>
        <p
          aria-live="polite"
          className={notice?.error ? 'text-sm text-danger' : 'text-sm text-fg-muted'}
        >
          {notice?.text}
        </p>
      </section>
    </div>
  )
}
