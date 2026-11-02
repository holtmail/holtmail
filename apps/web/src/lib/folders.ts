import type { Account, Folder } from '@holtmail/api-client'

const roleOrder = [
  'inbox',
  'flagged',
  'important',
  'drafts',
  'sent',
  'archive',
  'all',
  'junk',
  'trash',
] as const

type FolderRole = (typeof roleOrder)[number]

function folderRole(folder: Pick<Folder, 'role'>): FolderRole | undefined {
  return roleOrder.find((role) => role === folder.role)
}

export interface FolderNode {
  folder: Folder
  depth: number
}

export interface AccountFolders {
  accountId: string
  name: string
  address?: string | null
  folders: FolderNode[]
}

const collator = new Intl.Collator(undefined, { sensitivity: 'base', numeric: true })

function compareFolders(a: Folder, b: Folder): number {
  const roleA = folderRole(a)
  const roleB = folderRole(b)
  if (roleA && roleB) return roleOrder.indexOf(roleA) - roleOrder.indexOf(roleB)
  if (roleA) return -1
  if (roleB) return 1
  return collator.compare(a.name, b.name) || collator.compare(a.path, b.path)
}

function parentPath(path: string): string | undefined {
  const cut = path.lastIndexOf('/')
  return cut > 0 ? path.slice(0, cut) : undefined
}

export function orderFolders(folders: readonly Folder[]): FolderNode[] {
  const byPath = new Map(folders.map((folder) => [folder.path, folder]))
  const children = new Map<string | undefined, Folder[]>()
  for (const folder of folders) {
    let parent: string | undefined
    if (!folderRole(folder)) {
      parent = parentPath(folder.path)
      while (parent !== undefined && !byPath.has(parent)) parent = parentPath(parent)
    }
    const list = children.get(parent) ?? []
    list.push(folder)
    children.set(parent, list)
  }
  const result: FolderNode[] = []
  const visit = (parent: string | undefined, depth: number) => {
    const list = (children.get(parent) ?? []).toSorted(compareFolders)
    for (const folder of list) {
      result.push({ folder, depth })
      visit(folder.path, depth + 1)
    }
  }
  visit(undefined, 0)
  return result
}

export function groupFolders(
  folders: readonly Folder[],
  accounts: readonly Account[] = [],
): AccountFolders[] {
  const byAccount = new Map<string, Folder[]>()
  for (const folder of folders) {
    const list = byAccount.get(folder.account_id) ?? []
    list.push(folder)
    byAccount.set(folder.account_id, list)
  }
  const known = accounts.filter((account) => byAccount.has(account.id))
  const unknown = [...byAccount.keys()].filter((id) => !accounts.some((a) => a.id === id))
  return [
    ...known.map((account) => ({
      accountId: account.id,
      name: account.name,
      address: account.address,
      folders: orderFolders(byAccount.get(account.id) ?? []),
    })),
    ...unknown.map((id) => ({
      accountId: id,
      name: id,
      folders: orderFolders(byAccount.get(id) ?? []),
    })),
  ]
}

export function findInbox(folders: readonly Folder[]): Folder | undefined {
  return folders.find((folder) => folder.role === 'inbox')
}
