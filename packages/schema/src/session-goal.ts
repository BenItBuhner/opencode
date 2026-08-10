export * as SessionGoal from "./session-goal"

import { Schema } from "effect"
import { NonNegativeInt, optional } from "./schema"

export const Status = Schema.Literals(["active", "paused", "completed"])
export type Status = typeof Status.Type

export const Progress = Schema.Int.check(Schema.isGreaterThanOrEqualTo(0), Schema.isLessThanOrEqualTo(100))
export type Progress = typeof Progress.Type

export interface Summary extends Schema.Schema.Type<typeof Summary> {}
export const Summary = Schema.Struct({
  id: Schema.String,
  created: NonNegativeInt,
  progress: Progress,
  summary: Schema.String,
  headline: Schema.String.pipe(optional),
  revision: NonNegativeInt.pipe(optional),
}).annotate({ identifier: "Session.Goal.Summary" })

export interface Info extends Schema.Schema.Type<typeof Info> {}
export const Info = Schema.Struct({
  text: Schema.String,
  status: Status,
  created: NonNegativeInt,
  updated: NonNegativeInt,
  completed: NonNegativeInt.pipe(optional),
  progress: Progress.pipe(optional),
  summaries: Schema.Array(Summary).pipe(optional),
  revision: NonNegativeInt.pipe(optional),
}).annotate({ identifier: "Session.Goal.Info" })
