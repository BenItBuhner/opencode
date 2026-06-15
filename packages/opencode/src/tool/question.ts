import { Effect, Schema } from "effect"
import * as Tool from "./tool"
import { Question } from "../question"
import DESCRIPTION from "./question.txt"

export const Parameters = Schema.Struct({
  questions: Schema.mutable(Schema.Array(Question.Prompt)).annotate({ description: "Questions to ask" }),
  timeout: Schema.optional(
    Schema.Number.check(Schema.isGreaterThan(0), Schema.isLessThanOrEqualTo(Question.MAX_TIMEOUT_SECONDS)),
  ).annotate({
    description: `Optional timeout in seconds. Defaults to ${Question.DEFAULT_TIMEOUT_SECONDS} and may not exceed ${Question.MAX_TIMEOUT_SECONDS}.`,
  }),
})

type Metadata = {
  answers: ReadonlyArray<Question.Answer>
}

export const QuestionTool = Tool.define<typeof Parameters, Metadata, Question.Service>(
  "question",
  Effect.gen(function* () {
    const question = yield* Question.Service

    return {
      description: DESCRIPTION,
      parameters: Parameters,
      execute: (params: Schema.Schema.Type<typeof Parameters>, ctx: Tool.Context<Metadata>) =>
        Effect.gen(function* () {
          const answers = yield* question.ask({
            sessionID: ctx.sessionID,
            questions: params.questions,
            tool: ctx.callID ? { messageID: ctx.messageID, callID: ctx.callID } : undefined,
            timeout: params.timeout,
          })

          const formatted = params.questions
            .map((q, i) => `"${q.question}"="${answers[i]?.length ? answers[i].join(", ") : "Unanswered"}"`)
            .join(", ")
          const timedOut = answers.every((answer) => answer.length === 1 && answer[0]?.startsWith("User failed to answer in time"))

          return {
            title: `Asked ${params.questions.length} question${params.questions.length > 1 ? "s" : ""}`,
            output: timedOut
              ? `${answers[0]?.[0] ?? "User failed to answer in time"}. You can now continue without the user's answer.`
              : `User has answered your questions: ${formatted}. You can now continue with the user's answers in mind.`,
            metadata: {
              answers,
            },
          }
        }).pipe(Effect.orDie),
    }
  }),
)
