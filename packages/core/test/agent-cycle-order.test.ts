import { describe, expect, it } from "bun:test"
import { orderPrimaryAgents } from "@opencode-ai/core/agent/cycle-order"

describe("agent cycle order", () => {
  it("orders built-in primary agents as build, plan, goal", () => {
    const agents = [
      { name: "goal" },
      { name: "build" },
      { name: "custom" },
      { name: "plan" },
    ]

    expect(orderPrimaryAgents(agents).map((agent) => agent.name)).toEqual(["build", "plan", "goal", "custom"])
  })
})
