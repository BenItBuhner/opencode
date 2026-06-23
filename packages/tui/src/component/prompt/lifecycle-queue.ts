import { createRoot } from "solid-js"
import { createStore, produce } from "solid-js/store"
import type { PromptInfo } from "../../prompt/history"

export type LifecycleQueuedPrompt = {
  id: string
  prompt: PromptInfo
  mode: "normal" | "shell"
}

let nextLifecycleID = 0

function createLifecycleID() {
  nextLifecycleID += 1
  return `lifecycle-${nextLifecycleID}`
}

export const lifecycleQueue = createRoot(() => {
  const [store, setStore] = createStore<{ queues: Record<string, LifecycleQueuedPrompt[]> }>({ queues: {} })

  return {
    store,
    list(sessionID: string) {
      return store.queues[sessionID] ?? []
    },
    enqueue(sessionID: string, item: { prompt: PromptInfo; mode: "normal" | "shell" }) {
      const entry: LifecycleQueuedPrompt = { id: createLifecycleID(), ...item }
      setStore("queues", sessionID, (queue = []) => [...queue, entry])
      return entry
    },
    shift(sessionID: string) {
      let next: LifecycleQueuedPrompt | undefined
      setStore(
        produce((draft) => {
          const queue = draft.queues[sessionID]
          if (!queue?.length) return
          next = queue.shift()
          if (queue.length === 0) delete draft.queues[sessionID]
        }),
      )
      return next
    },
  }
})

export function lifecyclePromptText(prompt: PromptInfo) {
  const trimmed = prompt.input.trim()
  if (trimmed) return trimmed
  return prompt.parts
    .flatMap((part) => (part.type === "text" && !part.synthetic ? [part.text] : []))
    .join("\n\n")
    .trim()
}
