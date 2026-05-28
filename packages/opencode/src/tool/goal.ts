import { SessionGoal } from "@/session/goal"
import { Effect, Schema } from "effect"
import { NonNegativeInt } from "@opencode-ai/core/schema"
import * as Tool from "./tool"
import { Bus } from "@/bus"

const Empty = Schema.Struct({})
const Create = Schema.Struct({
  objective: Schema.String.annotate({
    description:
      "Required. The concrete objective to start pursuing. This starts a new active goal only when no goal is currently defined; if a goal already exists, this tool fails.",
  }),
  token_budget: Schema.optional(
    NonNegativeInt.annotate({
      description: "Optional positive token budget for the new active goal.",
    }),
  ),
})
const Update = Schema.Struct({
  status: Schema.Literals(["complete", "blocked"]).annotate({
    description:
      "Required. Set to `complete` only when the objective is achieved and no required work remains. Set to `blocked` only after the same blocking condition has recurred for at least three consecutive goal turns and the agent is at an impasse.",
  }),
})

export const GetGoalTool = Tool.define<typeof Empty, {}, never>(
  "get_goal",
  Effect.gen(function* () {
    return {
      description:
        "Get the current goal for this thread, including status, budgets, token and elapsed-time usage, and remaining token budget.",
      parameters: Empty,
      execute: (_args, ctx) =>
        Effect.gen(function* () {
          return goalOutput(yield* SessionGoal.getDirect(ctx.sessionID), false)
        }),
    } satisfies Tool.DefWithoutID<typeof Empty, {}>
  }),
)

export const CreateGoalTool = Tool.define<typeof Create, {}, Bus.Service>(
  "create_goal",
  Effect.gen(function* () {
    const bus = yield* Bus.Service
    return {
      description:
        "Create a goal only when explicitly requested by the user or system/developer instructions; do not infer goals from ordinary tasks.\nSet token_budget only when an explicit token budget is requested. Fails if a goal exists; use update_goal only for status.",
      parameters: Create,
      execute: (args: Schema.Schema.Type<typeof Create>, ctx) =>
        Effect.gen(function* () {
          const goal = yield* SessionGoal.createDirect({
            sessionID: ctx.sessionID,
            objective: args.objective,
            tokenBudget: args.token_budget,
          })
          yield* bus.publish(SessionGoal.Event.Updated, { sessionID: ctx.sessionID, goal })
          return goalOutput(goal, false)
        }),
    } satisfies Tool.DefWithoutID<typeof Create, {}>
  }),
)

export const UpdateGoalTool = Tool.define<typeof Update, {}, Bus.Service>(
  "update_goal",
  Effect.gen(function* () {
    const bus = yield* Bus.Service
    return {
      description: `Update the existing goal.
Use this tool only to mark the goal achieved or genuinely blocked.
Set status to \`complete\` only when the objective has actually been achieved and no required work remains.
Set status to \`blocked\` only when the same blocking condition has repeated for at least three consecutive goal turns, counting the original/user-triggered turn and any automatic continuations, and the agent cannot make meaningful progress without user input or an external-state change.
If the user resumes a goal that was previously marked \`blocked\`, treat the resumed run as a fresh blocked audit. If the same blocking condition then repeats for at least three consecutive resumed goal turns, set status to \`blocked\` again.
Once the blocked threshold is satisfied, do not keep reporting that you are still blocked while leaving the goal active; set status to \`blocked\`.
Do not use \`blocked\` merely because the work is hard, slow, uncertain, incomplete, or would benefit from clarification.
Do not mark a goal complete merely because its budget is nearly exhausted or because you are stopping work.
You cannot use this tool to pause, resume, budget-limit, or usage-limit a goal; those status changes are controlled by the user or system.
When marking a budgeted goal achieved with status \`complete\`, report the final token usage from the tool result to the user.`,
      parameters: Update,
      execute: (args: Schema.Schema.Type<typeof Update>, ctx) =>
        Effect.gen(function* () {
          const goal = yield* SessionGoal.setDirect({
            sessionID: ctx.sessionID,
            status: args.status,
          })
          yield* bus.publish(SessionGoal.Event.Updated, { sessionID: ctx.sessionID, goal })
          return goalOutput(goal, args.status === "complete")
        }),
    } satisfies Tool.DefWithoutID<typeof Update, {}>
  }),
)

function goalOutput(goal: SessionGoal.Info | undefined, includeCompletionBudgetReport: boolean) {
  return {
    title: goal ? `Goal ${goal.status}` : "No goal",
    metadata: {},
    output: JSON.stringify(
      {
        goal,
        remainingTokens: SessionGoal.remainingTokens(goal),
        completionBudgetReport:
          includeCompletionBudgetReport && goal?.status === "complete" && (goal.tokenBudget || goal.timeUsedSeconds > 0)
            ? "Goal achieved. Report final usage from this tool result's structured goal fields. If `goal.tokenBudget` is present, include token usage from `goal.tokensUsed` and `goal.tokenBudget`. If `goal.timeUsedSeconds` is greater than 0, summarize elapsed time in a concise, human-friendly form appropriate to the response language."
            : undefined,
      },
      null,
      2,
    ),
  }
}
