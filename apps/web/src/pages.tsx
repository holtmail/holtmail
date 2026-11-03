import { Button } from '@holtmail/ui'
import { type ErrorComponentProps, Link } from '@tanstack/react-router'
import { ArchiveIcon, CircleAlertIcon, CompassIcon } from 'lucide-react'

import { EmptyState } from './mail/empty-state.tsx'
import { UserMenu } from './mail/user-menu.tsx'
import { strings } from './strings.ts'

export function NoArchives() {
  return (
    <main className="relative h-full bg-bg text-fg">
      <div className="absolute top-2 right-2">
        <UserMenu />
      </div>
      <EmptyState icon={<ArchiveIcon />} title={strings.welcome.memberTitle}>
        {strings.welcome.memberLead}
      </EmptyState>
    </main>
  )
}

export function ArchivesFailed({ error }: ErrorComponentProps) {
  return (
    <main className="h-full bg-bg text-fg">
      <EmptyState icon={<CircleAlertIcon />} title={strings.archive.loadFailed}>
        {error instanceof Error ? error.message : String(error)}
      </EmptyState>
    </main>
  )
}

export function ServerFailed({ error }: ErrorComponentProps) {
  return (
    <main className="h-full bg-bg text-fg">
      <EmptyState icon={<CircleAlertIcon />} title={strings.serverFailed}>
        {error instanceof Error ? error.message : String(error)}
      </EmptyState>
    </main>
  )
}

export function NotFound() {
  return (
    <main className="h-full bg-bg text-fg">
      <EmptyState icon={<CompassIcon />} title={strings.notFound}>
        <Button asChild size="sm" className="mt-4">
          <Link to="/">{strings.goHome}</Link>
        </Button>
      </EmptyState>
    </main>
  )
}
