import { describe, expect } from "bun:test"
import { Effect, Layer } from "effect"
import { Database } from "@opencode-ai/core/database/database"
import { AppNodeBuilder } from "@opencode-ai/core/effect/app-node-builder"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { PermissionV2 } from "@opencode-ai/core/permission"
import { Project } from "@opencode-ai/core/project"
import { ProjectTable } from "@opencode-ai/core/project/sql"
import { AbsolutePath } from "@opencode-ai/core/schema"
import { SessionV2 } from "@opencode-ai/core/session"
import { SessionGoal } from "@opencode-ai/core/session/goal"
import { SessionTable } from "@opencode-ai/core/session/sql"
import { GoalTool } from "@opencode-ai/core/tool/goal"
import { ToolRegistry } from "@opencode-ai/core/tool/registry"
import { ToolOutputStore } from "@opencode-ai/core/tool-output-store"
import { testEffect } from "./lib/effect"
import { settleTool, toolDefinitions, toolIdentity } from "./lib/tool"

const sessionID = SessionV2.ID.make("ses_goal_tool_test")
const assertions: PermissionV2.AssertInput[] = []
const names = ["goal_set", "goal_pause", "goal_resume", "goal_complete", "goal_status", "goal_summarize_state"]

const permission = Layer.succeed(
  PermissionV2.Service,
  PermissionV2.Service.of({
    assert: (input) => Effect.sync(() => assertions.push(input)),
    ask: () => Effect.die("unused"),
    reply: () => Effect.die("unused"),
    get: () => Effect.die("unused"),
    forSession: () => Effect.die("unused"),
    list: () => Effect.die("unused"),
  }),
)
const it = testEffect(
  AppNodeBuilder.build(
    LayerNode.group([Database.node, SessionGoal.node, ToolRegistry.node, ToolRegistry.toolsNode, GoalTool.node]),
    [
      [PermissionV2.node, permission],
      [ToolOutputStore.node, ToolOutputStore.nodeWithoutConfig],
    ],
  ),
)

const setup = Effect.gen(function* () {
  assertions.length = 0
  const { db } = yield* Database.Service
  yield* db
    .insert(ProjectTable)
    .values({ id: Project.ID.global, worktree: AbsolutePath.make("/project"), sandboxes: [] })
    .onConflictDoNothing()
    .run()
    .pipe(Effect.orDie)
  yield* db
    .insert(SessionTable)
    .values({
      id: sessionID,
      project_id: Project.ID.global,
      slug: "goal",
      directory: "/project",
      title: "goal",
      version: "test",
    })
    .onConflictDoNothing()
    .run()
    .pipe(Effect.orDie)
})

const call = (name: string, input: Record<string, unknown>) => ({
  sessionID,
  ...toolIdentity,
  call: { type: "tool-call" as const, id: `call-${name}`, name, input },
})

describe("GoalTool", () => {
  it.effect("registers goal tools and persists goal state", () =>
    Effect.gen(function* () {
      yield* setup
      const registry = yield* ToolRegistry.Service
      const goals = yield* SessionGoal.Service

      expect((yield* toolDefinitions(registry)).map((tool) => tool.name)).toEqual(names)
      expect(yield* settleTool(registry, call("goal_set", { text: "Ship V2 recovery" }))).toMatchObject({
        result: { type: "text", value: expect.stringContaining("Goal: Ship V2 recovery") },
      })
      expect(yield* goals.get(sessionID)).toMatchObject({
        text: "Ship V2 recovery",
        status: "active",
        revision: 1,
      })
      expect(assertions).toMatchObject([{ sessionID, action: "goal_set", resources: ["*"], save: ["*"] }])
    }),
  )
})
