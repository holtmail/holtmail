import {
  ApiError,
  deleteDraft,
  type Draft,
  type DraftAttachment,
  type DraftPayload,
  draftQuery,
  type Identity,
  identitiesQuery,
  idempotencyKey,
  type Leak,
  type Mailbox,
  type Outbox,
  type Route,
  type RouteError,
  refusedLeaks,
  reverseAliasFix,
  saveDraft,
  sendDraft,
  sessionQuery,
  transportsQuery,
  uploadDraftAttachment,
} from '@holtmail/api-client'
import { transition } from '@holtmail/tokens'
import {
  Button,
  cn,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
  Skeleton,
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import type { Editor } from '@tiptap/react'
import {
  ArchiveIcon,
  ChevronDownIcon,
  ClockIcon,
  Maximize2Icon,
  Minimize2Icon,
  PaperclipIcon,
  SendIcon,
  SignatureIcon,
  Trash2Icon,
  XIcon,
} from 'lucide-react'
import { m } from 'motion/react'
import {
  type ChangeEvent,
  type KeyboardEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from 'react'

import type { Layout } from '../hooks/use-layout.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { formatFullDate, formatSize } from '../lib/format.ts'
import { allRecipients } from '../lib/recipients.ts'
import {
  aliasIdentity,
  leakFixes,
  leakText,
  reasonText,
  routeErrorText,
  routeSummary,
  sendableDespite,
  withoutRecipient,
} from '../lib/route.ts'
import {
  discardsOnClose,
  fromLocalInput,
  htmlToText,
  insertTextSignature,
  renewsSendKey,
  signatureHtml,
  signatureText,
  splitQuote,
  textToHtml,
  toLocalInput,
} from '../lib/writing.ts'
import { AutoHeightFrame } from '../mail/message-body.tsx'
import { ConfirmDialog } from '../manage/confirm-dialog.tsx'
import { strings } from '../strings.ts'
import { ArchivePicker } from './archive-picker.tsx'
import { RecipientField } from './recipient-field.tsx'
import { RichEditor } from './rich-editor.tsx'

const t = strings.writing.compose
const AUTOSAVE_MS = 1_200
const quoteMarker = /\n\n(On .* wrote:|---------- Forwarded message ----------)\n/

const quoteDocument = (html: string) =>
  `<!doctype html><meta charset="utf-8"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'"><body style="margin:0;font:14px/1.5 sans-serif">${html}</body>`

/** The compose window over the reader (full screen on narrow screens). */
export function ComposePane({
  draftId,
  fresh,
  archiveId,
  layout,
  onClose,
  onSent,
}: {
  draftId: string
  fresh: boolean
  archiveId: string
  layout: Layout
  onClose: () => void
  onSent: (entry: Outbox) => void
}) {
  const draft = useQuery({
    ...draftQuery(draftId),
    staleTime: Number.POSITIVE_INFINITY,
    refetchOnWindowFocus: false,
  })
  const [expanded, setExpanded] = useState(false)
  const full = layout === 'narrow' || expanded

  return (
    <m.section
      data-compose
      aria-label={t.landmark}
      initial={{ x: '100%' }}
      animate={{ x: 0, transition: transition.expressive.moderate02 }}
      className={cn(
        'z-40 flex flex-col bg-bg',
        layout === 'narrow'
          ? 'fixed inset-0'
          : cn('absolute inset-y-0 right-0 w-full border-l border-border', !full && 'max-w-3xl'),
      )}
    >
      {draft.isPending ? (
        <div aria-busy className="grid gap-4 p-6">
          <span className="sr-only">{t.loading}</span>
          <Skeleton className="h-8 w-1/2" />
          <Skeleton className="h-8 w-2/3" />
          <Skeleton className="h-48 w-full" />
        </div>
      ) : draft.isError ? (
        <div className="grid gap-4 p-6">
          <p role="alert" className="text-sm text-danger">
            {t.loadFailed}: {authErrorMessage(draft.error)}
          </p>
          <Button variant="secondary" className="justify-self-start" onClick={onClose}>
            {t.close}
          </Button>
        </div>
      ) : (
        <ComposeForm
          initial={draft.data}
          fresh={fresh}
          archiveId={archiveId}
          expanded={expanded}
          canExpand={layout !== 'narrow'}
          onExpand={setExpanded}
          onClose={onClose}
          onSent={onSent}
        />
      )}
    </m.section>
  )
}

type Mode = 'rich' | 'plain'

function initialMode(payload: DraftPayload): Mode {
  if (payload.html != null) return 'rich'
  return (payload.context?.kind ?? 'new') === 'new' && !payload.text ? 'rich' : 'plain'
}

function ComposeForm({
  initial,
  fresh,
  archiveId,
  expanded,
  canExpand,
  onExpand,
  onClose,
  onSent,
}: {
  initial: Draft
  fresh: boolean
  archiveId: string
  expanded: boolean
  canExpand: boolean
  onExpand: (expanded: boolean) => void
  onClose: () => void
  onSent: (entry: Outbox) => void
}) {
  const start = initial.payload
  const split = useMemo(() => splitQuote(start.html), [start.html])
  const [identity, setIdentity] = useState<string | null>(start.identity ?? null)
  const [to, setTo] = useState<Mailbox[]>(start.to ?? [])
  const [cc, setCc] = useState<Mailbox[]>(start.cc ?? [])
  const [bcc, setBcc] = useState<Mailbox[]>(start.bcc ?? [])
  const [showCc, setShowCc] = useState((start.cc ?? []).length > 0)
  const [showBcc, setShowBcc] = useState((start.bcc ?? []).length > 0)
  const [subject, setSubject] = useState(start.subject ?? '')
  const [mode, setMode] = useState<Mode>(() => initialMode(start))
  const [ownHtml, setOwnHtml] = useState(split.own)
  const [editorSeed, setEditorSeed] = useState(split.own)
  const [editorKey, setEditorKey] = useState(0)
  const quote = split.quote
  const [text, setText] = useState(start.text ?? '')
  const [attachments, setAttachments] = useState<DraftAttachment[]>(start.attachments ?? [])
  const [sendAt, setSendAt] = useState<number | null>(start.send_at ?? null)
  const [scheduling, setScheduling] = useState(start.send_at != null)
  const [route, setRoute] = useState<Route | null>(initial.route ?? null)
  const [routeError, setRouteError] = useState<RouteError | null>(initial.route_error ?? null)
  const [leaks, setLeaks] = useState<Leak[]>(initial.leaks ?? [])
  const [confirmLeaks, setConfirmLeaks] = useState(false)
  const [context, setContext] = useState(start.context)
  const [saveState, setSaveState] = useState<'idle' | 'saving' | 'saved' | 'error'>('idle')
  const [saveProblem, setSaveProblem] = useState<string>()
  const [uploads, setUploads] = useState<{ id: number; name: string }[]>([])
  const [picking, setPicking] = useState(false)
  const [problem, setProblem] = useState<string>()
  const [sending, setSending] = useState(false)
  const [discarding, setDiscarding] = useState(false)
  const editor = useRef<Editor | null>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const version = useRef(initial.version)
  const chain = useRef<Promise<unknown>>(Promise.resolve())
  const sendKey = useRef<string>(undefined)
  const touched = useRef(false)
  const dirty = useRef(false)
  const closed = useRef(false)
  const uploadId = useRef(0)
  const queryClient = useQueryClient()
  const onEditorReady = useCallback((e: Editor | null) => {
    editor.current = e
  }, [])

  const me = useQuery(sessionQuery).data
  const identities = useQuery(identitiesQuery(archiveId))
  const transports = useQuery(transportsQuery(archiveId))
  const mine = (identities.data ?? []).filter(
    (i) => i.active && (me === undefined || i.users.includes(me.id)),
  )
  const transportName = (id: string) => transports.data?.find((tr) => tr.id === id)?.name

  const payload = useMemo<DraftPayload>(
    () => ({
      identity,
      to,
      cc,
      bcc,
      subject,
      text: mode === 'plain' ? text : '',
      html: mode === 'rich' ? `${ownHtml}${quote}` : null,
      attachments,
      in_reply_to: start.in_reply_to ?? null,
      references: start.references ?? [],
      context,
      send_at: sendAt,
    }),
    [
      identity,
      to,
      cc,
      bcc,
      subject,
      mode,
      text,
      ownHtml,
      quote,
      attachments,
      sendAt,
      start,
      context,
    ],
  )
  const payloadRef = useRef(payload)
  payloadRef.current = payload

  const enqueue = useCallback(<T,>(work: () => Promise<T>): Promise<T> => {
    const next = chain.current.then(work, work)
    chain.current = next.catch(() => undefined)
    return next
  }, [])

  const adopt = useCallback(
    (draft: Draft, sent: DraftPayload) => {
      queryClient.setQueryData(draftQuery(draft.id).queryKey, draft)
      version.current = draft.version
      setRoute(draft.route ?? null)
      setRouteError(draft.route_error ?? null)
      setLeaks(draft.leaks ?? [])
      setContext(draft.payload.context)
      if (JSON.stringify(payloadRef.current.attachments) === JSON.stringify(sent.attachments)) {
        setAttachments(draft.payload.attachments ?? [])
      }
    },
    [queryClient],
  )

  const save = useCallback(
    () =>
      enqueue(async () => {
        const sent = payloadRef.current
        setSaveState('saving')
        try {
          let result = await saveDraft(initial.id, version.current, sent)
          if (result.conflict) {
            version.current = result.draft.version
            result = await saveDraft(initial.id, version.current, sent)
          }
          adopt(result.draft, sent)
          if (payloadRef.current === sent) dirty.current = false
          setSaveState('saved')
          setSaveProblem(undefined)
          return result.draft
        } catch (error) {
          setSaveState('error')
          setSaveProblem(authErrorMessage(error))
          throw error
        }
      }),
    [enqueue, adopt, initial.id],
  )

  useEffect(() => {
    if (!touched.current) return undefined
    dirty.current = true
    const timer = window.setTimeout(() => void save().catch(() => undefined), AUTOSAVE_MS)
    return () => window.clearTimeout(timer)
  }, [payload, save])

  useEffect(() => {
    const warn = (event: BeforeUnloadEvent) => {
      if (dirty.current) event.preventDefault()
    }
    window.addEventListener('beforeunload', warn)
    return () => {
      window.removeEventListener('beforeunload', warn)
      if (closed.current) return
      if (discardsOnClose(payloadRef.current, { fresh, touched: touched.current })) {
        void deleteDraft(initial.id).catch(() => undefined)
      } else if (dirty.current) {
        void save().catch(() => undefined)
      }
    }
  }, [save, fresh, initial.id])

  const touch =
    <A,>(set: (value: A) => void) =>
    (value: A) => {
      touched.current = true
      set(value)
    }

  const currentIdentity: Identity | undefined = mine.find(
    (i) => i.id === (route?.identity ?? identity),
  )

  const insertSignature = () => {
    if (!currentIdentity) return
    touched.current = true
    if (mode === 'rich') {
      const html = signatureHtml(currentIdentity)
      if (html) editor.current?.chain().focus('end').insertContent(html).run()
    } else {
      setText((value) => insertTextSignature(value, signatureText(currentIdentity)))
    }
  }

  const switchMode = (next: Mode) => {
    if (next === mode) return
    touched.current = true
    if (next === 'plain') {
      const own = htmlToText(ownHtml)
      const textQuote = (() => {
        const original = start.text ?? ''
        const at = original.search(quoteMarker)
        return at < 0 ? '' : original.slice(at)
      })()
      setText(`${own}${textQuote}`)
    } else {
      const at = text.search(quoteMarker)
      const own = at < 0 || !quote ? text : text.slice(0, at)
      const html = textToHtml(own)
      setOwnHtml(html)
      setEditorSeed(html)
      setEditorKey((k) => k + 1)
    }
    setMode(next)
  }

  const upload = (event: ChangeEvent<HTMLInputElement>) => {
    const files = [...(event.target.files ?? [])]
    event.target.value = ''
    for (const file of files) {
      uploadId.current += 1
      const id = uploadId.current
      setUploads((list) => [...list, { id, name: file.name }])
      void save()
        .catch(() => undefined)
        .then(() =>
          enqueue(async () => {
            const draft = await uploadDraftAttachment(initial.id, file)
            queryClient.setQueryData(draftQuery(draft.id).queryKey, draft)
            version.current = draft.version
            setAttachments(draft.payload.attachments ?? [])
            setRoute(draft.route ?? null)
            setRouteError(draft.route_error ?? null)
            setLeaks(draft.leaks ?? [])
          }),
        )
        .catch((error: unknown) => setProblem(t.uploadFailed(file.name, authErrorMessage(error))))
        .finally(() => setUploads((list) => list.filter((u) => u.id !== id)))
    }
  }

  const attachArchived = (attachment: DraftAttachment) => {
    touched.current = true
    setAttachments((list) =>
      list.some((a) => a.message === attachment.message && a.ordinal === attachment.ordinal)
        ? list
        : [...list, attachment],
    )
  }

  const close = async () => {
    closed.current = true
    if (discardsOnClose(payloadRef.current, { fresh, touched: touched.current })) {
      await deleteDraft(initial.id).catch(() => undefined)
    } else if (dirty.current || touched.current) {
      await save().catch(() => undefined)
    }
    onClose()
  }

  const send = async (allowLeaks = false) => {
    if (sending) return
    setProblem(undefined)
    if (allRecipients(to, cc, bcc).length === 0) {
      setProblem(t.noRecipients)
      return
    }
    if (sendAt != null && sendAt <= Date.now()) {
      setProblem(t.schedulePast)
      return
    }
    setSending(true)
    sendKey.current ??= idempotencyKey()
    const key = sendKey.current
    try {
      const saved = await save()
      if (!allowLeaks && (saved.leaks ?? []).length > 0) {
        setSending(false)
        setConfirmLeaks(true)
        return
      }
      const entry = await enqueue(() =>
        sendDraft(
          initial.id,
          {
            version: version.current,
            send_at: sendAt,
            allow_leaks: allowLeaks,
          },
          key,
        ),
      )
      closed.current = true
      dirty.current = false
      onSent(entry)
    } catch (error) {
      if (renewsSendKey(error)) sendKey.current = undefined
      setSending(false)
      const refused = refusedLeaks(error)
      if (refused && refused.length > 0) {
        setLeaks(refused)
        setConfirmLeaks(true)
        return
      }
      setProblem(authErrorMessage(error))
      if (error instanceof ApiError && error.status === 409) {
        const latest = await queryClient
          .fetchQuery({ ...draftQuery(initial.id), staleTime: 0 })
          .catch(() => undefined)
        if (latest) adopt(latest, payloadRef.current)
      }
    }
  }

  const removeRecipient = (address: string) => {
    touched.current = true
    setTo((list) => withoutRecipient(list, address))
    setCc((list) => withoutRecipient(list, address))
    setBcc((list) => withoutRecipient(list, address))
  }

  const reverseRecipient = async (address: string) => {
    setProblem(undefined)
    try {
      await save()
      await enqueue(async () => {
        const draft = await reverseAliasFix(initial.id, version.current, address)
        setTo(draft.payload.to ?? [])
        setCc(draft.payload.cc ?? [])
        setBcc(draft.payload.bcc ?? [])
        adopt(draft, draft.payload)
      })
    } catch (error) {
      setProblem(strings.writing.route.leaks.fixFailed(authErrorMessage(error)))
    }
  }

  const onKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.target instanceof Element && event.target.closest('[role="dialog"]')) return
    if (event.key === 'Escape' && !event.defaultPrevented) {
      event.preventDefault()
      void close()
    } else if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault()
      void send()
    }
  }

  const title = t.titles[start.context?.kind ?? 'new'] ?? t.titles.new

  return (
    <form
      className="flex min-h-0 flex-1 flex-col"
      onKeyDown={onKeyDown}
      onSubmit={(event) => {
        event.preventDefault()
        void send()
      }}
    >
      <header className="flex h-12 shrink-0 items-center gap-1 border-b border-border px-2">
        <h2 className="min-w-0 flex-1 truncate px-2 text-sm font-semibold">
          {subject.trim() || title}
        </h2>
        <span aria-live="polite" className="px-2 text-xs text-fg-subtle">
          {saveState === 'saving'
            ? t.saving
            : saveState === 'saved'
              ? t.saved
              : saveState === 'error'
                ? t.saveFailed(saveProblem ?? '')
                : ''}
        </span>
        {canExpand && (
          <IconButton label={expanded ? t.collapse : t.expand} onClick={() => onExpand(!expanded)}>
            {expanded ? <Minimize2Icon /> : <Maximize2Icon />}
          </IconButton>
        )}
        <IconButton label={t.close} onClick={() => void close()}>
          <XIcon />
        </IconButton>
      </header>

      <div className="min-h-0 flex-1 overflow-y-auto px-4">
        <FromPicker
          identities={mine}
          chosen={identity}
          route={route}
          routeError={routeError}
          transportName={transportName}
          onChoose={touch(setIdentity)}
        />
        {leaks.length > 0 && (
          <LeakNotice
            leaks={leaks}
            route={route}
            identities={mine}
            onRemove={removeRecipient}
            onReverse={(address) => void reverseRecipient(address)}
            onIdentity={(id) => touch(setIdentity)(id)}
            onSendAnyway={() => setConfirmLeaks(true)}
          />
        )}
        <div className="relative">
          <RecipientField
            archiveId={archiveId}
            label={t.to}
            value={to}
            onChange={touch(setTo)}
            autoFocus={to.length === 0}
          />
          <div className="absolute top-1 right-0 flex gap-1">
            {!showCc && (
              <Button variant="ghost" size="sm" onClick={() => setShowCc(true)}>
                {t.cc}
              </Button>
            )}
            {!showBcc && (
              <Button variant="ghost" size="sm" onClick={() => setShowBcc(true)}>
                {t.bcc}
              </Button>
            )}
          </div>
        </div>
        {showCc && (
          <RecipientField archiveId={archiveId} label={t.cc} value={cc} onChange={touch(setCc)} />
        )}
        {showBcc && (
          <RecipientField
            archiveId={archiveId}
            label={t.bcc}
            value={bcc}
            onChange={touch(setBcc)}
          />
        )}
        <div className="grid grid-cols-[4rem_1fr] items-center border-b border-border">
          <label htmlFor="compose-subject" className="text-xs text-fg-muted">
            {t.subject}
          </label>
          <input
            id="compose-subject"
            className="h-10 bg-transparent text-sm text-fg outline-none"
            value={subject}
            autoFocus={to.length > 0 && !subject}
            onChange={(event) => {
              touched.current = true
              setSubject(event.target.value)
            }}
          />
        </div>

        <div className="flex flex-wrap items-center gap-2 py-2">
          <div role="radiogroup" aria-label={t.body} className="flex border border-border">
            {(['rich', 'plain'] as const).map((value) => (
              <button
                key={value}
                type="button"
                role="radio"
                aria-checked={mode === value}
                title={value === 'plain' ? t.plainHint : undefined}
                className={cn(
                  'h-7 px-3 text-xs text-fg-muted focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus',
                  mode === value ? 'bg-bg-selected text-fg' : 'hover:bg-bg-hover',
                )}
                onClick={() => switchMode(value)}
              >
                {value === 'rich' ? t.rich : t.plain}
              </button>
            ))}
          </div>
          {currentIdentity &&
            (currentIdentity.signature_html || currentIdentity.signature_text) && (
              <Button variant="ghost" size="sm" onClick={insertSignature}>
                <SignatureIcon />
                {t.signature}
              </Button>
            )}
        </div>

        {mode === 'rich' ? (
          <RichEditor
            key={editorKey}
            content={editorSeed}
            label={t.body}
            onChange={(html) => {
              touched.current = true
              setOwnHtml(html)
            }}
            onReady={onEditorReady}
            onSubmit={() => void send()}
          />
        ) : (
          <textarea
            aria-label={t.body}
            className="min-h-64 w-full resize-y bg-transparent py-2 font-mono text-sm text-fg outline-none"
            value={text}
            onChange={(event) => {
              touched.current = true
              setText(event.target.value)
            }}
          />
        )}

        {mode === 'rich' && quote && (
          <div className="mb-4 border-l border-border-strong pl-3">
            <AutoHeightFrame srcDoc={quoteDocument(quote)} title={t.body} />
          </div>
        )}

        {(attachments.length > 0 || uploads.length > 0) && (
          <ul aria-label={t.attachments} className="flex flex-wrap gap-2 py-3">
            {attachments.map((a, index) => (
              <li
                key={`${a.blob ?? `${a.message}:${a.ordinal}`}:${index}`}
                className="flex h-8 max-w-72 items-center gap-2 border border-border pl-2 text-xs"
              >
                <PaperclipIcon aria-hidden className="size-3.5 shrink-0 text-fg-subtle" />
                <span className="truncate">{a.name ?? a.content_type}</span>
                {a.size != null && (
                  <span className="shrink-0 font-mono text-fg-subtle">{formatSize(a.size)}</span>
                )}
                <button
                  type="button"
                  aria-label={t.removeAttachment(a.name ?? '')}
                  className="flex size-8 items-center justify-center text-fg-subtle hover:text-fg focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus [&_svg]:size-3.5"
                  onClick={() => {
                    touched.current = true
                    setAttachments((list) => list.filter((_, i) => i !== index))
                  }}
                >
                  <XIcon />
                </button>
              </li>
            ))}
            {uploads.map((u) => (
              <li
                key={`upload-${u.id}`}
                className="flex h-8 items-center border border-dashed border-border px-2 text-xs text-fg-subtle"
              >
                {t.uploading(u.name)}
              </li>
            ))}
          </ul>
        )}
      </div>

      <footer className="grid shrink-0 gap-2 border-t border-border p-2">
        {problem && (
          <p role="alert" className="px-2 text-sm text-danger">
            {problem}
          </p>
        )}
        {scheduling && (
          <div className="flex flex-wrap items-center gap-2 px-2">
            <label htmlFor="compose-send-at" className="text-xs text-fg-muted">
              {t.scheduleAt}
            </label>
            <input
              id="compose-send-at"
              type="datetime-local"
              className="h-8 border border-border bg-bg px-2 text-sm text-fg"
              value={sendAt == null ? '' : toLocalInput(sendAt)}
              min={toLocalInput(Date.now())}
              onChange={(event) => {
                touched.current = true
                setSendAt(fromLocalInput(event.target.value) ?? null)
              }}
            />
            <Button
              variant="ghost"
              size="sm"
              onClick={() => {
                touched.current = true
                setSendAt(null)
                setScheduling(false)
              }}
            >
              {t.scheduleClear}
            </Button>
          </div>
        )}
        <div className="flex items-center gap-1">
          <Button
            type="submit"
            variant="primary"
            disabled={sending || (!route && !sendableDespite(routeError))}
            title={t.shortcutSend}
          >
            <SendIcon />
            {sending ? t.sending : sendAt != null ? t.sendAt(formatFullDate(sendAt)) : t.send}
          </Button>
          {!scheduling && (
            <IconButton label={t.schedule} onClick={() => setScheduling(true)}>
              <ClockIcon />
            </IconButton>
          )}
          <IconButton label={t.attach} onClick={() => fileInput.current?.click()}>
            <PaperclipIcon />
          </IconButton>
          <input ref={fileInput} type="file" multiple hidden onChange={upload} />
          <IconButton label={t.attachArchived} onClick={() => setPicking(true)}>
            <ArchiveIcon />
          </IconButton>
          <span className="flex-1" />
          <IconButton label={t.discard} onClick={() => setDiscarding(true)}>
            <Trash2Icon />
          </IconButton>
        </div>
      </footer>

      <ArchivePicker
        archiveId={archiveId}
        open={picking}
        onOpenChange={setPicking}
        onPick={attachArchived}
      />

      <ConfirmDialog
        open={confirmLeaks}
        onOpenChange={setConfirmLeaks}
        title={strings.writing.route.leaks.confirmTitle}
        description={strings.writing.route.leaks.confirmLead(leaks.length, route?.from ?? '')}
        action={strings.writing.route.leaks.confirmAction}
        busyAction={t.sending}
        onConfirm={() => send(true)}
      />

      <ConfirmDialog
        open={discarding}
        onOpenChange={setDiscarding}
        title={t.discardTitle}
        description={t.discardLead}
        action={t.discard}
        busyAction={t.saving}
        onConfirm={async () => {
          closed.current = true
          await enqueue(() => deleteDraft(initial.id))
          onClose()
        }}
      />
    </form>
  )
}

