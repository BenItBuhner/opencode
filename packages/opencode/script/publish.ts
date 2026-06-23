#!/usr/bin/env bun
import { $ } from "bun"
import fs from "fs"
import path from "path"
import pkg from "../package.json"
import { Script } from "@opencode-ai/script"
import { fileURLToPath } from "url"

const dir = fileURLToPath(new URL("..", import.meta.url))
process.chdir(dir)

const packageName = "@benitbuhner/opengoal"
const packageDirName = "opengoal"
const commandName = "opengoal"
const publishExtraRegistries = process.env.OPENCODE_GOAL_MODE_PUBLISH_REGISTRIES === "1"
const packOnly = process.env.OPENCODE_GOAL_MODE_PACK_ONLY === "1"

async function published(name: string, version: string) {
  return (await $`npm view ${name}@${version} version`.nothrow()).exitCode === 0
}

function ensureBinExecutable(dir: string) {
  const binDir = path.join(dir, "bin")
  if (!fs.existsSync(binDir)) return
  for (const name of fs.readdirSync(binDir)) {
    const target = path.join(binDir, name)
    if (!fs.statSync(target).isFile()) continue
    try {
      fs.chmodSync(target, 0o755)
    } catch {
      // Publishing from Windows cannot always set Unix modes in the tarball.
    }
  }
}

async function publish(dir: string, name: string, version: string) {
  // GitHub artifact downloads can drop the executable bit, and Docker uses the
  // unpacked dist binaries directly rather than the published tarball.
  ensureBinExecutable(dir)
  if (process.platform !== "win32") await $`chmod -R 755 .`.cwd(dir)
  if (await published(name, version)) {
    console.log(`already published ${name}@${version}`)
    return
  }
  for (const artifact of new Bun.Glob("*.tgz").scanSync({ cwd: dir })) {
    fs.rmSync(path.join(dir, artifact), { force: true })
  }
  await $`bun pm pack`.cwd(dir)
  if (packOnly) return
  const npmrc = path.join(dir, ".npmrc")
  if (process.env.NPM_TOKEN) {
    fs.writeFileSync(npmrc, "//registry.npmjs.org/:_authToken=${NPM_TOKEN}\n")
  }
  try {
    await $`npm publish *.tgz --access public --tag ${Script.channel}`.cwd(dir)
  } finally {
    fs.rmSync(npmrc, { force: true })
  }
}

type BinaryPackage = {
  dir: string
  name: string
  version: string
}

const binaryPackages: BinaryPackage[] = []
const binaries: Record<string, string> = {}
fs.rmSync(`./dist/${packageDirName}`, { recursive: true, force: true })
for (const filepath of new Bun.Glob("*/package.json").scanSync({ cwd: "./dist" })) {
  if (filepath === `${packageDirName}/package.json` || filepath === `${packageDirName}\\package.json`) continue
  const item = await Bun.file(`./dist/${filepath}`).json()
  const dir = `./dist/${filepath.replace(/[\\/]package\.json$/, "")}`
  const name = String(item.name).replace(/^opengoal-/, `${packageName}-`)
  await Bun.file(`./dist/${filepath}`).write(
    JSON.stringify(
      {
        ...item,
        name,
      },
      null,
      2,
    ),
  )
  binaries[name] = item.version
  binaryPackages.push({ dir, name, version: item.version })
}
console.log("binaries", binaries)
const version = process.env.OPENCODE_GOAL_MODE_META_VERSION ?? Object.values(binaries)[0]

await $`mkdir -p ./dist/${packageDirName}`
await $`mkdir -p ./dist/${packageDirName}/bin`
await $`cp ./script/launcher.mjs ./dist/${packageDirName}/bin/${commandName}.mjs`
await $`cp ./script/launcher.mjs ./dist/${packageDirName}/bin/opencode.mjs`
await $`cp ./script/npm-postinstall-chmod.mjs ./dist/${packageDirName}/postinstall.mjs`
await Bun.file(`./dist/${packageDirName}/LICENSE`).write(await Bun.file("../../LICENSE").text())
ensureBinExecutable(`./dist/${packageDirName}`)

await Bun.file(`./dist/${packageDirName}/package.json`).write(
  JSON.stringify(
    {
      name: packageName,
      type: "module",
      files: ["bin", "postinstall.mjs", "LICENSE"],
      bin: {
        [commandName]: `./bin/${commandName}.mjs`,
        opencode: "./bin/opencode.mjs",
      },
      scripts: {
        postinstall: "node postinstall.mjs",
      },
      version: version,
      license: pkg.license,
      os: ["darwin", "linux", "win32"],
      cpu: ["arm64", "x64"],
      optionalDependencies: binaries,
    },
    null,
    2,
  ),
)

const tasks = binaryPackages.map(async (item) => {
  await publish(item.dir, item.name, item.version)
})
await Promise.all(tasks)
await publish(`./dist/${packageDirName}`, packageName, version)

const image = "ghcr.io/anomalyco/opencode"
const platforms = "linux/amd64,linux/arm64"
const tags = [`${image}:${version}`, `${image}:${Script.channel}`]
const tagFlags = tags.flatMap((t) => ["-t", t])

