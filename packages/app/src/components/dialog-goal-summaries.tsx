import { Component, createMemo, createSignal, For, Show } from "solid-js"
import { Dialog } from "@opencode-ai/ui/dialog"
import { Button } from "@opencode-ai/ui/button"
import { useSync } from "@/context/sync"
import { useLanguage } from "@/context/language"

export type GoalSummaryView = {
  id?: string
  created?: number
  progress?: number
  summary?: string
  headline?: string
}

export type GoalView = {
  text: string
  status: string
  created?: number
  progress?: number
  headline?: string
  summaries?: GoalSummaryView[]
}

export function compactGoalProgressBar(progress: number) {
  const value = Math.max(0, Math.min(100, Math.round(progress)))
  const filled = Math.round(value / 20)
  return `${"#".repeat(filled)}${"-".repeat(5 - filled)}`
}

export function goalFromSessionMetadata(metadata: Record<string, unknown> | undefined): GoalView | undefined {
  const goal = metadata?.goal
  if (!goal || typeof goal !== "object") return undefined
  const item = goal as {
    text?: unknown
    status?: unknown
    created?: unknown
    progress?: unknown
    summaries?: unknown
  }
  if (typeof item.text !== "string" || typeof item.status !== "string") return undefined
  const summaries = Array.isArray(item.summaries)
    ? item.summaries
        .filter((summary): summary is Record<string, unknown> => summary !== null && typeof summary === "object")
        .map(
          (summary): GoalSummaryView => ({
            id: typeof summary.id === "string" ? summary.id : undefined,
            created: typeof summary.created === "number" ? summary.created : undefined,
            progress: typeof summary.progress === "number" ? summary.progress : undefined,
            summary: typeof summary.summary === "string" ? summary.summary : undefined,
            headline: typeof summary.headline === "string" ? summary.headline : undefined,
          }),
        )
    : undefined
  return {
    text: item.text,
    status: item.status,
    created: typeof item.created === "number" ? item.created : undefined,
    progress: typeof item.progress === "number" ? item.progress : summaries?.at(-1)?.progress,
    headline: summaries?.at(-1)?.headline,
    summaries,
  }
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

function clampProgress(progress: number | undefined) {
  return Math.max(0, Math.min(100, Math.round(progress ?? 0)))
}

const ProgressBar: Component<{ progress: number }> = (props) => {
  return (
    <div class="flex items-center gap-2 shrink-0">
      <div class="w-24 h-1 rounded-full bg-surface-raised-base overflow-hidden">
        <div class="h-full rounded-full bg-icon-info-base" style={{ width: `${clampProgress(props.progress)}%` }} />
      </div>
      <span class="text-12-regular text-text-muted tabular-nums">{clampProgress(props.progress)}%</span>
    </div>
  )
}

const SummaryCard: Component<{ summary: GoalSummaryView; latest?: boolean }> = (props) => {
  const language = useLanguage()
  const parsed = createMemo(() => sections(props.summary.summary))

  return (
    <div class="flex flex-col gap-2 rounded-md border border-border-subtle p-3">
      <div class="flex items-center justify-between gap-2">
        <span
          class="text-13-medium"
          classList={{ "text-text-strong": !!props.latest, "text-text-base": !props.latest }}
        >
          {props.latest
            ? language.t("dialog.goal.latest")
            : (props.summary.headline ?? language.t("dialog.goal.previous"))}
        </span>
        <ProgressBar progress={props.summary.progress ?? 0} />
      </div>
      <Show when={props.summary.created}>
        {(created) => <span class="text-12-regular text-text-weak">{new Date(created()).toLocaleString()}</span>}
      </Show>
      <For each={parsed()}>
        {(section) => (
          <div class="flex flex-col gap-1">
            <span class="text-12-medium text-text-base">{section.title}</span>
            <ul class="flex flex-col gap-0.5">
              <For each={section.bullets}>
                {(bullet) => <li class="text-12-regular text-text-muted">- {bullet}</li>}
              </For>
            </ul>
          </div>
        )}
      </For>
    </div>
  )
}

export const DialogGoalSummaries: Component<{ sessionID: string }> = (props) => {
  const sync = useSync()
  const language = useLanguage()
  const [showMore, setShowMore] = createSignal(false)

  const goal = createMemo(() => goalFromSessionMetadata(sync().session.get(props.sessionID)?.metadata))
  const summaries = createMemo(() => goal()?.summaries ?? [])
  const latest = createMemo(() => summaries().at(-1))
  const older = createMemo(() => summaries().slice(0, -1).toReversed())

  return (
    <Dialog title={language.t("dialog.goal.title")}>
      <Show when={goal()}>
        {(item) => (
          <div class="flex flex-col gap-3 px-4 pb-4 min-h-0">
            <div class="flex items-center justify-between gap-3">
              <span class="truncate text-13-regular text-text-base" title={item().text}>
                {item().text}
              </span>
              <ProgressBar progress={item().progress ?? latest()?.progress ?? 0} />
            </div>
            <div class="flex flex-col gap-2 overflow-y-auto min-h-0 max-h-[50vh]">
              <Show
                when={latest()}
                fallback={<span class="text-13-regular text-text-muted">{language.t("dialog.goal.empty")}</span>}
              >
                {(summary) => <SummaryCard latest summary={summary()} />}
              </Show>
              <Show when={showMore()}>
                <For each={older()}>{(summary) => <SummaryCard summary={summary} />}</For>
              </Show>
            </div>
            <Show when={older().length > 0 && !showMore()}>
              <div class="flex justify-end">
                <Button variant="secondary" size="small" onClick={() => setShowMore(true)}>
                  {language.t("dialog.goal.showMore")}
                </Button>
              </div>
            </Show>
          </div>
        )}
      </Show>
    </Dialog>
  )
}
