import type { Message, UserMessage } from "@opencode-ai/sdk/v2"
import { RGBA } from "@opentui/core"
import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show } from "solid-js"
import { useTheme } from "../context/theme"
import { Locale } from "../util/locale"
import { formatDuration } from "../util/format"

export type GoalSummaryView = {
  id?: string
  created?: number
  progress?: number
  summary?: string
  headline?: string
}

export type GoalSummariesView = {
  text: string
  progress?: number
  summaries?: GoalSummaryView[]
}

export type GoalStatusView = GoalSummariesView & {
  status: string
  created?: number
}

export function parseGoalStatus(value: unknown): GoalStatusView | undefined {
  if (!value || typeof value !== "object") return undefined
  const item = value as {
    text?: unknown
    status?: unknown
    created?: unknown
    progress?: unknown
    summaries?: unknown
  }
  if (typeof item.text !== "string" || typeof item.status !== "string") return undefined

  const summaries = Array.isArray(item.summaries)
    ? item.summaries
        .filter((summary): summary is GoalSummaryView => summary !== null && typeof summary === "object")
        .map((summary) => ({
          id: typeof summary.id === "string" ? summary.id : undefined,
          created: typeof summary.created === "number" ? summary.created : undefined,
          progress: typeof summary.progress === "number" ? summary.progress : undefined,
          summary: typeof summary.summary === "string" ? summary.summary : undefined,
          headline: typeof summary.headline === "string" ? summary.headline : undefined,
        }))
    : undefined

  return {
    text: item.text,
    status: item.status,
    created: typeof item.created === "number" ? item.created : undefined,
    progress: typeof item.progress === "number" ? item.progress : summaries?.at(-1)?.progress,
    summaries,
  }
}

export function compactProgressBar(progress: number) {
  const value = Math.max(0, Math.min(100, Math.round(progress)))
  const segments = 12
  const filled = Math.round((value / 100) * segments)
  return {
    filled: "━".repeat(filled),
    empty: "─".repeat(segments - filled),
  }
}

export function createGoalStatus(props: {
  sessionID: () => string | undefined
  goal: (sessionID: string) => unknown
  messages: (sessionID: string) => readonly Message[]
}) {
  const sessionGoal = createMemo(() => {
    const sessionID = props.sessionID()
    if (!sessionID) return undefined
    return parseGoalStatus(props.goal(sessionID))
  })
  const latestUserMessage = createMemo(() => {
    const sessionID = props.sessionID()
    if (!sessionID) return undefined
    return props.messages(sessionID).findLast((message): message is UserMessage => message.role === "user")
  })
  const [retainedGoal, setRetainedGoal] = createSignal<GoalStatusView>()

  createEffect(
    on(
      () => props.sessionID(),
      () => setRetainedGoal(undefined),
    ),
  )
  createEffect(
    on(
      () => sessionGoal(),
      (goal) => {
        if (goal) setRetainedGoal(goal)
      },
    ),
  )
  createEffect(
    on(
      () => latestUserMessage()?.id,
      () => {
        if (sessionGoal()) return
        const latest = latestUserMessage()
        if (latest && latest.agent !== "goal") setRetainedGoal(undefined)
      },
    ),
  )

  return createMemo(() => sessionGoal() ?? retainedGoal())
}

export function createGoalElapsed(goal: () => GoalStatusView | undefined) {
  const [now, setNow] = createSignal(Date.now())

  onMount(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000)
    onCleanup(() => clearInterval(timer))
  })

  return createMemo(() => {
    const item = goal()
    if (!item || item.status !== "active" || item.created === undefined) return
    return formatDuration(Math.floor((now() - item.created) / 1000)) || "0s"
  })
}

export function goalDetailsMessage(goal: GoalStatusView, elapsed: string | undefined) {
  return [
    `Status: ${goal.status}`,
    elapsed ? `Running: ${elapsed}` : undefined,
    "",
    goal.text,
  ]
    .filter((line) => line !== undefined)
    .join("\n")
}

export function GoalInlineStatus(props: {
  goal: GoalStatusView
  elapsed?: string
  onDetails: () => void
  onSummaries: () => void
}) {
  const { theme } = useTheme()

  return (
    <box flexDirection="row" gap={1}>
      <text fg={theme.accent} onMouseUp={props.onDetails}>
        goal
      </text>
      <Show when={props.goal.progress !== undefined}>
        {(() => {
          const progressBar = createMemo(() => compactProgressBar(props.goal.progress ?? 0))
          return (
            <box flexDirection="row" gap={1} onMouseUp={props.onSummaries}>
              <text fg={theme.accent}>{props.goal.progress}%</text>
              <text wrapMode="none">
                <span style={{ fg: theme.accent }}>{progressBar().filled}</span>
                <span style={{ fg: theme.textMuted }}>{progressBar().empty}</span>
              </text>
            </box>
          )
        })()}
      </Show>
      <Show when={props.elapsed}>{(elapsed) => <text fg={theme.textMuted}>{elapsed()}</text>}</Show>
    </box>
  )
}

export function GoalSidebarStatus(props: {
  goal: GoalStatusView
  elapsed?: string
  onDetails: () => void
  onSummaries: () => void
}) {
  const { theme } = useTheme()
  const progressBar = createMemo(() => compactProgressBar(props.goal.progress ?? 0))
  const stateColor = createMemo(() => (props.goal.status === "active" ? theme.accent : theme.textMuted))
  const fadedAccent = createMemo(() => RGBA.fromValues(theme.accent.r, theme.accent.g, theme.accent.b, theme.accent.a * 0.45))

  return (
    <box gap={1}>
      <box flexDirection="row" justifyContent="space-between" gap={1}>
        <text fg={theme.text}>
          <b>Goal</b>
        </text>
        <text fg={stateColor()} onMouseUp={props.onDetails}>
          {Locale.titlecase(props.goal.status)}
        </text>
      </box>
      <text fg={theme.textMuted}>{Locale.truncate(props.goal.text, 34)}</text>
      <Show when={props.goal.progress !== undefined}>
        <box flexDirection="row" justifyContent="space-between" gap={1} onMouseUp={props.onSummaries}>
          <text wrapMode="none">
            <span style={{ fg: theme.accent }}>{progressBar().filled}</span>
            <span style={{ fg: fadedAccent() }}>{progressBar().empty}</span>
          </text>
          <text fg={theme.accent}>{props.goal.progress}%</text>
        </box>
      </Show>
      <Show when={props.elapsed}>{(elapsed) => <text fg={theme.textMuted}>Running {elapsed()}</text>}</Show>
    </box>
  )
}
