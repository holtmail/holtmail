import { transition } from '@holtmail/tokens'
import { cn, RadioGroup, RadioGroupItem, Switch } from '@holtmail/ui'
import { ArchiveIcon, TriangleAlertIcon } from 'lucide-react'
import { AnimatePresence, m } from 'motion/react'

import { useReadOnOpen } from '../hooks/use-read-on-open.ts'
import { useRemoteImages } from '../hooks/use-remote-images.ts'
import { parseRemoteImages, type RemoteImages } from '../lib/remote-images.ts'
import { strings } from '../strings.ts'
import { SettingsPage } from './settings-layout.tsx'

const t = strings.settings
const choices: RemoteImages[] = ['ask', 'proxy', 'direct']

export function ReadingPage() {
  const [remote, setRemote] = useRemoteImages()
  const [readOnOpen, setReadOnOpen] = useReadOnOpen()
  const mb = strings.writing.mailbox
  return (
    <SettingsPage title={t.reading} lead={t.readingLead}>
      <section aria-labelledby="remote-images" className="grid max-w-xl gap-4">
        <div className="grid gap-1">
          <h3 id="remote-images" className="text-lg font-normal">
            {t.remoteImages}
          </h3>
          <p className="text-sm text-fg-muted">{t.remoteImagesLead}</p>
        </div>
        <RadioGroup
          aria-labelledby="remote-images"
          value={remote}
          onValueChange={(value) => setRemote(parseRemoteImages(value))}
          className="gap-px border border-border bg-border"
        >
          {choices.map((choice) => {
            const option = t.remoteOptions[choice]
            const id = `remote-${choice}`
            return (
              <label
                key={choice}
                htmlFor={id}
                className={cn(
                  'flex cursor-default gap-3 bg-bg px-4 py-3',
                  'transition-colors duration-fast-02 ease-productive hover:bg-bg-hover',
                  remote === choice && 'bg-bg-selected hover:bg-bg-selected',
                )}
              >
                <RadioGroupItem id={id} value={choice} className="mt-0.5" />
                <span className="grid gap-0.5">
                  <span className="text-sm text-fg">{option.label}</span>
                  <span className="text-xs text-fg-muted">{option.hint}</span>
                </span>
              </label>
            )
          })}
        </RadioGroup>
        <AnimatePresence initial={false}>
          {remote === 'direct' && (
            <m.p
              key="warning"
              role="alert"
              className="flex items-start gap-3 overflow-hidden border-l-2 border-warning bg-bg-subtle px-4 py-3 text-sm text-fg"
              initial={{ opacity: 0, height: 0 }}
              animate={{ opacity: 1, height: 'auto', transition: transition.expressive.moderate02 }}
              exit={{ opacity: 0, height: 0, transition: transition.productiveExit.fast02 }}
            >
              <TriangleAlertIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-warning" />
              {t.directWarning}
            </m.p>
          )}
        </AnimatePresence>
        <p className="flex items-start gap-3 text-xs text-fg-subtle">
          <ArchiveIcon aria-hidden className="size-4 shrink-0" />
          <span>
            {t.savedCopiesNote} {t.thisBrowser}
          </span>
        </p>
      </section>
      <section aria-labelledby="read-on-open" className="mt-10 grid max-w-xl gap-1">
        <div className="flex items-start gap-4">
          <div className="grid min-w-0 flex-1 gap-1">
            <label id="read-on-open" htmlFor="read-on-open-switch" className="text-lg font-normal">
              {mb.readOnOpen}
            </label>
            <p id="read-on-open-hint" className="text-sm text-fg-muted">
              {mb.readOnOpenHint} {t.thisBrowser}
            </p>
          </div>
          <Switch
            id="read-on-open-switch"
            aria-describedby="read-on-open-hint"
            checked={readOnOpen}
            onCheckedChange={setReadOnOpen}
          />
        </div>
      </section>
    </SettingsPage>
  )
}
