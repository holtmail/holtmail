import { accountsQuery, sessionQuery } from '@holtmail/api-client'
import { distance, transition } from '@holtmail/tokens'
import { Button } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ArchiveIcon, LoaderCircleIcon, MailPlusIcon } from 'lucide-react'
import { m } from 'motion/react'
import type { ReactNode } from 'react'

import { anyRunning } from '../lib/accounts.ts'
import { manages } from '../lib/roles.ts'
import { strings } from '../strings.ts'
import { useMail } from './context.ts'

export function EmptyState({
  icon,
  title,
  children,
}: {
  icon: ReactNode
  title: string
  children?: ReactNode
}) {
  return (
    <m.div
      className="flex h-full flex-col items-center justify-center gap-3 px-8 py-16 text-center"
      initial={{ opacity: 0, y: distance.xs }}
      animate={{ opacity: 1, y: 0, transition: transition.expressive.slow01 }}
    >
      <div className="text-fg-subtle [&_svg]:size-8 [&_svg]:stroke-1">{icon}</div>
      <p className="text-base text-fg">{title}</p>
      {children && <div className="max-w-xs text-sm text-fg-muted">{children}</div>}
    </m.div>
  )
}

export function EmptyArchive() {
  const { archiveId } = useMail()
  const user = useQuery(sessionQuery).data
  const accounts = useQuery(accountsQuery(archiveId))
  if (anyRunning(accounts.data)) {
    return (
      <EmptyState
        icon={<LoaderCircleIcon className="animate-spin" />}
        title={strings.archive.syncing}
      >
        {strings.welcome.progressLead}
      </EmptyState>
    )
  }
  return (
    <EmptyState icon={<ArchiveIcon />} title={strings.archive.emptyTitle}>
      <p>{strings.archive.emptyLead}</p>
      {manages(user?.role) && (
        <Button asChild variant="primary" className="mt-6">
          <Link to="/settings/archives" search={{ archive: archiveId, add: 'connect' }}>
            <MailPlusIcon />
            {strings.archive.emptyAction}
          </Link>
        </Button>
      )}
    </EmptyState>
  )
}
