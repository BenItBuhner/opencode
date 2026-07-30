import { expect, test } from "bun:test"

test("new-layout wordmark spells OpenGoal", async () => {
  const source = await Bun.file(new URL("./wordmark-v2.tsx", import.meta.url)).text()

  expect(source).toContain('data-brand="opengoal"')
  expect(source).toContain('data-wordmark="opengoal"')
  expect(source).toContain('aria-label="OpenGoal"')
  expect(source).toContain('data-letter="g"')
  expect(source).toContain('data-letter="a"')
  expect(source).toContain('data-letter="l"')
  expect(source).not.toContain("M442.846 36.4286H387.462V91.7143")
  expect(source).not.toContain("M609.385 36.8571H572.462V92.1429")
})
