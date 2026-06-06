import { Effect, Schema } from "effect"
import * as Tool from "./tool"
import { Session } from "../session/session"

const EmptyParameters = Schema.Struct({})
const SetParameters = Schema.Struct({
  text: Schema.String.annotate({ description: "The durable session goal to work toward" }),
})

type Metadata = {
  goal?: Session.Goal | null
}

function formatGoal(goal: Session.Goal | undefined) {
  if (!goal) return "No session goal is currently set."
  return [`Goal: ${goal.text}`, `Status: ${goal.status}`, `Revision: ${goal.revision ?? 0}`].join("\n")
}

export const GoalSetTool = Tool.define<typeof SetParameters, Metadata, Session.Service>(
  "goal_set",
  Effect.gen(function* () {
    const session = yield* Session.Service

    return {
      description: "Set or replace the durable goal for this session and mark it active.",
      parameters: SetParameters,
      execute: (params: Schema.Schema.Type<typeof SetParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_set",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.setGoal({ sessionID: ctx.sessionID, text: params.text, status: "active" })
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

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "paused" })
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

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "active" })
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
      description: "Mark the current session goal as completed.",
      parameters: EmptyParameters,
      execute: (_params: Schema.Schema.Type<typeof EmptyParameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          yield* ctx.ask({
            permission: "goal_complete",
            patterns: ["*"],
            always: ["*"],
            metadata: {},
          })

          const goal = yield* session.updateGoal({ sessionID: ctx.sessionID, status: "completed" })
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

          const goal = yield* session.getGoal(ctx.sessionID)
          return {
            title: goal ? "Goal status" : "No goal",
            output: formatGoal(goal),
            metadata: { goal: goal ?? null },
          }
        }),
    } satisfies Tool.DefWithoutID<typeof EmptyParameters, Metadata>
  }),
)
