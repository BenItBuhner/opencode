import { BusEvent } from "@/bus/bus-event"
import { Bus } from "@/bus"
import { Database } from "@/storage/db"
import { ThreadGoalTable } from "./session.sql"
import { SessionID } from "./schema"
import { serviceUse } from "@opencode-ai/core/effect/service-use"
import { eq } from "drizzle-orm"
import { randomUUID } from "crypto"
import { Context, Effect, Layer, Schema } from "effect"
import { NonNegativeInt, optionalOmitUndefined } from "@opencode-ai/core/schema"

export const MAX_THREAD_GOAL_OBJECTIVE_CHARS = 4_000

export const Status = Schema.Literals([
  "active",
  "paused",
  "blocked",
  "usage_limited",
  "budget_limited",
  "complete",
])
export type Status = typeof Status.Type

export const Info = Schema.Struct({
  threadId: SessionID,
  objective: Schema.String,
  status: Status,
  tokenBudget: optionalOmitUndefined(NonNegativeInt),
  tokensUsed: NonNegativeInt,
  timeUsedSeconds: NonNegativeInt,
  createdAt: NonNegativeInt,
  updatedAt: NonNegativeInt,
})
export type Info = typeof Info.Type

export const SetInput = Schema.Struct({
  sessionID: SessionID,
  objective: Schema.optional(Schema.String),
  status: Schema.optional(Status),
  tokenBudget: Schema.optional(Schema.NullOr(NonNegativeInt)),
})
export const CreateInput = Schema.Struct({
  sessionID: SessionID,
  objective: Schema.String,
  tokenBudget: Schema.optional(NonNegativeInt),
})

export const Event = {
  Updated: BusEvent.define(
    "session.goal.updated",
    Schema.Struct({
      sessionID: SessionID,
      goal: Info,
    }),
  ),
  Cleared: BusEvent.define(
    "session.goal.cleared",
    Schema.Struct({
      sessionID: SessionID,
    }),
  ),
}

export interface Interface {
  readonly get: (sessionID: SessionID) => Effect.Effect<Info | undefined>
  readonly create: (input: typeof CreateInput.Type) => Effect.Effect<Info>
  readonly set: (input: typeof SetInput.Type) => Effect.Effect<Info>
  readonly clear: (sessionID: SessionID) => Effect.Effect<void>
  readonly account: (input: { sessionID: SessionID; tokens?: number; seconds?: number }) => Effect.Effect<Info | undefined>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/SessionGoal") {}

export const use = serviceUse(Service)

export const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const bus = yield* Bus.Service

