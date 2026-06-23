import { Effect, Schema } from "effect"
import * as Tool from "./tool"
import { Session } from "../session/session"

const EmptyParameters = Schema.Struct({})
const SetParameters = Schema.Struct({
  text: Schema.String.annotate({ description: "The durable session goal to work toward" }),
  complete_override_confirmation: Schema.optional(Schema.String).annotate({
    description:
      "Required only when replacing a different existing goal. Set this to the exact current goal text shown by the previous goal_set warning to confirm the override.",
  }),
})
const SummarizeStateParameters = Schema.Struct({
  progress: Session.GoalProgress.annotate({
    description: "Estimated goal completion percentage as an integer from 0 to 100.",
  }),
  summary: Schema.String.annotate({
    description:
      "A markdown state summary of goal progress. Use paragraphs, headings, bullets, numbered lists, code blocks, or other markdown structure that best fits the current state.",
  }),
  headline: Schema.optional(Schema.String).annotate({
    description: "Optional one-line preview of the current state for compact UI displays.",
  }),
})

type Metadata = {
  goal?: Session.Goal | null
  summary?: Session.GoalSummary | null
}

function formatGoal(goal: Session.Goal | undefined) {
  if (!goal) return "No session goal is currently set."
  return [
    `Goal: ${goal.text}`,
    `Status: ${goal.status}`,
    goal.progress === undefined ? undefined : `Progress: ${goal.progress}%`,
    `Revision: ${goal.revision ?? 0}`,
  ]
    .filter((line) => line !== undefined)
    .join("\n")
}

function validateSummaryFormat(summary: string) {
  if (!summary.trim()) throw new Error("Goal summary cannot be empty.")
}

function formatOverrideWarning(input: { existing: Session.Goal; requested: string }) {
  return [
    "A session goal is already set, and the requested goal would replace it.",
    "Only override the current goal if the user explicitly asked to change goals.",
    "",
    "Current goal, verbatim:",
    input.existing.text,
    "",
    "Requested replacement:",
    input.requested.trim(),
    "",
    "To confirm this destructive replacement, call goal_set again with the same text and:",
    `complete_override_confirmation: ${JSON.stringify(input.existing.text)}`,
  ].join("\n")
}

export const GoalSetTool = Tool.define<typeof SetParameters, Metadata, Session.Service>(
  "goal_set",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description:
        "Set the durable goal for this session and mark it active. If a different goal already exists, this tool refuses to replace it unless complete_override_confirmation exactly matches the current goal text from the previous warning.",
      parameters: SetParameters,
      execute: (params: Schema.Schema.Type<typeof SetParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_set",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const existing = yield* session.getGoal(ctx.sessionID).pipe(Effect.orDie)
          if (existing && existing.text.trim() !== params.text.trim()) {
            if (params.complete_override_confirmation !== existing.text) {
              throw new Error(formatOverrideWarning({ existing, requested: params.text }))
            }
          }

          const goal = yield* session
            .setGoal({ sessionID: ctx.sessionID, text: params.text, status: "active" })
            .pipe(Effect.orDie)
          return {
            title: "Goal set",
            output: formatGoal(goal),
            metadata: { goal },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof SetParameters, Metadata>
  }),
)

export const GoalPauseTool = Tool.define<typeof EmptyParameters, Metadata, Session.Service>(
  "goal_pause",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description: "Pause the active session goal. This does not switch agents.",
      parameters: EmptyParameters,
      execute: (_params: Schema.Schema.Type<typeof EmptyParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_pause",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "paused" }).pipe(Effect.orDie)
          return {
            title: goal ? "Goal paused" : "No goal",
            output: formatGoal(goal),
            metadata: { goal: goal ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof EmptyParameters, Metadata>
  }),
)

export const GoalResumeTool = Tool.define<typeof EmptyParameters, Metadata, Session.Service>(
  "goal_resume",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description: "Resume a paused session goal and mark it active. This does not switch agents by itself.",
      parameters: EmptyParameters,
      execute: (_params: Schema.Schema.Type<typeof EmptyParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_resume",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "active" }).pipe(Effect.orDie)
          return {
            title: goal ? "Goal resumed" : "No goal",
            output: formatGoal(goal),
            metadata: { goal: goal ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof EmptyParameters, Metadata>
  }),
)

export const GoalCompleteTool = Tool.define<typeof EmptyParameters, Metadata, Session.Service>(
  "goal_complete",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description: "Mark the current session goal as completed and clear it from the session.",
      parameters: EmptyParameters,
      execute: (_params: Schema.Schema.Type<typeof EmptyParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_complete",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "completed" }).pipe(Effect.orDie)
          if (goal) yield* session.clearGoal(ctx.sessionID).pipe(Effect.orDie)
          return {
            title: goal ? "Goal completed" : "No goal",
            output: formatGoal(goal),
            metadata: { goal: goal ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof EmptyParameters, Metadata>
  }),
)

export const GoalStatusTool = Tool.define<typeof EmptyParameters, Metadata, Session.Service>(
  "goal_status",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description: "Read the current durable session goal and status.",
      parameters: EmptyParameters,
      execute: (_params: Schema.Schema.Type<typeof EmptyParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_status",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.getGoal(ctx.sessionID).pipe(Effect.orDie)
          return {
            title: goal ? "Goal status" : "No goal",
            output: formatGoal(goal),
            metadata: { goal: goal ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof EmptyParameters, Metadata>
  }),
)

export const GoalSummarizeStateTool = Tool.define<typeof SummarizeStateParameters, Metadata, Session.Service>(
  "goal_summarize_state",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description:
        "Persist a structured progress snapshot for the active session goal. Use this periodically after meaningful progress, before pausing, and before completing the goal.",
      parameters: SummarizeStateParameters,
      execute: (params: Schema.Schema.Type<typeof SummarizeStateParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_summarize_state",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          validateSummaryFormat(params.summary)

          const goal = yield* session
            .addGoalSummary({
              sessionID: ctx.sessionID,
              progress: params.progress,
              summary: params.summary,
              headline: params.headline,
            })
            .pipe(Effect.orDie)
          const summary = goal?.summaries?.at(-1)
          return {
            title: goal ? "Goal state summarized" : "No goal",
            output: goal
              ? [formatGoal(goal), "", "Latest summary:", params.summary].join("\n")
              : "No session goal is currently set.",
            metadata: { goal: goal ?? null, summary: summary ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof SummarizeStateParameters, Metadata>
  }),
)
