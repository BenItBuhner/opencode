import path from "path"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import { Effect } from "effect"
import { Agent } from "@/agent/agent"
import { FSUtil } from "@opencode-ai/core/fs-util"
import { InstanceState } from "@/effect/instance-state"
import { RuntimeFlags } from "@/effect/runtime-flags"
import { PartID } from "./schema"
import { MessageV2 } from "./message-v2"
import { Session } from "./session"
import PROMPT_PLAN from "./prompt/plan.txt"
import BUILD_SWITCH from "./prompt/build-switch.txt"
import PLAN_MODE from "./prompt/plan-mode.txt"

export const apply = Effect.fn("SessionReminders.apply")(function* (input: {
  messages: SessionV1.WithParts[]
  agent: Agent.Info
  session: Session.Info
}) {
  const flags = yield* RuntimeFlags.Service
  const fsys = yield* FSUtil.Service
  const sessions = yield* Session.Service
  const userMessage = input.messages.findLast((msg) => msg.info.role === "user")
  if (!userMessage) return input.messages

  if (Session.isGoalHarnessSession(input.session, input.agent.name)) {
    const goal = yield* sessions.getGoal(input.session.id)
    const latestSummary = goal?.summaries?.at(-1)
    userMessage.parts.push({
      id: PartID.ascending(),
      messageID: userMessage.info.id,
      sessionID: userMessage.info.sessionID,
      type: "text",
      text: goal
        ? [
            "<session-goal>",
            `Status: ${goal.status}`,
            goal.progress === undefined ? undefined : `Progress: ${goal.progress}%`,
            `Goal: ${goal.text}`,
            latestSummary
              ? [
                  "",
                  "<latest-goal-state-summary>",
                  `Updated: ${new Date(latestSummary.created).toISOString()}`,
                  `Progress: ${latestSummary.progress}%`,
                  latestSummary.summary,
                  "</latest-goal-state-summary>",
                ].join("\n")
              : undefined,
            "",
            goal.status === "active"
              ? [
                  "Continue working toward this objective until it is completed, paused, or blocked by a question for the user.",
                  "Do not stop after a progress update. If the goal is not complete yet, take the next concrete action.",
                  "Call goal_summarize_state after meaningful progress or when the latest summary is materially stale.",
                  "When the goal is complete, call goal_complete before giving the final summary.",
                ].join("\n")
              : goal.status === "paused"
                ? "The goal is paused. Do not continue it unless the user explicitly resumes it."
                : "The goal is completed. Do not continue it unless the user explicitly resumes or replaces it.",
            "</session-goal>",
          ]
            .filter((line) => line !== undefined)
            .join("\n")
        : [
            "<session-goal>",
            "No session goal is set. Ask the user what goal to set, or set one only if they explicitly provide it.",
            "</session-goal>",
          ].join("\n"),
      synthetic: true,
    })
  }

  if (!flags.experimentalPlanMode) {
    if (input.agent.name === "plan") {
      userMessage.parts.push({
        id: PartID.ascending(),
        messageID: userMessage.info.id,
        sessionID: userMessage.info.sessionID,
        type: "text",
        text: PROMPT_PLAN,
        synthetic: true,
      })
    }
    const wasPlan = input.messages.some((msg) => msg.info.role === "assistant" && msg.info.agent === "plan")
    if (wasPlan && input.agent.name === "build") {
      userMessage.parts.push({
        id: PartID.ascending(),
        messageID: userMessage.info.id,
        sessionID: userMessage.info.sessionID,
        type: "text",
        text: BUILD_SWITCH,
        synthetic: true,
      })
    }
    return input.messages
  }

  const assistantMessage = input.messages.findLast((msg) => msg.info.role === "assistant")
  if (input.agent.name !== "plan" && assistantMessage?.info.agent === "plan") {
    const ctx = yield* InstanceState.context
    const plan = Session.plan(input.session, ctx)
    const exists = yield* fsys.existsSafe(plan)
    const part = yield* sessions.updatePart({
      id: PartID.ascending(),
      messageID: userMessage.info.id,
      sessionID: userMessage.info.sessionID,
      type: "text",
      text: exists
        ? `${BUILD_SWITCH}\n\nA plan file exists at ${plan}. You should execute on the plan defined within it`
        : BUILD_SWITCH,
      synthetic: true,
    })
    userMessage.parts.push(part)
    return input.messages
  }

  if (input.agent.name !== "plan" || assistantMessage?.info.agent === "plan") return input.messages

  const ctx = yield* InstanceState.context
  const plan = Session.plan(input.session, ctx)
  const exists = yield* fsys.existsSafe(plan)
  if (!exists) yield* fsys.ensureDir(path.dirname(plan)).pipe(Effect.catch(Effect.die))
  const part = yield* sessions.updatePart({
    id: PartID.ascending(),
    messageID: userMessage.info.id,
    sessionID: userMessage.info.sessionID,
    type: "text",
    text: PLAN_MODE.replace("${planInfo}", () =>
      exists
        ? `A plan file already exists at ${plan}. You can read it and make incremental edits using the edit tool.`
        : `No plan file exists yet. You should create your plan at ${plan} using the write tool.`,
    ),
    synthetic: true,
  })
  userMessage.parts.push(part)
  return input.messages
})

export * as SessionReminders from "./reminders"
