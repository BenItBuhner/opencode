import { expect, test } from "bun:test"
import { renderToString } from "solid-js/web"
import { Logo, Mark, Splash } from "./logo"

test("shared logo assets carry the OpenGoal identity", () => {
  const mark = renderToString(() => <Mark />)
  const splash = renderToString(() => <Splash />)
  const wordmark = renderToString(() => <Logo />)

  expect(mark).toContain('data-brand="opengoal"')
  expect(mark).toContain('data-glyph="g"')
  expect(mark).toContain('aria-label="OpenGoal"')
  expect(mark).toContain('data-slot="logo-logo-mark-g"')
  expect(mark).not.toContain('data-slot="logo-logo-mark-o"')
  expect(splash).toContain('data-brand="opengoal"')
  expect(splash).toContain('data-glyph="g"')
  expect(wordmark).toContain('data-wordmark="opengoal"')
  expect(wordmark).toContain("<title>OpenGoal</title>")
})
