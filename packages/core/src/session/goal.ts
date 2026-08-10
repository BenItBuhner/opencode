export * as SessionGoal from "./goal"

import { eq } from "drizzle-orm"
import { Context, DateTime, Effect, Layer, Option, Schema } from "effect"
import { SessionGoal } from "@opencode-ai/schema/session-goal"
import { Database } from "../database/database"
import { EventV2 } from "../event"
import { makeLocationNode } from "../effect/app-node"
import { KeyedMutex } from "../effect/keyed-mutex"
import { SessionEvent } from "./event"
import { SessionProjector } from "./projector"
import { SessionSchema } from "./schema"
import { SessionTable } from "./sql"

export const Status = SessionGoal.Status
export type Status = SessionGoal.Status
export const Progress = SessionGoal.Progress
export type Progress = SessionGoal.Progress
export const Summary = SessionGoal.Summary
export type Summary = SessionGoal.Summary
export const Info = SessionGoal.Info
export type Info = SessionGoal.Info

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
    const events = yield* EventV2.Service
    const mutex = KeyedMutex.makeUnsafe<SessionSchema.ID>()

    const row = Effect.fn("SessionGoal.row")(function* (sessionID: SessionSchema.ID) {
      const current = yield* db
        .select()
        .from(SessionTable)
        .where(eq(SessionTable.id, sessionID))
        .get()
        .pipe(Effect.orDie)
      if (!current) return yield* new NotFoundError({ sessionID })
      return current
    })
    const write = Effect.fn("SessionGoal.write")(function* (input: { sessionID: SessionSchema.ID; goal?: Info }) {
      yield* row(input.sessionID)
      yield* events.publish(SessionEvent.GoalUpdated, {
        sessionID: input.sessionID,
        timestamp: yield* DateTime.now,
        goal: input.goal,
      })
    })
    const get = Effect.fn("SessionGoal.get")(function* (sessionID: SessionSchema.ID) {
      const goal = Option.getOrUndefined(decode((yield* row(sessionID)).metadata?.goal))
      if (!goal) return undefined
      return { ...goal, summaries: goal.summaries?.map((summary) => ({ ...summary })) }
    })
    const setUnlocked = Effect.fnUntraced(function* (input: {
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
    const set = Effect.fn("SessionGoal.set")((input: Parameters<typeof setUnlocked>[0]) =>
      mutex.withLock(input.sessionID)(setUnlocked(input)),
    )
    const updateUnlocked = Effect.fnUntraced(function* (input: UpdateInput) {
      const existing = yield* get(input.sessionID)
      if (!existing) {
        if (input.text === undefined) return undefined
        return yield* setUnlocked({ sessionID: input.sessionID, text: input.text, status: input.status })
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
    const update = Effect.fn("SessionGoal.update")((input: UpdateInput) =>
      mutex.withLock(input.sessionID)(updateUnlocked(input)),
    )
    const addSummaryUnlocked = Effect.fnUntraced(function* (input: SummaryInput) {
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
    const addSummary = Effect.fn("SessionGoal.addSummary")((input: SummaryInput) =>
      mutex.withLock(input.sessionID)(addSummaryUnlocked(input)),
    )
    const clear = Effect.fn("SessionGoal.clear")((sessionID: SessionSchema.ID) =>
      mutex.withLock(sessionID)(write({ sessionID })),
    )

    return Service.of({ get, set, update, addSummary, clear })
  }),
)

export const node = makeLocationNode({
  service: Service,
  layer,
  deps: [Database.node, EventV2.node, SessionProjector.node],
})
