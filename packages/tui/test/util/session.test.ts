import { describe, expect, test } from "bun:test"
import { BTW_METADATA, btwSessions, createBtwTitle, isBtwSession, isDefaultTitle, parseBtwPrompt } from "../../src/util/session"

describe("util.session", () => {
  test("recognizes generated parent and child titles", () => {
    expect(isDefaultTitle("New session - 2026-06-06T12:34:56.789Z")).toBeTrue()
    expect(isDefaultTitle("Child session - 2026-06-06T12:34:56.789Z")).toBeTrue()
    expect(isDefaultTitle("New session - custom")).toBeFalse()
  })

  test("finds BTW child sessions by parent and recency", () => {
    const title = createBtwTitle(new Date("2026-06-06T12:34:56.789Z"))
    const sessions = [
      { id: "older", parentID: "parent", title, time: { updated: 1 } },
      {
        id: "newer",
        parentID: "parent",
        title: "Quick question",
        metadata: BTW_METADATA,
        time: { updated: 3 },
      },
      { id: "subagent", parentID: "parent", title: "@build subagent", time: { updated: 4 } },
      { id: "other-parent", parentID: "other", title, time: { updated: 5 } },
      { id: "root", title, time: { updated: 6 } },
    ]

    expect(isBtwSession(sessions[0])).toBeTrue()
    expect(isBtwSession(sessions[4])).toBeFalse()
    expect(btwSessions("parent", sessions).map((session) => session.id)).toEqual(["newer", "older"])
  })

  test("creates readable BTW titles from prompts", () => {
    expect(createBtwTitle("  what is this?\nreally  ")).toBe("BTW - what is this? really")
    expect(createBtwTitle("x".repeat(70))).toBe(`BTW - ${"x".repeat(61)}...`)
  })

  test("parses BTW prompt arguments", () => {
    expect(parseBtwPrompt("/btw what is running?")).toBe("what is running?")
    expect(parseBtwPrompt("/btw\nmulti\nline")).toBe("multi\nline")
    expect(parseBtwPrompt("/btw-resume")).toBeUndefined()
    expect(parseBtwPrompt("/btwice")).toBeUndefined()
  })
})
