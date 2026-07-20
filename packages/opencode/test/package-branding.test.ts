import { expect, test } from "bun:test"

test("workspace and CLI packages use OpenGoal branding", async () => {
  const root = await Bun.file(new URL("../../../package.json", import.meta.url)).json()
  const cli = await Bun.file(new URL("../package.json", import.meta.url)).json()
  const web = await Bun.file(new URL("../../web/package.json", import.meta.url)).json()

  expect(root.name).toBe("opengoal")
  expect(cli.name).toBe("opengoal")
  expect(cli.bin).toEqual({
    opengoal: "./bin/opencode",
    opencode: "./bin/opencode",
  })
  expect(web.devDependencies).toHaveProperty("opengoal", "workspace:*")
  expect(web.devDependencies).not.toHaveProperty("opencode")
})

test("publish pipeline emits OpenGoal packages and command aliases", async () => {
  const build = await Bun.file(new URL("../script/build.ts", import.meta.url)).text()
  const launcher = await Bun.file(new URL("../script/launcher.mjs", import.meta.url)).text()
  const publish = await Bun.file(new URL("../script/publish.ts", import.meta.url)).text()

  expect(build).toContain("outfile: `dist/${name}/bin/${pkg.name}`")
  expect(launcher).toContain("@benitbuhner/opengoal-")
  expect(launcher).toContain('sourceBinary = platform === "windows" ? "opengoal.exe" : "opengoal"')
  expect(publish).toContain('const packageName = "@benitbuhner/opengoal"')
  expect(publish).toContain('const commandName = "opengoal"')
  expect(publish).toContain('opencode: "./bin/opencode.mjs"')
})