    const get = Effect.fn("SessionGoal.get")(function* (sessionID: SessionID) {
      return rowToGoal(
        Database.use((db) => db.select().from(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, sessionID)).get()),
      )
    })

    const publish = Effect.fn("SessionGoal.publish")(function* (goal: Info) {
      yield* bus.publish(Event.Updated, { sessionID: goal.threadId, goal })
      return goal
    })

    const create = Effect.fn("SessionGoal.create")(function* (input: typeof CreateInput.Type) {
      const objective = validateObjective(input.objective)
      const existing = yield* get(input.sessionID)
      if (existing) throw new Error("cannot create a new goal because this thread already has a goal")
      const now = Date.now()
      const goal = rowToGoal(
        Database.use((db) =>
          db
            .insert(ThreadGoalTable)
            .values({
              session_id: input.sessionID,
              goal_id: randomUUID(),
              objective,
              status: statusAfterBudget("active", 0, input.tokenBudget),
              token_budget: input.tokenBudget,
              tokens_used: 0,
              time_used_seconds: 0,
              created_at_ms: now,
              updated_at_ms: now,
            })
            .returning()
            .get(),
        ),
      )
      if (!goal) throw new Error("failed to create goal")
      return yield* publish(goal)
    })

    const set = Effect.fn("SessionGoal.set")(function* (input: typeof SetInput.Type) {
      const existing = yield* get(input.sessionID)
      if (!existing && !input.objective) throw new Error("thread has no goal")
      if (input.objective) {
        const now = Date.now()
        const tokenBudget = input.tokenBudget === null ? undefined : (input.tokenBudget ?? existing?.tokenBudget)
        const goal = rowToGoal(
          Database.use((db) =>
            db
              .insert(ThreadGoalTable)
              .values({
                session_id: input.sessionID,
                goal_id: randomUUID(),
                objective: validateObjective(input.objective!),
                status: statusAfterBudget(input.status ?? "active", 0, tokenBudget),
                token_budget: tokenBudget,
                tokens_used: 0,
                time_used_seconds: 0,
                created_at_ms: now,
                updated_at_ms: now,
              })
              .onConflictDoUpdate({
                target: ThreadGoalTable.session_id,
                set: {
                  goal_id: randomUUID(),
                  objective: validateObjective(input.objective!),
                  status: statusAfterBudget(input.status ?? "active", 0, tokenBudget),
                  token_budget: tokenBudget,
                  tokens_used: 0,
                  time_used_seconds: 0,
                  created_at_ms: now,
                  updated_at_ms: now,
                },
              })
              .returning()
              .get(),
          ),
        )
        if (!goal) throw new Error("failed to set goal")
        return yield* publish(goal)
      }

      const nextBudget = input.tokenBudget === undefined ? existing!.tokenBudget : (input.tokenBudget ?? undefined)
      const nextStatus = statusAfterBudget(input.status ?? existing!.status, existing!.tokensUsed, nextBudget)
      const now = Date.now()
      const timeUsedSeconds =
        existing!.status === "active" && input.status !== undefined && input.status !== "active"
          ? existing!.timeUsedSeconds + Math.max(0, Math.floor((now - existing!.updatedAt) / 1000))
          : existing!.timeUsedSeconds
      const goal = rowToGoal(
        Database.use((db) =>
          db
            .update(ThreadGoalTable)
            .set({
              status: existing!.status === "budget_limited" && ["paused", "blocked"].includes(nextStatus)
                ? "budget_limited"
                : nextStatus,
              token_budget: nextBudget,
              time_used_seconds: timeUsedSeconds,
              updated_at_ms: now,
            })
            .where(eq(ThreadGoalTable.session_id, input.sessionID))
            .returning()
            .get(),
        ),
      )
      if (!goal) throw new Error("thread has no goal")
      return yield* publish(goal)
    })

    const clear = Effect.fn("SessionGoal.clear")(function* (sessionID: SessionID) {
      Database.use((db) => db.delete(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, sessionID)).run())
      yield* bus.publish(Event.Cleared, { sessionID })
    })

    const account = Effect.fn("SessionGoal.account")(function* (input: {
      sessionID: SessionID
      tokens?: number
      seconds?: number
    }) {
      const existing = yield* get(input.sessionID)
      if (!existing || existing.status !== "active") return existing
      const tokensUsed = existing.tokensUsed + Math.max(0, Math.floor(input.tokens ?? 0))
      const timeUsedSeconds = existing.timeUsedSeconds + Math.max(0, Math.floor(input.seconds ?? 0))
      const goal = rowToGoal(
        Database.use((db) =>
          db
            .update(ThreadGoalTable)
            .set({
              tokens_used: tokensUsed,
              time_used_seconds: timeUsedSeconds,
              status: statusAfterBudget(existing.status, tokensUsed, existing.tokenBudget),
              updated_at_ms: Date.now(),
            })
            .where(eq(ThreadGoalTable.session_id, input.sessionID))
            .returning()
            .get(),
        ),
      )
      if (!goal) return undefined
      return yield* publish(goal)
    })

    return Service.of({ get, create, set, clear, account })
  }),
)

export const defaultLayer: Layer.Layer<Service> = layer.pipe(Layer.provide(Bus.layer))

export const getDirect = (sessionID: SessionID) =>
  Effect.sync(() =>
    rowToGoal(Database.use((db) => db.select().from(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, sessionID)).get())),
  )

export const createDirect = (input: typeof CreateInput.Type) =>
  Effect.sync(() => {
    const objective = validateObjective(input.objective)
    const existing = rowToGoal(
      Database.use((db) => db.select().from(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, input.sessionID)).get()),
    )
    if (existing) throw new Error("cannot create a new goal because this thread already has a goal")
    const now = Date.now()
    return rowToGoal(
      Database.use((db) =>
        db
          .insert(ThreadGoalTable)
          .values({
            session_id: input.sessionID,
            goal_id: randomUUID(),
            objective,
            status: statusAfterBudget("active", 0, input.tokenBudget),
            token_budget: input.tokenBudget,
            tokens_used: 0,
            time_used_seconds: 0,
            created_at_ms: now,
            updated_at_ms: now,
          })
          .returning()
          .get(),
      ),
    )!
  })

