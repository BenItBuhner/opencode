import { createOpencodeClient } from "@opencode-ai/sdk/v2/client"
import { OpenCode, type OpenCodeClient, type SessionPromptInput, type SessionPromptOutput } from "@opencode-ai/client/promise"
import type { ServerConnection } from "@/context/server"
import { decode64 } from "@/utils/base64"

type CurrentPromptInput = SessionPromptInput & {
  delivery?: "steer" | "queue" | null
  resume?: boolean | null
  files?: ReadonlyArray<{
    uri: string
    name?: string
    description?: string
    mention?: { start: number; end: number; text: string }
  }>
  agents?: ReadonlyArray<{ name: string; mention?: { start: number; end: number; text: string } }>
}

export function authTokenFromCredentials(input: { username?: string; password: string }) {
  return btoa(`${input.username ?? "opencode"}:${input.password}`)
}

export function authFromToken(token: string | null) {
  const decoded = decode64(token ?? undefined)
  if (!decoded) return
  const separator = decoded.indexOf(":")
  if (separator === -1) return
  return {
    username: decoded.slice(0, separator) || "opencode",
    password: decoded.slice(separator + 1),
  }
}

export function createSdkForServer({
  server,
  ...config
}: Omit<NonNullable<Parameters<typeof createOpencodeClient>[0]>, "baseUrl"> & {
  server: ServerConnection.HttpBase
}) {
  const auth = (() => {
    if (!server.password) return
    return {
      Authorization: `Basic ${authTokenFromCredentials({ username: server.username, password: server.password })}`,
    }
  })()

  return createOpencodeClient({
    ...config,
    headers: {
      ...(config.headers instanceof Headers ? Object.fromEntries(config.headers.entries()) : config.headers),
      ...auth,
    },
    baseUrl: server.url,
  })
}

export function createApiForServer(input: {
  server: ServerConnection.HttpBase
  fetch?: typeof globalThis.fetch
}): OpenCodeClient {
  const headers = input.server.password
    ? {
        Authorization: `Basic ${authTokenFromCredentials({
          username: input.server.username,
          password: input.server.password,
        })}`,
      }
    : undefined
  const client = OpenCode.make({
    baseUrl: input.server.url,
    fetch: input.fetch,
    headers,
  })
  return {
    ...client,
    session: {
      ...client.session,
      async prompt(value: CurrentPromptInput, _requestOptions?: unknown): Promise<SessionPromptOutput> {
        const response = await (input.fetch ?? globalThis.fetch)(
          `${input.server.url}/api/session/${encodeURIComponent(value.sessionID)}/prompt`,
          {
            method: "POST",
            headers: {
              "content-type": "application/json",
              ...headers,
            },
            body: JSON.stringify({
              id: value.id,
              prompt: {
                text: value.text,
                files: value.files?.map((file) => ({
                  uri: file.uri,
                  mime: file.mention ? "text/plain" : mime(file.uri),
                  name: file.name,
                  source: file.mention,
                })),
                agents: value.agents?.map((agent) => ({
                  name: agent.name,
                  source: agent.mention,
                })),
              },
              delivery: value.delivery ?? undefined,
              resume: value.resume ?? undefined,
            }),
          },
        )
        if (!response.ok) throw new Error((await response.text()) || `Prompt failed with status ${response.status}`)
        return {
          admittedSeq: 0,
          id: value.id ?? "",
          sessionID: value.sessionID,
          timeCreated: Date.now(),
          type: "user",
          data: { text: value.text },
          delivery: value.delivery ?? "steer",
        }
      },
    },
  }
}

export type ServerApi = OpenCodeClient

function mime(uri: string) {
  return /^data:([^;,]+)/.exec(uri)?.[1] ?? "application/octet-stream"
}