// registries
if (publishExtraRegistries && !Script.preview) {
  await $`docker buildx build --platform ${platforms} ${tagFlags} --push .`
  // Calculate SHA values
  const arm64Sha = await $`sha256sum ./dist/opencode-linux-arm64.tar.gz | cut -d' ' -f1`.text().then((x) => x.trim())
  const x64Sha = await $`sha256sum ./dist/opencode-linux-x64.tar.gz | cut -d' ' -f1`.text().then((x) => x.trim())
  const macX64Sha = await $`sha256sum ./dist/opencode-darwin-x64.zip | cut -d' ' -f1`.text().then((x) => x.trim())
  const macArm64Sha = await $`sha256sum ./dist/opencode-darwin-arm64.zip | cut -d' ' -f1`.text().then((x) => x.trim())

  const [pkgver, _subver = ""] = Script.version.split(/(-.*)/, 2)

  // arch
  const binaryPkgbuild = [
    "# Maintainer: dax",
    "# Maintainer: adam",
    "",
    "pkgname='opencode-bin'",
    `pkgver=${pkgver}`,
    `_subver=${_subver}`,
    "options=('!debug' '!strip')",
    "pkgrel=1",
    "pkgdesc='The AI coding agent built for the terminal.'",
    "url='https://github.com/anomalyco/opencode'",
    "arch=('aarch64' 'x86_64')",
    "license=('MIT')",
    "provides=('opencode')",
    "conflicts=('opencode')",
    "depends=('ripgrep')",
    "",
    `source_aarch64=("\${pkgname}_\${pkgver}_aarch64.tar.gz::https://github.com/anomalyco/opencode/releases/download/v\${pkgver}\${_subver}/opencode-linux-arm64.tar.gz")`,
    `sha256sums_aarch64=('${arm64Sha}')`,

    `source_x86_64=("\${pkgname}_\${pkgver}_x86_64.tar.gz::https://github.com/anomalyco/opencode/releases/download/v\${pkgver}\${_subver}/opencode-linux-x64.tar.gz")`,
    `sha256sums_x86_64=('${x64Sha}')`,
    "",
    "package() {",
    '  install -Dm755 ./opencode "${pkgdir}/usr/bin/opencode"',
    "}",
    "",
  ].join("\n")

  for (const [pkg, pkgbuild] of [["opencode-bin", binaryPkgbuild]]) {
    for (let i = 0; i < 30; i++) {
      try {
        await $`rm -rf ./dist/aur-${pkg}`
        await $`git clone ssh://aur@aur.archlinux.org/${pkg}.git ./dist/aur-${pkg}`
        await $`cd ./dist/aur-${pkg} && git checkout master`
        await Bun.file(`./dist/aur-${pkg}/PKGBUILD`).write(pkgbuild)
        await $`cd ./dist/aur-${pkg} && makepkg --printsrcinfo > .SRCINFO`
        await $`cd ./dist/aur-${pkg} && git add PKGBUILD .SRCINFO`
        if ((await $`cd ./dist/aur-${pkg} && git diff --cached --quiet`.nothrow()).exitCode === 0) break
        await $`cd ./dist/aur-${pkg} && git commit -m "Update to v${Script.version}"`
        await $`cd ./dist/aur-${pkg} && git push`
        break
      } catch {
        continue
      }
    }
  }

  // Homebrew formula
  const homebrewFormula = [
    "# typed: false",
    "# frozen_string_literal: true",
    "",
    "# This file was generated by GoReleaser. DO NOT EDIT.",
    "class Opencode < Formula",
    `  desc "The AI coding agent built for the terminal."`,
    `  homepage "https://github.com/anomalyco/opencode"`,
    `  version "${Script.version.split("-")[0]}"`,
    "",
    `  depends_on "ripgrep"`,
    "",
    "  on_macos do",
    "    if Hardware::CPU.intel?",
    `      url "https://github.com/anomalyco/opencode/releases/download/v${Script.version}/opencode-darwin-x64.zip"`,
    `      sha256 "${macX64Sha}"`,
    "",
    "      def install",
    '        bin.install "opencode"',
    "      end",
    "    end",
    "    if Hardware::CPU.arm?",
    `      url "https://github.com/anomalyco/opencode/releases/download/v${Script.version}/opencode-darwin-arm64.zip"`,
    `      sha256 "${macArm64Sha}"`,
    "",
    "      def install",
    '        bin.install "opencode"',
    "      end",
    "    end",
    "  end",
    "",
    "  on_linux do",
    "    if Hardware::CPU.intel? and Hardware::CPU.is_64_bit?",
    `      url "https://github.com/anomalyco/opencode/releases/download/v${Script.version}/opencode-linux-x64.tar.gz"`,
    `      sha256 "${x64Sha}"`,
    "      def install",
    '        bin.install "opencode"',
    "      end",
    "    end",
    "    if Hardware::CPU.arm? and Hardware::CPU.is_64_bit?",
    `      url "https://github.com/anomalyco/opencode/releases/download/v${Script.version}/opencode-linux-arm64.tar.gz"`,
    `      sha256 "${arm64Sha}"`,
    "      def install",
    '        bin.install "opencode"',
    "      end",
    "    end",
    "  end",
    "end",
    "",
    "",
  ].join("\n")

  const token = process.env.GITHUB_TOKEN
  if (!token) {
    console.error("GITHUB_TOKEN is required to update homebrew tap")
    process.exit(1)
  }
  const tap = `https://x-access-token:${token}@github.com/anomalyco/homebrew-tap.git`
  await $`rm -rf ./dist/homebrew-tap`
  await $`git clone ${tap} ./dist/homebrew-tap`
  await Bun.file("./dist/homebrew-tap/opencode.rb").write(homebrewFormula)
  await $`cd ./dist/homebrew-tap && git add opencode.rb`
  if ((await $`cd ./dist/homebrew-tap && git diff --cached --quiet`.nothrow()).exitCode !== 0) {
    await $`cd ./dist/homebrew-tap && git commit -m "Update to v${Script.version}"`
    await $`cd ./dist/homebrew-tap && git push`
  }
}
