import { accountsQuery, archivesQuery, createArchive } from '@holtmail/api-client'
import { distance, duration, transition } from '@holtmail/tokens'
import { Button, cn } from '@holtmail/ui'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { ArrowLeftIcon, ArrowRightIcon, FileUpIcon, MailPlusIcon } from 'lucide-react'
import { AnimatePresence, m, type Variants } from 'motion/react'
import { type ReactNode, useEffect, useState } from 'react'

import { Wordmark } from '../auth/auth-screen.tsx'
import { Field, FormError } from '../auth/field.tsx'
import { useLiveArchive } from '../hooks/use-live-archive.ts'
import { useOAuthReturn } from '../hooks/use-oauth-return.ts'
import { authErrorMessage } from '../lib/auth.ts'
import { UserMenu } from '../mail/user-menu.tsx'
import { AccountList } from '../manage/account-list.tsx'
import { ConnectMailbox } from '../manage/connect-mailbox.tsx'
import { ImportFiles } from '../manage/import-files.tsx'
import { OAuthReturnNote } from '../manage/oauth-sign-in.tsx'
import { strings } from '../strings.ts'

const t = strings.welcome

type Step = 'choose' | 'connect' | 'upload' | 'progress'

const column: Variants = {
  shown: { transition: { staggerChildren: duration.fast01 / 1000 } },
}

const item: Variants = {
  hidden: { opacity: 0, y: distance.sm },
  shown: { opacity: 1, y: 0, transition: transition.expressive.slow01 },
}

export function WelcomeScreen() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [step, setStep] = useState<Step>('choose')
  const [name, setName] = useState<string>(t.defaultName)
  const [archiveId, setArchiveId] = useState<string>()
  const [uploaded, setUploaded] = useState(false)
  const [signedIn, dismissSignedIn] = useOAuthReturn()

  useEffect(() => {
    let current = true
    const resume = async () => {
      if (!signedIn) return
      const archives = await queryClient.fetchQuery(archivesQuery)
      const lists = await Promise.all(
        archives.map((archive) => queryClient.fetchQuery(accountsQuery(archive.id))),
      )
      const owner =
        signedIn.kind === 'done'
          ? archives.find((_, index) =>
              lists[index]?.some((account) => account.id === signedIn.account),
            )
          : undefined
      const empty = archives.find((_, index) => lists[index]?.length === 0)
      const id = (owner ?? empty ?? archives[0])?.id
      if (!current || !id) return
      setArchiveId(id)
      setStep(signedIn.kind === 'done' ? 'progress' : 'connect')
    }
    resume().catch(() => undefined)
    return () => {
      current = false
    }
  }, [signedIn, queryClient])

  const create = useMutation({
    mutationFn: async (next: Step) => {
      if (archiveId) return { id: archiveId, next }
      const archive = await createArchive(name.trim() || t.defaultName)
      return { id: archive.id, next }
    },
    onSuccess: ({ id, next }) => {
      setArchiveId(id)
      setStep(next)
    },
  })

  const open = async () => {
    if (!archiveId) return
    await queryClient.invalidateQueries({ queryKey: ['archives'] })
    await navigate({ to: '/a/$archiveId', params: { archiveId } })
  }

  const back = () => setStep('choose')

  const title =
    step === 'progress'
      ? t.progressTitle
      : step === 'connect'
        ? strings.connect.title
        : step === 'upload'
          ? strings.importFiles.title
          : t.title
  const lead =
    step === 'progress'
      ? t.progressLead
      : step === 'connect'
        ? strings.connect.lead
        : step === 'upload'
          ? strings.importFiles.lead
          : t.lead

  return (
    <main className="relative min-h-full bg-bg px-4 py-16 text-fg">
      <div className="absolute top-2 right-2">
        <UserMenu />
      </div>
      <m.div className="mx-auto w-full max-w-xl" initial="hidden" animate="shown" variants={column}>
        <m.div variants={item}>
          <Wordmark />
        </m.div>
        <AnimatePresence mode="wait" initial={false}>
          <m.div
            key={step}
            initial={{ opacity: 0, x: distance.sm * 2 }}
            animate={{ opacity: 1, x: 0, transition: transition.expressive.moderate02 }}
            exit={{
              opacity: 0,
              x: -distance.sm * 2,
              transition: transition.expressiveExit.moderate02,
            }}
          >
            <m.h1 variants={item} className="mt-16 text-2xl font-normal text-fg">
              {title}
            </m.h1>
            <m.p variants={item} className="mt-2 max-w-lg text-sm text-fg-muted">
              {lead}
            </m.p>
            <m.div variants={item} className="mt-10">
              {step === 'choose' && (
                <div className="grid gap-8">
                  {!archiveId && (
                    <Field
                      label={t.archiveName}
                      hint={t.archiveHint}
                      value={name}
                      onChange={(event) => setName(event.target.value)}
                      maxLength={100}
                    />
                  )}
                  <div className="grid gap-px bg-border sm:grid-cols-2">
                    <Choice
                      icon={<MailPlusIcon />}
                      title={t.connect}
                      lead={t.connectLead}
                      busy={create.isPending && create.variables === 'connect'}
                      onClick={() => create.mutate('connect')}
                    />
                    <Choice
                      icon={<FileUpIcon />}
                      title={t.upload}
                      lead={t.uploadLead}
                      busy={create.isPending && create.variables === 'upload'}
                      onClick={() => create.mutate('upload')}
                    />
                  </div>
                  <FormError
                    message={create.isError ? authErrorMessage(create.error) : undefined}
                  />
                  {archiveId && (
                    <Button variant="ghost" className="w-fit" onClick={() => void open()}>
                      {t.open}
                      <ArrowRightIcon />
                    </Button>
                  )}
                </div>
              )}
              {step === 'connect' && signedIn?.kind === 'error' && (
                <div className="mb-8">
                  <OAuthReturnNote result={signedIn} onDismiss={dismissSignedIn} />
                </div>
              )}
              {step === 'connect' && archiveId && (
                <ConnectMailbox
                  archiveId={archiveId}
                  onConnected={() => setStep('progress')}
                  onCancel={back}
                />
              )}
              {step === 'upload' && archiveId && (
                <div className="grid gap-8">
                  <ImportFiles archiveId={archiveId} onUploaded={() => setUploaded(true)} />
                  <div className="flex items-center gap-2">
                    <Button variant="ghost" className="h-12 px-4" onClick={back}>
                      <ArrowLeftIcon />
                      {t.back}
                    </Button>
                    <Button
                      variant="primary"
                      className="h-12 flex-1 justify-between px-4"
                      disabled={!uploaded}
                      onClick={() => setStep('progress')}
                    >
                      {t.open}
                      <ArrowRightIcon />
                    </Button>
                  </div>
                </div>
              )}
              {step === 'progress' && archiveId && (
                <Progress archiveId={archiveId} onOpen={() => void open()} onAnother={back} />
              )}
            </m.div>
          </m.div>
        </AnimatePresence>
      </m.div>
    </main>
  )
}

