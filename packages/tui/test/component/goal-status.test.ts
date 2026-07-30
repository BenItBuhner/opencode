import { describe, expect, test } from "bun:test"
import { compactProgressBar, parseGoalStatus } from "../../src/component/goal-status"

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
})
