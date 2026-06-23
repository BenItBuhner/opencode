import { ConfigV1 } from "@opencode-ai/core/v1/config/config"
import { SessionV1 } from "@opencode-ai/core/v1/session"
import type { Provider } from "@/provider/provider"
import type { MessageV2 } from "./message-v2"

const DEFAULT_COMPACTION_THRESHOLD = 0.9

export function usable(input: { cfg: ConfigV1.Info; model: Provider.Model }) {
  const context = input.model.limit.context
  if (context === 0) return 0

  const reserved = input.cfg.compaction?.reserved
  if (reserved === undefined) {
    return Math.max(
      0,
      Math.floor(
        Math.min(input.model.limit.input ?? context, context) *
          (input.cfg.compaction?.threshold ?? DEFAULT_COMPACTION_THRESHOLD),
      ),
    )
  }

  return input.model.limit.input
    ? Math.max(0, input.model.limit.input - reserved)
    : Math.max(0, context - reserved)
}

export function isOverflow(input: {
  cfg: ConfigV1.Info
  tokens: SessionV1.Assistant["tokens"]
  model: Provider.Model
}) {
  if (input.cfg.compaction?.auto === false) return false
  if (input.model.limit.context === 0) return false

  const count =
    input.tokens.total || input.tokens.input + input.tokens.output + input.tokens.cache.read + input.tokens.cache.write
  return count >= usable(input)
}
