import { describe, expect, test } from "bun:test"
import type { SessionStatus } from "@opencode-ai/sdk/v2"
import { createRoot, createSignal } from "solid-js"
import { compactProgressBar, createGoalStatus, parseGoalStatus } from "../../src/component/goal-status"

describe("goal status", () => {
  test("parses valid goal metadata and derives latest summary progress", () => {
    const goal = parseGoalStatus({
      text: "Ship the sidebar goal panel",
      status: "active",
      created: 1_000,
      summaries: [
        { id: "a", created: 2_000, progress: 25, summary: "## Done\n- Initial pass" },
        { id: "b", created: 3_000, progress: 75, headline: "Almost there" },
      ],
    })

    expect(goal).toEqual({
      text: "Ship the sidebar goal panel",
      status: "active",
      created: 1_000,
      progress: 75,
      summaries: [
        { id: "a", created: 2_000, progress: 25, summary: "## Done\n- Initial pass", headline: undefined },
        { id: "b", created: 3_000, progress: 75, summary: undefined, headline: "Almost there" },
      ],
    })
  })

  test("rejects malformed goal metadata", () => {
    expect(parseGoalStatus(undefined)).toBeUndefined()
    expect(parseGoalStatus({ text: "missing status" })).toBeUndefined()
    expect(parseGoalStatus({ status: "active" })).toBeUndefined()
  })

  test("clamps compact progress bars", () => {
    expect(compactProgressBar(-10)).toEqual({ filled: "", empty: "────────────" })
    expect(compactProgressBar(150)).toEqual({ filled: "━━━━━━━━━━━━", empty: "" })
  })

  test("clears a retained goal-agent goal when the session returns to idle", async () => {
    await new Promise<void>((resolve, reject) =>
      createRoot((dispose) => {
        const [goalValue, setGoalValue] = createSignal<unknown>({
          text: "Calculate 8 * 7 and report the result",
          status: "active",
          created: Date.now(),
        })
        const [statusValue, setStatusValue] = createSignal<SessionStatus>({ type: "busy" })
        const goal = createGoalStatus({
          sessionID: () => "ses_goal",
          goal: () => goalValue(),
          messages: () => [
            {
              id: "msg_goal",
              sessionID: "ses_goal",
              role: "user",
              time: { created: 1 },
              agent: "goal",
              model: { providerID: "test", modelID: "test" },
            },
          ],
          status: () => statusValue(),
        })

        Promise.resolve()
          .then(() => {
            expect(goal()?.status).toBe("active")
            setGoalValue(undefined)
            return Promise.resolve()
          })
          .then(() => {
            expect(goal()?.status).toBe("active")
            setStatusValue({ type: "idle" })
            return Promise.resolve()
          })
          .then(() => {
            expect(goal()).toBeUndefined()
            dispose()
            resolve()
          }, reject)
      }),
    )
  })
})