function Choice({
  icon,
  title,
  lead,
  busy,
  onClick,
}: {
  icon: ReactNode
  title: string
  lead: string
  busy: boolean
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={busy}
      aria-busy={busy}
      className={cn(
        'group relative grid min-h-44 content-between gap-6 bg-bg p-6 text-left outline-none',
        'transition-colors duration-moderate-01 ease-productive hover:bg-bg-hover',
        'focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-focus',
      )}
    >
      <span className="text-accent [&_svg]:size-6 [&_svg]:stroke-[1.5]">{icon}</span>
      <span className="grid gap-1">
        <span className="flex items-center justify-between gap-2 text-base text-fg">
          {busy ? t.working : title}
          <ArrowRightIcon
            aria-hidden
            className="size-4 -translate-x-1 text-fg-subtle opacity-0 transition duration-moderate-01 ease-productive group-hover:translate-x-0 group-hover:opacity-100"
          />
        </span>
        <span className="text-sm text-fg-muted">{lead}</span>
      </span>
      <span
        aria-hidden
        className="absolute inset-x-0 bottom-0 h-0.5 origin-left scale-x-0 bg-accent transition-transform duration-moderate-02 ease-expressive group-hover:scale-x-100"
      />
    </button>
  )
}

function Progress({
  archiveId,
  onOpen,
  onAnother,
}: {
  archiveId: string
  onOpen: () => void
  onAnother: () => void
}) {
  useLiveArchive(archiveId)
  return (
    <div className="grid gap-8">
      <AccountList archiveId={archiveId} manage={false} />
      <div className="flex flex-wrap items-center gap-2">
        <Button variant="ghost" className="h-12 px-4" onClick={onAnother}>
          {t.another}
        </Button>
        <Button variant="primary" className="h-12 flex-1 justify-between px-4" onClick={onOpen}>
          {t.open}
          <ArrowRightIcon />
        </Button>
      </div>
    </div>
  )
}
