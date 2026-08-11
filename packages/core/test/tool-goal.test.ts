import { describe, expect } from "bun:test"
import { Effect, Layer } from "effect"
import { and, eq } from "drizzle-orm"
import { Database } from "@opencode-ai/core/database/database"
import { AppNodeBuilder } from "@opencode-ai/core/effect/app-node-builder"
import { LayerNode } from "@opencode-ai/core/effect/layer-node"
import { PermissionV2 } from "@opencode-ai/core/permission"
import { Project } from "@opencode-ai/core/project"
import { EventV2 } from "@opencode-ai/core/event"
import { EventTable } from "@opencode-ai/core/event/sql"
import { ProjectTable } from "@opencode-ai/core/project/sql"
import { AbsolutePath } from "@opencode-ai/core/schema"
import { SessionV2 } from "@opencode-ai/core/session"
import { SessionGoal } from "@opencode-ai/core/session/goal"
import { fromRow } from "@opencode-ai/core/session/info"
import { SessionEvent } from "@opencode-ai/core/session/event"
import { SessionProjector } from "@opencode-ai/core/session/projector"
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
    LayerNode.group([
      Database.node,
      EventV2.node,
      SessionProjector.node,
      SessionGoal.node,
      ToolRegistry.node,
      ToolRegistry.toolsNode,
      GoalTool.node,
    ]),
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
      const { db } = yield* Database.Service
      const registry = yield* ToolRegistry.Service
      const goals = yield* SessionGoal.Service
      const events = yield* EventV2.Service

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

      const recorded = yield* db
        .select()
        .from(EventTable)
        .where(eq(EventTable.aggregate_id, sessionID))
        .all()
        .pipe(Effect.orDie)
      yield* events.remove(sessionID)
      yield* db
        .update(SessionTable)
        .set({ metadata: null })
        .where(eq(SessionTable.id, sessionID))
        .run()
        .pipe(Effect.orDie)
      yield* events.replayAll(
        recorded.map((event) => ({
          id: event.id,
          aggregateID: event.aggregate_id,
          seq: event.seq,
          type: event.type,
          data: event.data,
        })),
      )
      expect(yield* goals.get(sessionID)).toMatchObject({
        text: "Ship V2 recovery",
        status: "active",
        revision: 1,
      })
    }),
  )

  it.effect("persists the complete goal lifecycle as durable Session events", () =>
    Effect.gen(function* () {
      yield* setup
      const { db } = yield* Database.Service
      const registry = yield* ToolRegistry.Service
      const goals = yield* SessionGoal.Service
      const summary = [
        "## Progress",
        "- Restored current protocol",
        "## Current State",
        "- Queue semantics pass",
        "## Blockers",
        "- None",
        "## Next Steps",
        "- Ship",
      ].join("\n")

      yield* settleTool(registry, call("goal_set", { text: "Ship V2 recovery" }))
      yield* settleTool(registry, call("goal_summarize_state", { progress: 80, summary, headline: "Ready" }))
      yield* settleTool(registry, call("goal_pause", {}))
      yield* settleTool(registry, call("goal_resume", {}))
      expect(yield* settleTool(registry, call("goal_status", {}))).toMatchObject({
        result: { type: "text", value: expect.stringContaining("Progress: 80%") },
      })
      yield* settleTool(registry, call("goal_complete", {}))

      expect(yield* goals.get(sessionID)).toBeUndefined()
      const completedRow = yield* db
        .select()
        .from(SessionTable)
        .where(eq(SessionTable.id, sessionID))
        .get()
        .pipe(Effect.orDie)
      expect(completedRow?.metadata?.completed_goal).toMatchObject({ status: "completed", progress: 80 })
      expect(completedRow?.metadata?.goal).toBeUndefined()
      if (!completedRow) return yield* Effect.die("Completed Goal row missing")
      const completed = fromRow(completedRow)
      expect(completed.completedGoal).toMatchObject({ status: "completed", progress: 80 })
      expect(completed.goal).toBeUndefined()
      expect(assertions.map((item) => item.action)).toEqual([
        "goal_set",
        "goal_summarize_state",
        "goal_pause",
        "goal_resume",
        "goal_status",
        "goal_complete",
      ])
      expect(
        yield* db
          .select()
          .from(EventTable)
          .where(
            and(
              eq(EventTable.aggregate_id, sessionID),
              eq(EventTable.type, EventV2.versionedType(SessionEvent.GoalUpdated.type, 1)),
            ),
          )
          .all()
          .pipe(Effect.orDie),
      ).toHaveLength(6)
    }),
  )

  it.effect("rejects malformed summaries without changing goal state", () =>
    Effect.gen(function* () {
      yield* setup
      const registry = yield* ToolRegistry.Service
      const goals = yield* SessionGoal.Service
      yield* settleTool(registry, call("goal_set", { text: "Ship V2 recovery" }))

      expect(
        yield* settleTool(registry, call("goal_summarize_state", { progress: 80, summary: "# Wrong\n- format" })),
      ).toMatchObject({
        result: { type: "error", value: expect.stringContaining("size 2 markdown headers") },
      })
      expect(yield* goals.get(sessionID)).toMatchObject({ revision: 1, summaries: undefined })
    }),
  )
})
