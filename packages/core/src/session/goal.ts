export * as SessionGoal from "./goal"

import { eq } from "drizzle-orm"
import { Context, Effect, Layer, Option, Schema } from "effect"
import { Database } from "../database/database"
import { makeLocationNode } from "../effect/app-node"
import { SessionSchema } from "./schema"
import { SessionTable } from "./sql"

export const Status = Schema.Literals(["active", "paused", "completed"])
export type Status = typeof Status.Type
export const Progress = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0), Schema.isLessThanOrEqualTo(100))
export type Progress = typeof Progress.Type
export const Summary = Schema.Struct({
  id: Schema.String,
  created: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  progress: Progress,
  summary: Schema.String,
  headline: Schema.String.pipe(Schema.optional),
  revision: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)).pipe(Schema.optional),
})
export type Summary = typeof Summary.Type
export const Info = Schema.Struct({
  text: Schema.String,
  status: Status,
  created: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  updated: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)),
  completed: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)).pipe(Schema.optional),
  progress: Progress.pipe(Schema.optional),
  summaries: Schema.Array(Summary).pipe(Schema.optional),
  revision: Schema.Int.check(Schema.isGreaterThanOrEqualTo(0)).pipe(Schema.optional),
})
export type Info = typeof Info.Type

type UpdateInput = {
  sessionID: SessionSchema.ID
  text?: string
  status?: Status
}
type SummaryInput = {
  sessionID: SessionSchema.ID
  progress: Progress
  summary: string
  headline?: string
}
const SUMMARY_LIMIT = 6
const decode = Schema.decodeUnknownOption(Info)

export class NotFoundError extends Schema.TaggedErrorClass<NotFoundError>()("SessionGoal.NotFoundError", {
  sessionID: SessionSchema.ID,
}) {}

export interface Interface {
  readonly get: (sessionID: SessionSchema.ID) => Effect.Effect<Info | undefined, NotFoundError>
  readonly set: (input: {
    sessionID: SessionSchema.ID
    text: string
    status?: Status
  }) => Effect.Effect<Info, NotFoundError>
  readonly update: (input: UpdateInput) => Effect.Effect<Info | undefined, NotFoundError>
  readonly addSummary: (input: SummaryInput) => Effect.Effect<Info | undefined, NotFoundError>
  readonly clear: (sessionID: SessionSchema.ID) => Effect.Effect<void, NotFoundError>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/v2/SessionGoal") {}

const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const { db } = yield* Database.Service

    const row = Effect.fn("SessionGoal.row")(function* (sessionID: SessionSchema.ID) {
      const current = yield* db.select().from(SessionTable).where(eq(SessionTable.id, sessionID)).get().pipe(Effect.orDie)
      if (!current) return yield* new NotFoundError({ sessionID })
      return current
    })
    const write = Effect.fn("SessionGoal.write")(function* (input: { sessionID: SessionSchema.ID; goal?: Info }) {
      const current = yield* row(input.sessionID)
      const metadata = { ...(current.metadata ?? {}) }
      if (input.goal) metadata.goal = input.goal
      else delete metadata.goal
      yield* db
        .update(SessionTable)
        .set({ metadata, time_updated: Date.now() })
        .where(eq(SessionTable.id, input.sessionID))
        .run()
        .pipe(Effect.orDie)
    })
    const get = Effect.fn("SessionGoal.get")(function* (sessionID: SessionSchema.ID) {
      const goal = Option.getOrUndefined(decode((yield* row(sessionID)).metadata?.goal))
      if (!goal) return undefined
      return { ...goal, summaries: goal.summaries?.map((summary) => ({ ...summary })) }
    })
    const set = Effect.fn("SessionGoal.set")(function* (input: {
      sessionID: SessionSchema.ID
      text: string
      status?: Status
    }) {
      const existing = yield* get(input.sessionID)
      const now = Date.now()
      const status = input.status ?? "active"
      const goal: Info = {
        text: input.text.trim(),
        status,
        created: existing?.created ?? now,
        updated: now,
        completed: status === "completed" ? (existing?.completed ?? now) : undefined,
        revision: (existing?.revision ?? 0) + 1,
      }
      yield* write({ sessionID: input.sessionID, goal })
      return goal
    })
    const update = Effect.fn("SessionGoal.update")(function* (input: UpdateInput) {
      const existing = yield* get(input.sessionID)
      if (!existing) {
        if (input.text === undefined) return undefined
        return yield* set({ sessionID: input.sessionID, text: input.text, status: input.status })
      }
      const now = Date.now()
      const status = input.status ?? existing.status
      const goal: Info = {
        ...existing,
        text: input.text === undefined ? existing.text : input.text.trim(),
        status,
        updated: now,
        completed: status === "completed" ? (existing.completed ?? now) : undefined,
        revision: (existing.revision ?? 0) + 1,
      }
      yield* write({ sessionID: input.sessionID, goal })
      return goal
    })
    const addSummary = Effect.fn("SessionGoal.addSummary")(function* (input: SummaryInput) {
      const existing = yield* get(input.sessionID)
      if (!existing) return undefined
      const now = Date.now()
      const revision = (existing.revision ?? 0) + 1
      const summary: Summary = {
        id: `${now}-${revision}`,
        created: now,
        progress: input.progress,
        summary: input.summary.trim(),
        headline: input.headline?.trim() || undefined,
        revision,
      }
      const goal: Info = {
        ...existing,
        progress: input.progress,
        summaries: [...(existing.summaries ?? []), summary].slice(-SUMMARY_LIMIT),
        updated: now,
        revision,
      }
      yield* write({ sessionID: input.sessionID, goal })
      return goal
    })
    const clear = Effect.fn("SessionGoal.clear")(function* (sessionID: SessionSchema.ID) {
      yield* write({ sessionID })
    })

    return Service.of({ get, set, update, addSummary, clear })
  }),
)

export const node = makeLocationNode({ service: Service, layer, deps: [Database.node] })
