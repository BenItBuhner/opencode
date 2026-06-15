import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { Deferred, Duration, Effect, Layer, Schema, Context } from "effect"
import { InstanceState } from "@/effect/instance-state"
import { SessionID, MessageID } from "@/session/schema"
import { QuestionID } from "./schema"
import { EventV2Bridge } from "@/event-v2-bridge"
import { EventV2 } from "@opencode-ai/core/event"

// Schemas — these are pure data; nothing checks class identity (see PR
// description) so they're plain `Schema.Struct` + type alias. That lets
// `Question.ask` and other internal sites trust the type contract without a
// re-decode to coerce nested class instances.

export const Option = Schema.Struct({
  label: Schema.String.annotate({
    description: "Display text (1-5 words, concise)",
  }),
  description: Schema.String.annotate({
    description: "Explanation of choice",
  }),
}).annotate({ identifier: "QuestionOption" })
export type Option = Schema.Schema.Type<typeof Option>

const base = {
  question: Schema.String.annotate({
    description: "Complete question",
  }),
  header: Schema.String.annotate({
    description: "Very short label (max 30 chars)",
  }),
  options: Schema.Array(Option).annotate({
    description: "Available choices",
  }),
  multiple: Schema.optional(Schema.Boolean).annotate({
    description: "Allow selecting multiple choices",
  }),
}

export const Info = Schema.Struct({
  ...base,
  custom: Schema.optional(Schema.Boolean).annotate({
    description: "Allow typing a custom answer (default: true)",
  }),
}).annotate({ identifier: "QuestionInfo" })
export type Info = Schema.Schema.Type<typeof Info>

export const Prompt = Schema.Struct(base).annotate({ identifier: "QuestionPrompt" })
export type Prompt = Schema.Schema.Type<typeof Prompt>

export const Tool = Schema.Struct({
  messageID: MessageID,
  callID: Schema.String,
}).annotate({ identifier: "QuestionTool" })
export type Tool = Schema.Schema.Type<typeof Tool>

export const Request = Schema.Struct({
  id: QuestionID,
  sessionID: SessionID,
  questions: Schema.Array(Info).annotate({
    description: "Questions to ask",
  }),
  tool: Schema.optional(Tool),
  timeout: Schema.optional(Schema.Number).annotate({
    description: "Timeout in seconds",
  }),
}).annotate({ identifier: "QuestionRequest" })
export type Request = Schema.Schema.Type<typeof Request>

export const Answer = Schema.Array(Schema.String).annotate({ identifier: "QuestionAnswer" })
export type Answer = Schema.Schema.Type<typeof Answer>

export const Reply = Schema.Struct({
  answers: Schema.Array(Answer).annotate({
    description: "User answers in order of questions (each answer is an array of selected labels)",
  }),
}).annotate({ identifier: "QuestionReply" })
export type Reply = Schema.Schema.Type<typeof Reply>

export const Replied = Schema.Struct({
  sessionID: SessionID,
  requestID: QuestionID,
  answers: Schema.Array(Answer),
}).annotate({ identifier: "QuestionReplied" })

export const Rejected = Schema.Struct({
  sessionID: SessionID,
  requestID: QuestionID,
}).annotate({ identifier: "QuestionRejected" })

export const Event = {
  Asked: EventV2.define({ type: "question.asked", schema: Request.fields }),
  Replied: EventV2.define({ type: "question.replied", schema: Replied.fields }),
  Rejected: EventV2.define({ type: "question.rejected", schema: Rejected.fields }),
}

export const DEFAULT_TIMEOUT_SECONDS = 5 * 60
export const MAX_TIMEOUT_SECONDS = 10 * 60

export function timeoutMessage(timeout: number) {
  return `User failed to answer in time (${timeout}s)`
}

export class RejectedError extends Schema.TaggedErrorClass<RejectedError>()("QuestionRejectedError", {}) {
  override get message() {
    return "The user dismissed this question"
  }
}

export class NotFoundError extends Schema.TaggedErrorClass<NotFoundError>()("Question.NotFoundError", {
  requestID: QuestionID,
}) {}

interface PendingEntry {
  info: Request
  deferred: Deferred.Deferred<ReadonlyArray<Answer>, RejectedError>
}

interface State {
  pending: Map<QuestionID, PendingEntry>
}

// Service

