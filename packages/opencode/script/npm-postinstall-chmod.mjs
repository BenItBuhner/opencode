#!/usr/bin/env node

import fs from "fs"
import os from "os"
import path from "path"
import { fileURLToPath } from "url"

if (os.platform() === "win32") process.exit(0)

const binDir = path.join(path.dirname(fileURLToPath(import.meta.url)), "bin")
if (!fs.existsSync(binDir)) process.exit(0)

for (const name of fs.readdirSync(binDir)) {
  const target = path.join(binDir, name)
  if (!fs.statSync(target).isFile()) continue
  try {
    fs.chmodSync(target, 0o755)
  } catch {
    // The launcher will surface a clearer error if execution still fails.
  }
}
