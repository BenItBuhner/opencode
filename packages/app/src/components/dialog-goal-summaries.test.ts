import { describe, expect, test } from "bun:test"
import { compactGoalProgressBar, goalFromSessionMetadata } from "./dialog-goal-summaries"

describe("goalFromSessionMetadata", () => {
  test("derives progress from the latest summary", () => {
    expect(
      goalFromSessionMetadata({
        goal: {
          text: "Restore OpenGoal",
          status: "active",
          summaries: [
            { id: "first", progress: 25 },
            { id: "latest", progress: 75, headline: "Branding restored" },
          ],
        },
      }),
    ).toEqual({
      text: "Restore OpenGoal",
      status: "active",
      created: undefined,
      progress: 75,
      summaries: [
        { id: "first", created: undefined, progress: 25, summary: undefined, headline: undefined },
        {
          id: "latest",
          created: undefined,
          progress: 75,
          summary: undefined,
          headline: "Branding restored",
        },
      ],
    })
  })

  test("rejects malformed goal metadata", () => {
    expect(goalFromSessionMetadata(undefined)).toBeUndefined()
    expect(goalFromSessionMetadata({ goal: { text: "missing status" } })).toBeUndefined()
  })

  test("formats compact progress consistently", () => {
    expect(compactGoalProgressBar(-1)).toBe("-----")
    expect(compactGoalProgressBar(50)).toBe("###--")
    expect(compactGoalProgressBar(100)).toBe("#####")
  })
})
