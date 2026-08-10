export * as GoalTool from "./goal"

import { ToolFailure } from "@opencode-ai/llm"
import { Effect, Layer, Schema } from "effect"
import { makeLocationNode } from "../effect/app-node"
import { PermissionV2 } from "../permission"
import { SessionV2 } from "../session"
import { ToolRegistry } from "./registry"
import { Tool } from "./tool"
import { Tools } from "./tools"

const EmptyInput = Schema.Struct({})
const SetInput = Schema.Struct({
  text: Schema.String.annotate({ description: "The durable session goal to work toward." }),
})
const SummarizeStateInput = Schema.Struct({
  progress: SessionV2.GoalProgress.annotate({
    description: "Estimated goal completion percentage as an integer from 0 to 100.",
  }),
  summary: Schema.String.annotate({
    description:
      "A structured markdown state summary. Must include ## Progress, ## Current State, ## Blockers, and ## Next Steps sections, each with bullet items.",
  }),
  headline: Schema.String.pipe(Schema.optional).annotate({
    description: "Optional one-line preview of the current state for compact UI displays.",
  }),
})
const Output = Schema.Struct({
  title: Schema.String,
  output: Schema.String,
  goal: SessionV2.Goal.pipe(Schema.optional),
  summary: SessionV2.GoalSummary.pipe(Schema.optional),
})

const names = {
  set: "goal_set",
  pause: "goal_pause",
  resume: "goal_resume",
  complete: "goal_complete",
  status: "goal_status",
  summarizeState: "goal_summarize_state",
} as const
const REQUIRED_SUMMARY_SECTIONS = ["Progress", "Current State", "Blockers", "Next Steps"]

const layer = Layer.effectDiscard(
  Effect.gen(function* () {
    const tools = yield* Tools.Service
    const sessions = yield* SessionV2.Service
    const permission = yield* PermissionV2.Service

    const ask = (action: string, context: Tool.Context) =>
      permission
        .assert({
          action,
          resources: ["*"],
          save: ["*"],
          sessionID: context.sessionID,
          agent: context.agent,
          source: { type: "tool", messageID: context.assistantMessageID, callID: context.toolCallID },
        })
        .pipe(Effect.mapError(() => new ToolFailure({ message: `Permission denied: ${action}` })))

    yield* tools
      .register({
        [names.set]: Tool.make({
          description: "Set or replace the durable goal for this session and mark it active.",
          input: SetInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (input, context) =>
            Effect.gen(function* () {
              yield* ask(names.set, context)
              const goal = yield* sessions.setGoal({ sessionID: context.sessionID, text: input.text, status: "active" })
              return { title: "Goal set", output: formatGoal(goal), goal }
            }),
        }),
        [names.pause]: Tool.make({
          description: "Pause the active session goal. This does not switch agents.",
          input: EmptyInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (_input, context) =>
            Effect.gen(function* () {
              yield* ask(names.pause, context)
              const goal = yield* sessions.updateGoal({ sessionID: context.sessionID, status: "paused" })
              return { title: goal ? "Goal paused" : "No goal", output: formatGoal(goal), goal }
            }),
        }),
        [names.resume]: Tool.make({
          description: "Resume a paused session goal and mark it active. This does not switch agents by itself.",
          input: EmptyInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (_input, context) =>
            Effect.gen(function* () {
              yield* ask(names.resume, context)
              const goal = yield* sessions.updateGoal({ sessionID: context.sessionID, status: "active" })
              return { title: goal ? "Goal resumed" : "No goal", output: formatGoal(goal), goal }
            }),
        }),
        [names.complete]: Tool.make({
          description: "Mark the current session goal as completed and clear it from the session.",
          input: EmptyInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (_input, context) =>
            Effect.gen(function* () {
              yield* ask(names.complete, context)
              const goal = yield* sessions.updateGoal({ sessionID: context.sessionID, status: "completed" })
              if (goal) yield* sessions.clearGoal(context.sessionID)
              return { title: goal ? "Goal completed" : "No goal", output: formatGoal(goal), goal }
            }),
        }),
        [names.status]: Tool.make({
          description: "Read the current durable session goal and status.",
          input: EmptyInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (_input, context) =>
            Effect.gen(function* () {
              yield* ask(names.status, context)
              const goal = yield* sessions.getGoal(context.sessionID)
              return { title: goal ? "Goal status" : "No goal", output: formatGoal(goal), goal }
            }),
        }),
        [names.summarizeState]: Tool.make({
          description:
            "Persist a structured progress snapshot for the active session goal. Use this periodically after meaningful progress, before pausing, and before completing the goal.",
          input: SummarizeStateInput,
          output: Output,
          toModelOutput: ({ output }) => [{ type: "text", text: output.output }],
          execute: (input, context) =>
            Effect.gen(function* () {
              yield* ask(names.summarizeState, context)
              const validation = validateSummaryFormat(input.summary)
              if (validation) return yield* Effect.fail(new ToolFailure({ message: validation }))
              const goal = yield* sessions.addGoalSummary({
                sessionID: context.sessionID,
                progress: input.progress,
                summary: input.summary,
                headline: input.headline,
              })
              const summary = goal?.summaries?.at(-1)
              return {
                title: goal ? "Goal state summarized" : "No goal",
                output: goal
                  ? [formatGoal(goal), "", "Latest summary:", input.summary].join("\n")
                  : "No session goal is currently set.",
                goal,
                summary,
              }
            }),
        }),
      })
      .pipe(Effect.orDie)
  }),
)

export const node = makeLocationNode({
  name: "tool/goal",
  layer,
  deps: [ToolRegistry.node, PermissionV2.node, SessionV2.node],
})

function formatGoal(goal: SessionV2.Goal | undefined) {
  if (!goal) return "No session goal is currently set."
  return [
    `Goal: ${goal.text}`,
    `Status: ${goal.status}`,
    goal.progress === undefined ? undefined : `Progress: ${goal.progress}%`,
    `Revision: ${goal.revision ?? 0}`,
  ]
    .filter((line): line is string => line !== undefined)
    .join("\n")
}

function validateSummaryFormat(summary: string) {
  const sections = new Map<string, number>()
  let current: string | undefined

  for (const raw of summary.trim().split(/\r?\n/)) {
    const line = raw.trimEnd()
    if (!line.trim()) continue

    const heading = line.match(/^(#{1,6})\s+(.+)$/)
    if (heading) {
      if (heading[1] !== "##") return `Goal summary headings must use size 2 markdown headers: ${line}`
      current = heading[2]?.trim()
      if (!current) return "Goal summary headings cannot be empty."
      sections.set(current, sections.get(current) ?? 0)
      continue
    }

    if (!current) return "Goal summary content must appear under size 2 markdown headers."
    if (!line.trimStart().startsWith("- ")) return `Goal summary section "${current}" must use bullet list items.`
    sections.set(current, (sections.get(current) ?? 0) + 1)
  }

  const missing = REQUIRED_SUMMARY_SECTIONS.find((section) => !sections.has(section))
  if (missing) return `Goal summary is missing the ## ${missing} section.`
  const empty = Array.from(sections).find((entry) => entry[1] === 0)?.[0]
  if (empty) return `Goal summary section ## ${empty} needs at least one bullet.`
}
