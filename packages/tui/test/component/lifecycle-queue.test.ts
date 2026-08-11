import { describe, expect, test } from "bun:test"
import { lifecyclePromptText, lifecycleQueue } from "../../src/component/prompt/lifecycle-queue"

describe("lifecycleQueue", () => {
  test("enqueue and shift preserve fifo order", () => {
    const sessionID = "session-test"
    lifecycleQueue.enqueue(sessionID, { prompt: { input: "first", parts: [] }, mode: "normal" })
    lifecycleQueue.enqueue(sessionID, { prompt: { input: "second", parts: [] }, mode: "normal" })

    expect(lifecycleQueue.list(sessionID).map((item) => lifecyclePromptText(item.prompt))).toEqual(["first", "second"])

    const next = lifecycleQueue.shift(sessionID)
    expect(next && lifecyclePromptText(next.prompt)).toBe("first")
    expect(lifecycleQueue.list(sessionID).map((item) => lifecyclePromptText(item.prompt))).toEqual(["second"])

    lifecycleQueue.shift(sessionID)
    expect(lifecycleQueue.list(sessionID)).toEqual([])
  })

  test("lifecyclePromptText prefers input over text parts", () => {
    expect(
      lifecyclePromptText({
        input: "hello",
        parts: [{ type: "text", text: "ignored", synthetic: false }],
      }),
    ).toBe("hello")
  })
})