export interface Interface {
  readonly ask: (input: {
    sessionID: SessionID
    questions: ReadonlyArray<Info>
    tool?: Tool
    timeout?: number
  }) => Effect.Effect<ReadonlyArray<Answer>, RejectedError>
  readonly reply: (input: {
    requestID: QuestionID
    answers: ReadonlyArray<Answer>
  }) => Effect.Effect<void, NotFoundError>
  readonly reject: (requestID: QuestionID) => Effect.Effect<void, NotFoundError>
  readonly list: () => Effect.Effect<ReadonlyArray<Request>>
}

export class Service extends Context.Service<Service, Interface>()("@opencode/Question") {}

export const layer = Layer.effect(
  Service,
  Effect.gen(function* () {
    const events = yield* EventV2Bridge.Service
    const state = yield* InstanceState.make<State>(
      Effect.fn("Question.state")(function* () {
        const state = {
          pending: new Map<QuestionID, PendingEntry>(),
        }

        yield* Effect.addFinalizer(() =>
          Effect.gen(function* () {
            for (const item of state.pending.values()) {
              yield* Deferred.fail(item.deferred, new RejectedError())
            }
            state.pending.clear()
          }),
        )

        return state
      }),
    )

    const ask = Effect.fn("Question.ask")(function* (input: {
      sessionID: SessionID
      questions: ReadonlyArray<Info>
      tool?: Tool
      timeout?: number
    }) {
      const pending = (yield* InstanceState.get(state)).pending
      const id = QuestionID.ascending()
      const timeout = Math.min(MAX_TIMEOUT_SECONDS, Math.max(1, Math.round(input.timeout ?? DEFAULT_TIMEOUT_SECONDS)))
      yield* Effect.logInfo("asking", { id, questions: input.questions.length, timeout })

      const deferred = yield* Deferred.make<ReadonlyArray<Answer>, RejectedError>()
      const info: Request = {
        id,
        sessionID: input.sessionID,
        questions: input.questions,
        tool: input.tool,
        timeout,
      }
      pending.set(id, { info, deferred })
      yield* events.publish(Event.Asked, info)

      return yield* Deferred.await(deferred).pipe(
        Effect.timeoutOrElse({
          duration: Duration.seconds(timeout),
          orElse: () =>
            Effect.gen(function* () {
              const answers = input.questions.map(() => [timeoutMessage(timeout)])
              yield* events.publish(Event.Replied, {
                sessionID: info.sessionID,
                requestID: info.id,
                answers,
              })
              yield* Deferred.succeed(deferred, answers)
              return answers
            }),
        }),
        Effect.ensuring(
          Effect.sync(() => {
            pending.delete(id)
          }),
        ),
      )
    })

    const reply = Effect.fn("Question.reply")(function* (input: {
      requestID: QuestionID
      answers: ReadonlyArray<Answer>
    }) {
      const pending = (yield* InstanceState.get(state)).pending
      const existing = pending.get(input.requestID)
      if (!existing) {
        yield* Effect.logWarning("reply for unknown request", { requestID: input.requestID })
        return yield* new NotFoundError({ requestID: input.requestID })
      }
      pending.delete(input.requestID)
      yield* Effect.logInfo("replied", { requestID: input.requestID, answers: input.answers })
      yield* events.publish(Event.Replied, {
        sessionID: existing.info.sessionID,
        requestID: existing.info.id,
        answers: input.answers.map((a) => [...a]),
      })
      yield* Deferred.succeed(existing.deferred, input.answers)
    })

    const reject = Effect.fn("Question.reject")(function* (requestID: QuestionID) {
      const pending = (yield* InstanceState.get(state)).pending
      const existing = pending.get(requestID)
      if (!existing) {
        yield* Effect.logWarning("reject for unknown request", { requestID })
        return yield* new NotFoundError({ requestID })
      }
      pending.delete(requestID)
      yield* Effect.logInfo("rejected", { requestID })
      yield* events.publish(Event.Rejected, {
        sessionID: existing.info.sessionID,
        requestID: existing.info.id,
      })
      yield* Deferred.fail(existing.deferred, new RejectedError())
    })

    const list = Effect.fn("Question.list")(function* () {
      const pending = (yield* InstanceState.get(state)).pending
      return Array.from(pending.values(), (x) => x.info)
    })

    return Service.of({ ask, reply, reject, list })
  }),
)

export const defaultLayer = layer.pipe(Layer.provide(EventV2Bridge.defaultLayer))

export const node = LayerNode.make(layer, [EventV2Bridge.node])

export * as Question from "."
