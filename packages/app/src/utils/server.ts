import { createOpencodeClient } from "@opencode-ai/sdk/v2/client"
import {
  OpenCode,
  type OpenCodeClient,
  type SessionPromptInput,
  type SessionPromptOutput,
} from "@opencode-ai/client/promise"
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
type CurrentPromptOptions = Parameters<OpenCodeClient["session"]["prompt"]>[1]

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
  const prompt = async (
    value: CurrentPromptInput,
    requestOptions?: CurrentPromptOptions,
  ): Promise<SessionPromptOutput> => {
    if (value.agent)
      await client.session.switchAgent({ sessionID: value.sessionID, agent: value.agent }, requestOptions)
    if (value.model)
      await client.session.switchModel(
        {
          sessionID: value.sessionID,
          model: {
            id: value.model.modelID,
            providerID: value.model.providerID,
            variant: value.variant,
          },
        },
        requestOptions,
      )
    const requestHeaders = new Headers(headers)
    for (const [key, value] of new Headers(requestOptions?.headers)) requestHeaders.set(key, value)
    requestHeaders.set("content-type", "application/json")
    const response = await (input.fetch ?? globalThis.fetch)(
      `${input.server.url}/api/session/${encodeURIComponent(value.sessionID)}/prompt`,
      {
        method: "POST",
        signal: requestOptions?.signal,
        headers: requestHeaders,
        body: JSON.stringify({
          id: value.id,
          prompt: {
            text: value.text,
            files: value.files?.map((file) => ({
              uri: file.uri,
              mime: file.mention ? "text/plain" : mime(file.uri),
              name: file.name,
              description: file.description,
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
    const admitted = (await response.json()) as {
      data: {
        admittedSeq: number
        id: string
        sessionID: string
        timeCreated: number
        delivery: "steer" | "queue"
      }
    }
    return {
      admittedSeq: admitted.data.admittedSeq,
      id: admitted.data.id,
      sessionID: admitted.data.sessionID,
      timeCreated: admitted.data.timeCreated,
      type: "user",
      data: { text: value.text },
      delivery: admitted.data.delivery,
    }
  }
  return {
    ...client,
    session: {
      ...client.session,
      prompt,
      command: (value, requestOptions) =>
        prompt(
          {
            sessionID: value.sessionID,
            id: value.id,
            text: `/${value.command} ${value.arguments ?? ""}`.trim(),
            agent: value.agent ?? undefined,
            model: value.model ? { providerID: value.model.providerID, modelID: value.model.id } : undefined,
            variant: value.model?.variant,
            files: value.files,
            agents: value.agents,
            delivery: value.delivery,
            resume: value.resume,
          },
          requestOptions,
        ),
    },
  }
}

export type ServerApi = OpenCodeClient

function mime(uri: string) {
  return /^data:([^;,]+)/.exec(uri)?.[1] ?? "application/octet-stream"
}
