import type { Account } from '@holtmail/api-client'
import { useState } from 'react'

import { ConnectGraphForm } from './connect-graph.tsx'
import { ConnectImapForm } from './connect-imap.tsx'
import { ConnectJmapForm } from './connect-jmap.tsx'

export function ConnectMailbox(props: {
  archiveId: string
  onConnected: (account: Account) => void
  onCancel?: () => void
}) {
  const [protocol, setProtocol] = useState<'imap' | 'jmap' | 'graph'>('imap')
  if (protocol === 'jmap')
    return <ConnectJmapForm {...props} onUseImap={() => setProtocol('imap')} />
  if (protocol === 'graph')
    return <ConnectGraphForm {...props} onUseImap={() => setProtocol('imap')} />
  return (
    <ConnectImapForm
      {...props}
      onUseJmap={() => setProtocol('jmap')}
      onUseGraph={() => setProtocol('graph')}
    />
  )
}
