import { expect, test } from "bun:test"

test("browser metadata and icons use OpenGoal branding", async () => {
  const html = await Bun.file(new URL("../index.html", import.meta.url)).text()
  const manifest = await Bun.file(new URL("../public/site.webmanifest", import.meta.url)).json()
  const favicon = await Bun.file(new URL("../public/favicon-v3.svg", import.meta.url)).text()

  expect(html).toContain("<title>OpenGoal</title>")
  expect(html).not.toContain("<title>OpenCode</title>")
  expect(manifest.name).toBe("OpenGoal")
  expect(manifest.short_name).toBe("OpenGoal")
  expect(favicon).toContain("<title>OpenGoal</title>")
  expect(favicon).toContain('aria-label="OpenGoal"')
})
