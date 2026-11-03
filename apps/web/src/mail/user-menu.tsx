import { logout, sessionQuery } from '@holtmail/api-client'
import { themes } from '@holtmail/tokens'
import {
  Button,
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@holtmail/ui'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { LogOutIcon, MonitorIcon, MoonIcon, SettingsIcon, SunIcon } from 'lucide-react'

import { initials } from '../lib/auth.ts'
import { strings } from '../strings.ts'
import { parseTheme, useTheme } from '../theme.ts'

const themeIcons = { light: SunIcon, dark: MoonIcon, system: MonitorIcon } as const

export function UserMenu() {
  const user = useQuery(sessionQuery).data
  const [theme, setTheme] = useTheme()
  const queryClient = useQueryClient()
  const navigate = useNavigate()

  const signOut = async () => {
    try {
      await logout()
    } finally {
      queryClient.clear()
      await navigate({ to: '/login', replace: true })
    }
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" aria-label={strings.user.menu}>
          <span
            aria-hidden
            className="flex size-6 items-center justify-center bg-accent text-xs font-semibold text-accent-fg"
          >
            {user ? initials(user.name, user.email) : ''}
          </span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-64">
        {user && (
          <>
            <div className="grid gap-0.5 px-3 py-2">
              <span className="truncate text-sm font-medium text-fg">{user.name}</span>
              <span className="truncate font-mono text-xs text-fg-subtle">{user.email}</span>
            </div>
            <DropdownMenuSeparator />
          </>
        )}
        <DropdownMenuLabel>{strings.theme.label}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={theme}
          onValueChange={(value) => setTheme(parseTheme(value))}
        >
          {themes.map((value) => {
            const Icon = themeIcons[value]
            return (
              <DropdownMenuRadioItem key={value} value={value}>
                <Icon />
                {strings.theme.names[value]}
              </DropdownMenuRadioItem>
            )
          })}
        </DropdownMenuRadioGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem asChild>
          <Link to="/settings">
            <SettingsIcon />
            {strings.user.settings}
          </Link>
        </DropdownMenuItem>
        <DropdownMenuItem onSelect={() => void signOut()}>
          <LogOutIcon />
          {strings.user.signOut}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
