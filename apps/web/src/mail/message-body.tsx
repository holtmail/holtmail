import { bodyQuery, type MessageDetail } from '@holtmail/api-client'
import { Button, Skeleton } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArchiveIcon, GlobeIcon, ImageOffIcon } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

import { useRemoteImages } from '../hooks/use-remote-images.ts'
import { bodyMode, liveMode } from '../lib/remote-images.ts'
import { linkify, withLinkTarget } from '../lib/text.ts'
import { strings } from '../strings.ts'

export function MessageBody({ message }: { message: MessageDetail }) {
  if (message.has_html) return <HtmlBody message={message} />
  if (!message.text?.trim()) {
    return <p className="text-sm text-fg-subtle italic">{strings.reader.noBody}</p>
  }
  return <TextBody text={message.text} />
}

function TextBody({ text }: { text: string }) {
  const parts = useMemo(() => linkify(text), [text])
  return (
    <div className="max-w-prose font-sans text-base wrap-break-word whitespace-pre-wrap text-fg">
      {parts.map((part, index) =>
        part.kind === 'link' ? (
          <a
            key={index}
            href={part.href}
            target="_blank"
            rel="noopener noreferrer"
            className="text-accent underline-offset-2 hover:underline"
          >
            {part.text}
          </a>
        ) : (
          <span key={index}>{part.text}</span>
        ),
      )}
    </div>
  )
}

function HtmlBody({ message }: { message: MessageDetail }) {
  const [preference] = useRemoteImages()
  const [shown, setShown] = useState(false)
  const [live, setLive] = useState(false)
  const mode = live ? liveMode(preference) : bodyMode(preference, shown)
  const body = useQuery({
    ...bodyQuery(message.id, mode, !live),
    placeholderData: (previous) => previous,
  })
  const srcDoc = useMemo(
    () => (body.data ? withLinkTarget(body.data.html) : undefined),
    [body.data],
  )

  if (body.isError) return <p className="text-sm text-danger">{strings.reader.bodyFailed}</p>

  return (
    <div className="grid gap-3">
      {body.data?.remoteBlocked && mode === 'block' && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border border-border bg-bg-subtle py-1 pr-1 pl-3 text-sm text-fg-muted">
          <ImageOffIcon aria-hidden className="size-4 shrink-0" />
          <span className="flex-1">{strings.reader.remoteHidden}</span>
          <span className="flex">
            <Button size="sm" variant="ghost" onClick={() => setShown(true)}>
              {strings.reader.showRemote}
            </Button>
            <Button size="sm" variant="ghost" asChild>
              <Link to="/settings/reading">{strings.reader.alwaysShowRemote}</Link>
            </Button>
          </span>
        </div>
      )}
      {body.data?.savedShown && !live && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 py-0.5 pl-3 text-xs text-fg-subtle">
          <ArchiveIcon aria-hidden className="size-3.5 shrink-0" />
          <span className="flex-1">{strings.reader.savedShown}</span>
          <Button size="sm" variant="ghost" onClick={() => setLive(true)}>
            {strings.reader.loadOriginals}
          </Button>
        </div>
      )}
      {live && (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 py-0.5 pl-3 text-xs text-fg-subtle">
          <GlobeIcon aria-hidden className="size-3.5 shrink-0" />
          <span className="flex-1">{strings.reader.originalsShown}</span>
          <Button size="sm" variant="ghost" onClick={() => setLive(false)}>
            {strings.reader.showSaved}
          </Button>
        </div>
      )}
      {srcDoc ? (
        <AutoHeightFrame srcDoc={srcDoc} title={strings.reader.bodyTitle(message.subject ?? '')} />
      ) : (
        <div className="grid gap-2">
          <Skeleton className="h-4 w-3/4" />
          <Skeleton className="h-4 w-2/3" />
          <Skeleton className="h-4 w-1/2" />
        </div>
      )}
    </div>
  )
}

function AutoHeightFrame({ srcDoc, title }: { srcDoc: string; title: string }) {
  const ref = useRef<HTMLIFrameElement>(null)
  const [height, setHeight] = useState<number>()

  useEffect(() => {
    const frame = ref.current
    if (!frame) return undefined
    let observer: ResizeObserver | undefined
    const measure = () => {
      const doc = frame.contentDocument
      if (!doc?.documentElement) return
      const next = Math.ceil(
        Math.max(doc.documentElement.offsetHeight, doc.body?.scrollHeight ?? 0),
      )
      setHeight((previous) => (previous === next ? previous : next))
    }
    const onLoad = () => {
      measure()
      observer?.disconnect()
      const doc = frame.contentDocument
      if (!doc?.documentElement) return
      observer = new ResizeObserver(measure)
      observer.observe(doc.documentElement)
      if (doc.body) observer.observe(doc.body)
    }
    frame.addEventListener('load', onLoad)
    if (frame.contentDocument?.readyState === 'complete') onLoad()
    return () => {
      frame.removeEventListener('load', onLoad)
      observer?.disconnect()
    }
  }, [srcDoc])

  return (
    <iframe
      ref={ref}
      title={title}
      srcDoc={srcDoc}
      sandbox="allow-same-origin allow-popups allow-popups-to-escape-sandbox"
      referrerPolicy="no-referrer"
      className="block min-h-24 w-full border-0"
      style={height === undefined ? undefined : { height }}
    />
  )
}
