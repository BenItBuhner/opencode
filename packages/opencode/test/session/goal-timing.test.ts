import { describe, expect, test } from "bun:test"
import { goalActiveSecondsAt, goalActiveTiming, type Goal } from "@/session/session"

const goal = (input: Partial<Goal> & Pick<Goal, "status">): Goal => ({
  text: "ship goal mode",
  created: 1_000,
  updated: 1_000,
  ...input,
})

describe("goal active timing", () => {
  test("starts a new active goal at zero", () => {
    expect(goalActiveTiming(undefined, "active", 5_000, true)).toEqual({
      activeSeconds: 0,
      activeSince: 5_000,
    })
  })

  test("accumulates active time when pausing", () => {
    const existing = goal({
      status: "active",
      activeSeconds: 10,
      activeSince: 4_000,
    })

    expect(goalActiveTiming(existing, "paused", 9_000, false)).toEqual({
      activeSeconds: 15,
      activeSince: undefined,
    })
  })

  test("resumes from accumulated active time", () => {
    const existing = goal({
      status: "paused",
      activeSeconds: 42,
    })

    expect(goalActiveTiming(existing, "active", 20_000, false)).toEqual({
      activeSeconds: 42,
      activeSince: 20_000,
    })
  })

  test("resets timing when the goal text changes", () => {
    const existing = goal({
      status: "paused",
      activeSeconds: 99,
    })

    expect(goalActiveTiming(existing, "active", 30_000, true)).toEqual({
      activeSeconds: 0,
      activeSince: 30_000,
    })
  })

  test("counts only active running time at a timestamp", () => {
    const active = goal({
      status: "active",
      activeSeconds: 30,
      activeSince: 10_000,
    })
    const paused = goal({
      status: "paused",
      activeSeconds: 30,
      activeSince: 10_000,
    })

    expect(goalActiveSecondsAt(active, 25_000)).toBe(45)
    expect(goalActiveSecondsAt(paused, 25_000)).toBe(30)
  })

  test("falls back to created for legacy active goals", () => {
    const legacy = goal({ status: "active", created: 1_000 })

    expect(goalActiveSecondsAt(legacy, 16_000)).toBe(15)
  })
})
