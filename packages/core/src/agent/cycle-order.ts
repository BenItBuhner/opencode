export const PRIMARY_AGENT_CYCLE_ORDER = ["build", "plan", "goal", "ask"] as const

type AgentLike = { name: string }

export function primaryAgentSortRank(name: string): number {
  const index = PRIMARY_AGENT_CYCLE_ORDER.indexOf(name as (typeof PRIMARY_AGENT_CYCLE_ORDER)[number])
  return index === -1 ? PRIMARY_AGENT_CYCLE_ORDER.length : index
}

export function comparePrimaryAgents(a: AgentLike, b: AgentLike): number {
  const diff = primaryAgentSortRank(a.name) - primaryAgentSortRank(b.name)
  if (diff !== 0) return diff
  return a.name.localeCompare(b.name)
}

export function orderPrimaryAgents<T extends AgentLike>(agents: readonly T[]): T[] {
  return [...agents].sort(comparePrimaryAgents)
}