export const setDirect = (input: typeof SetInput.Type) =>
  Effect.sync(() => {
    const existing = rowToGoal(
      Database.use((db) => db.select().from(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, input.sessionID)).get()),
    )
    if (!existing && !input.objective) throw new Error("thread has no goal")
    if (input.objective) {
      const now = Date.now()
      const tokenBudget = input.tokenBudget === null ? undefined : (input.tokenBudget ?? existing?.tokenBudget)
      return rowToGoal(
        Database.use((db) =>
          db
            .insert(ThreadGoalTable)
            .values({
              session_id: input.sessionID,
              goal_id: randomUUID(),
              objective: validateObjective(input.objective!),
              status: statusAfterBudget(input.status ?? "active", 0, tokenBudget),
              token_budget: tokenBudget,
              tokens_used: 0,
              time_used_seconds: 0,
              created_at_ms: now,
              updated_at_ms: now,
            })
            .onConflictDoUpdate({
              target: ThreadGoalTable.session_id,
              set: {
                goal_id: randomUUID(),
                objective: validateObjective(input.objective!),
                status: statusAfterBudget(input.status ?? "active", 0, tokenBudget),
                token_budget: tokenBudget,
                tokens_used: 0,
                time_used_seconds: 0,
                created_at_ms: now,
                updated_at_ms: now,
              },
            })
            .returning()
            .get(),
        ),
      )!
    }

    const nextBudget = input.tokenBudget === undefined ? existing!.tokenBudget : (input.tokenBudget ?? undefined)
    const nextStatus = statusAfterBudget(input.status ?? existing!.status, existing!.tokensUsed, nextBudget)
    const now = Date.now()
    const timeUsedSeconds =
      existing!.status === "active" && input.status !== undefined && input.status !== "active"
        ? existing!.timeUsedSeconds + Math.max(0, Math.floor((now - existing!.updatedAt) / 1000))
        : existing!.timeUsedSeconds
    return rowToGoal(
      Database.use((db) =>
        db
          .update(ThreadGoalTable)
          .set({
            status:
              existing!.status === "budget_limited" && ["paused", "blocked"].includes(nextStatus)
                ? "budget_limited"
                : nextStatus,
            token_budget: nextBudget,
            time_used_seconds: timeUsedSeconds,
            updated_at_ms: now,
          })
          .where(eq(ThreadGoalTable.session_id, input.sessionID))
          .returning()
          .get(),
      ),
    )!
  })

export const clearDirect = (sessionID: SessionID) =>
  Effect.sync(() => {
    Database.use((db) => db.delete(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, sessionID)).run())
  })

export const accountDirect = (input: { sessionID: SessionID; tokens?: number; seconds?: number }) =>
  Effect.sync(() => {
    const existing = rowToGoal(
      Database.use((db) => db.select().from(ThreadGoalTable).where(eq(ThreadGoalTable.session_id, input.sessionID)).get()),
    )
    if (!existing || existing.status !== "active") return existing
    const tokensUsed = existing.tokensUsed + Math.max(0, Math.floor(input.tokens ?? 0))
    const timeUsedSeconds = existing.timeUsedSeconds + Math.max(0, Math.floor(input.seconds ?? 0))
    return rowToGoal(
      Database.use((db) =>
        db
          .update(ThreadGoalTable)
          .set({
            tokens_used: tokensUsed,
            time_used_seconds: timeUsedSeconds,
            status: statusAfterBudget(existing.status, tokensUsed, existing.tokenBudget),
            updated_at_ms: Date.now(),
          })
          .where(eq(ThreadGoalTable.session_id, input.sessionID))
          .returning()
          .get(),
      ),
    )
  })

function rowToGoal(row: (typeof ThreadGoalTable.$inferSelect) | undefined): Info | undefined {
  if (!row) return undefined
  return {
    threadId: row.session_id,
    objective: row.objective,
    status: row.status,
    tokenBudget: row.token_budget ?? undefined,
    tokensUsed: row.tokens_used,
    timeUsedSeconds: row.time_used_seconds,
    createdAt: row.created_at_ms,
    updatedAt: row.updated_at_ms,
  }
}

function validateObjective(value: string) {
  const objective = value.trim()
  if (!objective) throw new Error("goal objective must not be empty")
  if ([...objective].length > MAX_THREAD_GOAL_OBJECTIVE_CHARS) {
    throw new Error(`goal objective must be at most ${MAX_THREAD_GOAL_OBJECTIVE_CHARS} characters`)
  }
  return objective
}

function statusAfterBudget(status: Status, tokensUsed: number, tokenBudget: number | undefined): Status {
  if (status === "active" && tokenBudget !== undefined && tokensUsed >= tokenBudget) return "budget_limited"
  return status
}

export function remainingTokens(goal: Info | undefined) {
  if (!goal?.tokenBudget) return undefined
  return Math.max(0, goal.tokenBudget - goal.tokensUsed)
}

export * as SessionGoal from "./goal"