function IconButton({
  label,
  onClick,
  children,
}: {
  label: string
  onClick: () => void
  children: ReactNode
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={label} onClick={onClick}>
          {children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  )
}

function LeakNotice({
  leaks,
  route,
  identities,
  onRemove,
  onReverse,
  onIdentity,
  onSendAnyway,
}: {
  leaks: Leak[]
  route: Route | null
  identities: Identity[]
  onRemove: (address: string) => void
  onReverse: (address: string) => void
  onIdentity: (identity: string) => void
  onSendAnyway: () => void
}) {
  const l = strings.writing.route.leaks
  const alias = route?.alias
  const missingIdentity =
    alias != null &&
    leaks.some((leak) => leak.fix === 'use_alias_identity') &&
    !aliasIdentity(route, identities)
  return (
    <div role="alert" className="grid gap-2 border-b border-border py-2 text-xs">
      <p className="font-semibold text-danger">{l.title}</p>
      <ul className="grid gap-1">
        {leaks.map((leak) => (
          <li key={`${leak.field}-${leak.recipient}`} className="flex flex-wrap items-center gap-2">
            <span className="min-w-0 flex-1">{leakText(leak)}</span>
            {leakFixes(leak, route, identities).map((fix) =>
              fix.kind === 'reverse' ? (
                <Button
                  key="reverse"
                  size="sm"
                  variant="secondary"
                  onClick={() => onReverse(leak.recipient)}
                >
                  {l.useReverseAlias}
                </Button>
              ) : fix.kind === 'identity' ? (
                <Button
                  key="identity"
                  size="sm"
                  variant="secondary"
                  onClick={() => onIdentity(fix.identity.id)}
                >
                  {l.useAliasIdentity(fix.identity.address)}
                </Button>
              ) : (
                <Button
                  key="remove"
                  size="sm"
                  variant="ghost"
                  onClick={() => onRemove(leak.recipient)}
                >
                  {l.removeRecipient}
                </Button>
              ),
            )}
          </li>
        ))}
      </ul>
      {missingIdentity && <p className="text-fg-muted">{l.noAliasIdentity(alias)}</p>}
      <div>
        <Button size="sm" variant="ghost" className="text-danger" onClick={onSendAnyway}>
          {l.sendAnyway}
        </Button>
      </div>
    </div>
  )
}

function FromPicker({
  identities,
  chosen,
  route,
  routeError,
  transportName,
  onChoose,
}: {
  identities: Identity[]
  chosen: string | null
  route: Route | null
  routeError: RouteError | null
  transportName: (id: string) => string | undefined
  onChoose: (identity: string | null) => void
}) {
  const r = strings.writing.route
  return (
    <div className="grid grid-cols-[4rem_1fr] items-start border-b border-border py-1">
      <span className="pt-2 text-xs text-fg-muted">{t.from}</span>
      <div className="grid min-w-0 gap-1">
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              className="h-8 max-w-full justify-start px-0 hover:bg-transparent"
            >
              <span className="truncate text-sm">
                {route ? routeSummary(route, transportName(route.transport)) : r.chooseIdentity}
              </span>
              <ChevronDownIcon />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="w-80">
            <DropdownMenuLabel>{r.chooseIdentity}</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              value={chosen ?? ''}
              onValueChange={(value) => onChoose(value || null)}
            >
              <DropdownMenuRadioItem value="">{r.automatic}</DropdownMenuRadioItem>
              {identities.map((identity) => (
                <DropdownMenuRadioItem key={identity.id} value={identity.id}>
                  <span className="grid min-w-0">
                    <span className="truncate">{identity.name || identity.address}</span>
                    {identity.name && (
                      <span className="truncate font-mono text-xs text-fg-subtle">
                        {identity.address}
                      </span>
                    )}
                  </span>
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
        {route && route.reasons.length > 0 && (
          <ul aria-label={r.because} className="grid gap-0.5 pb-1 text-xs text-fg-subtle">
            {route.reasons.map((reason, index) => (
              <li key={`${reason.code}-${index}`}>{reasonText(reason)}</li>
            ))}
          </ul>
        )}
        {!route && routeError && (
          <p role="alert" className="pb-1 text-xs text-danger">
            {routeErrorText(routeError, transportName)}
          </p>
        )}
      </div>
    </div>
  )
}
