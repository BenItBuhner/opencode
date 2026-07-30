import { expect, test } from "bun:test"

test("empty prompt uses a CSS zero-width placeholder", async () => {
  const component = await Bun.file(new URL("./prompt-input/index.tsx", import.meta.url)).text()
  const styles = await Bun.file(new URL("./prompt-input-v2.css", import.meta.url)).text()

  expect(component).toContain('data-slot="prompt-input-v2-editor"')
  expect(component).not.toContain("content-['\\\\200B']")
  expect(styles).toContain('[data-slot="prompt-input-v2-editor"]:empty::before')
  expect(styles).toContain('content: "\\200B"')
})
