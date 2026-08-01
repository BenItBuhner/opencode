import { expect, test } from "bun:test"
import { UI } from "@/cli/ui"

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

test("plain CLI wordmark renders OpenGoal glyphs without marker characters", () => {
  const output = UI.logo()
  expect(output).toContain("█▀▀▀ █▀▀█ ▄▀▀█ █")
  expect(output).not.toContain("_")
  expect(output).not.toContain("^")
})

test("inline TUI splash uses the compact OpenGoal mark and command", async () => {
  const splash = await Bun.file(new URL("../src/cli/cmd/run/splash.ts", import.meta.url)).text()

  expect(splash).toContain("const mark = go.left.slice(1)")
  expect(splash).toContain("const markRight = go.right.slice(1)")
  expect(splash).toContain('"OpenGoal"')
  expect(splash).toContain("`opengoal --mini -s ${meta.session_id}`")
  expect(splash).not.toContain('"OpenCode"')
})

test("installer targets OpenGoal releases and paths", async () => {
  const installer = await Bun.file(new URL("../../../install", import.meta.url)).text()
  const installation = await Bun.file(new URL("../src/installation/index.ts", import.meta.url)).text()

  expect(installer).toContain("APP=opengoal")
  expect(installer).toContain("REPO=${OPENGOAL_REPO:-BenItBuhner/opengoal}")
  expect(installer).toContain("INSTALL_DIR=$HOME/.opengoal/bin")
  expect(installer).toContain('mv "$tmp_dir/opengoal" "$INSTALL_DIR"')
  expect(installer).toContain('cp "$binary_path" "${INSTALL_DIR}/opengoal"')
  expect(installer).not.toContain("github.com/anomalyco/opencode/releases")
  expect(installer).not.toContain("$HOME/.opencode/bin")
  expect(installation).toContain("api.github.com/repos/BenItBuhner/opengoal/releases/latest")
  expect(installation).toContain("@benitbuhner/opengoal")
  expect(installation).toContain('path.join(".opengoal", "bin")')
  expect(installation).not.toContain("HttpClientRequest.get(\"https://opencode.ai/install\")")
})
