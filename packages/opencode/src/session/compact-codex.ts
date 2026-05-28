import { Token } from "@/util/token"
import type { MessageV2 } from "./message-v2"

export const SUMMARIZATION_PROMPT = `You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue

Be concise, structured, and focused on helping the next LLM seamlessly continue the work.`

export const SUMMARY_PREFIX =
  "Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:"

export const COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000

export function isSummaryMessage(text: string) {
  return text.startsWith(`${SUMMARY_PREFIX}\n`)
}

export function compactSummaryText(text: string | undefined) {
  const summary = text?.trim() || "(no summary available)"
  return `${SUMMARY_PREFIX}\n${summary}`
}

export function realUserText(message: MessageV2.WithParts) {
  if (message.info.role !== "user") return
  if (message.parts.some((part) => part.type === "compaction")) return
  const text = message.parts
    .filter((part): part is MessageV2.TextPart => part.type === "text" && !part.ignored)
    .map((part) => part.text.trim())
    .filter((part) => part && !isSummaryMessage(part))
    .join("\n\n")
    .trim()
  return text || undefined
}

export function summaryAssistantText(message: MessageV2.WithParts) {
  if (message.info.role !== "assistant") return
  if (!message.info.summary || !message.info.finish || message.info.error) return
  const text = message.parts
    .filter((part): part is MessageV2.TextPart => part.type === "text")
    .map((part) => part.text.trim())
    .filter(Boolean)
    .join("\n\n")
    .trim()
  return text || undefined
}

export function selectRecentUserTexts(messages: MessageV2.WithParts[], maxTokens = COMPACT_USER_MESSAGE_MAX_TOKENS) {
  const selected: string[] = []
  let remaining = maxTokens
  for (const text of messages.flatMap((message) => realUserText(message) ?? []).toReversed()) {
    if (remaining <= 0) break
    const tokens = Token.estimate(text)
    if (tokens <= remaining) {
      selected.push(text)
      remaining -= tokens
      continue
    }
    selected.push(truncateByApproxTokens(text, remaining))
    break
  }
  return selected.toReversed()
}

function truncateByApproxTokens(text: string, tokens: number) {
  if (tokens <= 0) return ""
  const ratio = Math.min(1, tokens / Math.max(1, Token.estimate(text)))
  return text.slice(0, Math.max(0, Math.floor(text.length * ratio * 0.95))).trim()
}
