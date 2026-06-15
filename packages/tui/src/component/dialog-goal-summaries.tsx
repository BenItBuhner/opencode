import { TextAttributes } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { createMemo, createSignal, For, onMount, Show } from "solid-js"
import { Locale } from "../util/locale"
import { useTheme } from "../context/theme"
import { useDialog } from "../ui/dialog"
import { getScrollAcceleration } from "../util/scroll"

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

function progressBar(progress: number) {
  const value = Math.max(0, Math.min(100, Math.round(progress)))
  const filled = Math.round(value / 10)
  return `[${"#".repeat(filled)}${"-".repeat(10 - filled)}]`
}

function sections(markdown: string | undefined) {
  const result: { title: string; bullets: string[] }[] = []
  let current: { title: string; bullets: string[] } | undefined
  for (const raw of (markdown ?? "").split(/\r?\n/)) {
    const line = raw.trim()
    if (!line) continue
    if (line.startsWith("## ")) {
      current = { title: line.slice(3).trim(), bullets: [] }
      result.push(current)
      continue
    }
    if (!current || !line.startsWith("- ")) continue
    current.bullets.push(line.slice(2).trim())
  }
  return result
}

function SummaryCard(props: { summary: GoalSummaryView; latest?: boolean }) {
  const { theme } = useTheme()
  const parsed = createMemo(() => sections(props.summary.summary))
  const progress = createMemo(() => Math.max(0, Math.min(100, Math.round(props.summary.progress ?? 0))))

  return (
    <box gap={1} paddingTop={props.latest ? 0 : 1}>
      <box flexDirection="row" justifyContent="space-between">
        <text attributes={TextAttributes.BOLD} fg={props.latest ? theme.accent : theme.text}>
          {props.latest ? "Latest State" : (props.summary.headline ?? "Previous State")}
        </text>
        <text fg={theme.textMuted}>
          {progress()}% {progressBar(progress())}
        </text>
      </box>
      <Show when={props.summary.created}>
        {(created) => <text fg={theme.textMuted}>{Locale.datetime(created())}</text>}
      </Show>
      <For each={parsed()}>
        {(section) => (
          <box gap={1}>
            <text attributes={TextAttributes.BOLD} fg={theme.text}>
              {section.title}
            </text>
            <For each={section.bullets}>{(bullet) => <text fg={theme.textMuted}>- {bullet}</text>}</For>
          </box>
        )}
      </For>
    </box>
  )
}

export function DialogGoalSummaries(props: { goal: GoalSummariesView }) {
  const dialog = useDialog()
  const { theme } = useTheme()
  const dimensions = useTerminalDimensions()
  const scrollAcceleration = getScrollAcceleration()
  const [showMore, setShowMore] = createSignal(false)

  onMount(() => {
    dialog.setSize("large")
  })

  const summaries = createMemo(() => props.goal.summaries ?? [])
  const latest = createMemo(() => summaries().at(-1))
  const older = createMemo(() => summaries().slice(0, -1).toReversed())
  const progress = createMemo(() => Math.max(0, Math.min(100, Math.round(props.goal.progress ?? latest()?.progress ?? 0))))
  const bodyHeight = () => Math.max(4, dimensions().height - 13)

  return (
    <box paddingLeft={2} paddingRight={2} gap={1}>
      <box flexDirection="row" justifyContent="space-between">
        <text attributes={TextAttributes.BOLD} fg={theme.text}>
          Goal State Summaries
        </text>
        <text fg={theme.textMuted} onMouseUp={() => dialog.clear()}>
          esc
        </text>
      </box>
      <box flexDirection="row" justifyContent="space-between">
        <text fg={theme.textMuted}>{Locale.truncate(props.goal.text, 58)}</text>
        <text fg={theme.accent}>
          {progress()}% {progressBar(progress())}
        </text>
      </box>
      <scrollbox
        maxHeight={bodyHeight()}
        scrollbarOptions={{ visible: true }}
        scrollAcceleration={scrollAcceleration}
      >
        <Show
          when={latest()}
          fallback={<text fg={theme.textMuted}>No goal state summaries have been recorded yet.</text>}
        >
          {(item) => <SummaryCard latest summary={item()} />}
        </Show>
        <Show when={showMore()}>
          <For each={older()}>{(item) => <SummaryCard summary={item} />}</For>
        </Show>
      </scrollbox>
      <Show when={older().length > 0 && !showMore()}>
        <box flexDirection="row" justifyContent="flex-end" paddingBottom={1}>
          <box paddingLeft={2} paddingRight={2} backgroundColor={theme.primary} onMouseUp={() => setShowMore(true)}>
            <text fg={theme.selectedListItemText}>show more</text>
          </box>
        </box>
      </Show>
    </box>
  )
}
