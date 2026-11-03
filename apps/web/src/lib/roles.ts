export const manages = (role: string | undefined): boolean => role === 'owner' || role === 'admin'
