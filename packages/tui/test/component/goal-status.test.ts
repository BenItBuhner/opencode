import { describe, expect, test } from "bun:test"
import { compactProgressBar, goalActiveSecondsAt, parseGoalStatus } from "../../src/component/goal-status"

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

  test("counts active goal duration without paused wall time", () => {
    expect(
      goalActiveSecondsAt(
        {
          text: "Ship the sidebar goal panel",
          status: "active",
          created: 1_000,
          activeSeconds: 120,
          activeSince: 10_000,
        },
        25_000,
      ),
    ).toBe(135)
  })

  test("parses active timing fields from goal metadata", () => {
    const goal = parseGoalStatus({
      text: "Ship the sidebar goal panel",
      status: "active",
      created: 1_000,
      activeSeconds: 30,
      activeSince: 5_000,
    })

    expect(goal?.activeSeconds).toBe(30)
    expect(goal?.activeSince).toBe(5_000)
  })
})
