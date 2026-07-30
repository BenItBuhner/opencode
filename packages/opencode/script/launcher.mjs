#!/usr/bin/env node

import childProcess from "child_process"
import fs from "fs"
import os from "os"
import path from "path"
import { createRequire } from "module"
import { fileURLToPath } from "url"

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const require = createRequire(import.meta.url)

const platformMap = {
  darwin: "darwin",
  linux: "linux",
  win32: "windows",
}
const archMap = {
  x64: "x64",
  arm64: "arm64",
  arm: "arm",
}

const platform = platformMap[os.platform()] ?? os.platform()
const arch = archMap[os.arch()] ?? os.arch()
const base = `@benitbuhner/opengoal-${platform}-${arch}`
const sourceBinary = platform === "windows" ? "opengoal.exe" : "opengoal"

function supportsAvx2() {
  if (arch !== "x64") return false

  if (platform === "linux") {
    try {
      return /(^|\s)avx2(\s|$)/i.test(fs.readFileSync("/proc/cpuinfo", "utf8"))
    } catch {
      return false
    }
  }

  if (platform === "darwin") {
    try {
      const result = childProcess.spawnSync("sysctl", ["-n", "hw.optional.avx2_0"], {
        encoding: "utf8",
        timeout: 1500,
      })
      if (result.status !== 0) return false
      return (result.stdout || "").trim() === "1"
    } catch {
      return false
    }
  }

  if (platform === "windows") {
    const command =
      '(Add-Type -MemberDefinition "[DllImport(""kernel32.dll"")] public static extern bool IsProcessorFeaturePresent(int ProcessorFeature);" -Name Kernel32 -Namespace Win32 -PassThru)::IsProcessorFeaturePresent(40)'

    for (const executable of ["powershell.exe", "pwsh.exe", "pwsh", "powershell"]) {
      try {
        const result = childProcess.spawnSync(executable, ["-NoProfile", "-NonInteractive", "-Command", command], {
          encoding: "utf8",
          timeout: 3000,
          windowsHide: true,
        })
        if (result.status !== 0) continue
        const output = (result.stdout || "").trim().toLowerCase()
        if (output === "true" || output === "1") return true
        if (output === "false" || output === "0") return false
      } catch {
        continue
      }
    }
  }

  return false
}

function isMusl() {
  if (platform !== "linux") return false

  try {
    if (fs.existsSync("/etc/alpine-release")) return true
  } catch {
    // Ignore filesystem probes that are blocked by the host.
  }

  try {
    const result = childProcess.spawnSync("ldd", ["--version"], { encoding: "utf8" })
    return `${result.stdout || ""}${result.stderr || ""}`.toLowerCase().includes("musl")
  } catch {
    return false
  }
}

function packageNames() {
  const baseline = arch === "x64" && !supportsAvx2()

  if (platform === "linux") {
    if (isMusl()) {
      if (arch === "x64")
        return baseline
          ? [`${base}-baseline-musl`, `${base}-musl`, `${base}-baseline`, base]
          : [`${base}-musl`, `${base}-baseline-musl`, base, `${base}-baseline`]
      return [`${base}-musl`, base]
    }

    if (arch === "x64")
      return baseline
        ? [`${base}-baseline`, base, `${base}-baseline-musl`, `${base}-musl`]
        : [base, `${base}-baseline`, `${base}-musl`, `${base}-baseline-musl`]
    return [base, `${base}-musl`]
  }

  if (arch === "x64") return baseline ? [`${base}-baseline`, base] : [base, `${base}-baseline`]
  return [base]
}

function resolveBinary(name) {
  const packageJsonPath = require.resolve(`${name}/package.json`)
  const binaryPath = path.join(path.dirname(packageJsonPath), "bin", sourceBinary)
  if (!fs.existsSync(binaryPath)) throw new Error(`Binary not found at ${binaryPath}`)
  if (platform !== "windows") {
    try {
      fs.chmodSync(binaryPath, 0o755)
    } catch {
      // The following spawn will surface a clearer platform error if chmod is not permitted.
    }
  }
  return binaryPath
}

function findBinary() {
  for (const name of packageNames()) {
    try {
      return resolveBinary(name)
    } catch {
      // Try the next compatible package name.
    }
  }

  throw new Error(
    `Could not find the opengoal binary package. Try reinstalling, or manually install ${packageNames()
      .map((name) => JSON.stringify(name))
      .join(" or ")}.`,
  )
}

const binary = findBinary()
const result = childProcess.spawnSync(binary, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: false,
})

if (result.error) throw result.error
if (typeof result.status === "number") process.exit(result.status)
if (result.signal) {
  process.kill(process.pid, result.signal)
}
