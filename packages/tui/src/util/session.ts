export const BTW_TITLE_PREFIX = "BTW - "
export const BTW_METADATA = { btw: true } as const

export function isDefaultTitle(title: string) {
  return /^(New session - |Child session - )\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/.test(title)
}

export function createBtwTitle(input: string | Date = new Date()) {
  if (input instanceof Date) return BTW_TITLE_PREFIX + input.toISOString()
  const normalized = input.replace(/\s+/g, " ").trim()
  if (!normalized) return BTW_TITLE_PREFIX + new Date().toISOString()
  if (normalized.length <= 64) return BTW_TITLE_PREFIX + normalized
  return BTW_TITLE_PREFIX + normalized.slice(0, 61) + "..."
}

export function isBtwSession(input: { parentID?: string; title: string; metadata?: Record<string, unknown> }) {
  return input.parentID !== undefined && (input.metadata?.btw === true || input.title.startsWith(BTW_TITLE_PREFIX))
}

export function parseBtwPrompt(input: string) {
  if (input !== "/btw" && !/^\/btw\s/.test(input)) return undefined
  return input.slice("/btw".length).trim()
}

export function btwSessions<T extends { parentID?: string; title: string; time: { updated: number } }>(
  parentID: string,
  sessions: readonly T[],
) {
  return sessions
    .filter((session) => session.parentID === parentID && isBtwSession(session))
    .toSorted((a, b) => b.time.updated - a.time.updated)
}
