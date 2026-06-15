export * as QuestionTool from "./question"

import { ToolFailure } from "@opencode-ai/llm"
import { Effect, Layer, Schema } from "effect"
import { PermissionV2 } from "../permission"
import { QuestionV2 } from "../question"
import { PositiveInt } from "../schema"
import { Tool } from "./tool"
import { Tools } from "./tools"

export const name = "question"

export const description = `Use this tool when you need to ask the user questions during execution. This allows you to:
1. Gather user preferences or requirements
2. Clarify ambiguous instructions
3. Get decisions on implementation choices as you work
4. Offer choices to the user about what direction to take.

Usage notes:
- When \`custom\` is enabled (default), a "Type your own answer" option is added automatically; don't include "Other" or catch-all options
- Answers are returned as arrays of labels; set \`multiple: true\` to allow selecting more than one
- You can specify an optional \`timeout\` in seconds. If not specified, questions time out after 300 seconds. If the user does not answer in time, the answer is returned as "User failed to answer in time (Ns)" so you can continue without blocking.
- If you recommend a specific option, make that the first option in the list and add "(Recommended)" at the end of the label`

export const Input = Schema.Struct({
  questions: Schema.Array(QuestionV2.Prompt).annotate({ description: "Questions to ask" }),
  timeout: PositiveInt.check(Schema.isLessThanOrEqualTo(QuestionV2.MAX_TIMEOUT_SECONDS))
    .pipe(Schema.optional)
    .annotate({
      description: `Optional timeout in seconds. Defaults to ${QuestionV2.DEFAULT_TIMEOUT_SECONDS} and may not exceed ${QuestionV2.MAX_TIMEOUT_SECONDS}.`,
    }),
})

export const Output = Schema.Struct({
  answers: Schema.Array(QuestionV2.Answer),
})
export type Output = typeof Output.Type

export const toModelOutput = (
  questions: ReadonlyArray<QuestionV2.Prompt>,
  answers: ReadonlyArray<QuestionV2.Answer>,
) => {
  const timedOut = answers.every((answer) => answer.length === 1 && answer[0]?.startsWith("User failed to answer in time"))
  if (timedOut) {
    return `${answers[0]?.[0] ?? "User failed to answer in time"}. You can now continue without the user's answer.`
  }
  const formatted = questions
    .map(
      (question, index) =>
        `"${question.question}"="${answers[index]?.length ? answers[index].join(", ") : "Unanswered"}"`,
    )
    .join(", ")
  return `User has answered your questions: ${formatted}. You can now continue with the user's answers in mind.`
}

export const layer = Layer.effectDiscard(
  Effect.gen(function* () {
    const tools = yield* Tools.Service
    const question = yield* QuestionV2.Service
    const permission = yield* PermissionV2.Service

    yield* tools
      .register({
        [name]: Tool.make({
          description,
          input: Input,
          output: Output,
          toModelOutput: ({ input, output }) => [
            { type: "text", text: toModelOutput(input.questions, output.answers) },
          ],
          execute: (input, context) =>
            permission
              .assert({
                action: "question",
                resources: ["*"],
                sessionID: context.sessionID,
                agent: context.agent,
                source: { type: "tool", messageID: context.assistantMessageID, callID: context.toolCallID },
              })
              .pipe(
                Effect.mapError(() => new ToolFailure({ message: "Permission denied: question" })),
                Effect.andThen(
                  question
                    .ask({
                      sessionID: context.sessionID,
                      questions: input.questions,
                      tool: { messageID: context.assistantMessageID, callID: context.toolCallID },
                      timeout: input.timeout,
                    })
                    .pipe(Effect.orDie),
                ),
                Effect.map((answers) => ({ answers })),
              ),
        }),
      })
      .pipe(Effect.orDie)
  }),
)
