import { collectionsQuery } from '@holtmail/api-client'
import { Button } from '@holtmail/ui'
import { useQuery } from '@tanstack/react-query'
import { PlusIcon } from 'lucide-react'
import { useState } from 'react'

import { type EditKind, editableCollections } from '../lib/edits.ts'
import { strings } from '../strings.ts'
import { EditDialog } from './edit-dialog.tsx'

const t = strings.edits

/** "New event" or "New contact", shown only while one of the archive's calendars or address
 * books can be edited by the user. */
export function NewItemButton({
  archiveId,
  kind,
  day,
  iconOnly = false,
}: {
  archiveId: string
  kind: EditKind
  day?: string
  iconOnly?: boolean
}) {
  const collections = useQuery(collectionsQuery(archiveId)).data
  const [open, setOpen] = useState(false)
  if (editableCollections(collections, kind).length === 0) return null
  const label = kind === 'contact' ? t.actions.newContact : t.actions.newEvent
  return (
    <>
      <Button
        variant={iconOnly ? 'ghost' : 'secondary'}
        size={iconOnly ? 'icon' : 'sm'}
        aria-label={iconOnly ? label : undefined}
        title={label}
        onClick={() => setOpen(true)}
      >
        <PlusIcon />
        {!iconOnly && label}
      </Button>
      {open && (
        <EditDialog
          target={{ mode: 'create', kind, archiveId, day }}
          onClose={() => setOpen(false)}
        />
      )}
    </>
  )
}
