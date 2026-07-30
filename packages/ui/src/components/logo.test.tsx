import { expect, test } from "bun:test"

test("shared logo assets carry the OpenGoal identity", async () => {
  const source = await Bun.file(new URL("./logo.tsx", import.meta.url)).text()

  expect(source.match(/data-brand="opengoal"/g)).toHaveLength(3)
  expect(source.match(/data-glyph="g"/g)).toHaveLength(2)
  expect(source.match(/aria-label="OpenGoal"/g)).toHaveLength(3)
  expect(source).toContain('data-slot="logo-logo-mark-g"')
  expect(source).not.toContain('data-slot="logo-logo-mark-o"')
  expect(source).toContain('data-wordmark="opengoal"')
  expect(source.match(/<title>OpenGoal<\/title>/g)).toHaveLength(3)
})
