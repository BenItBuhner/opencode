import { describe, expect, test } from "bun:test"
import { go, logo, marks } from "../src/logo"

const allowed = new Set([...marks, " ", "█", "▀", "▄"])

const WORDMARK_WIDTH = 19
const GO_WIDTH = 4

function expectLogoShape(shape: { left: string[]; right: string[] }, width: number) {
  expect(shape.left.length).toBe(shape.right.length)
  for (let i = 0; i < shape.left.length; i++) {
    expect(shape.left[i].length).toBe(shape.right[i].length)
    expect(shape.left[i].length).toBe(width)
    for (const line of [shape.left[i], shape.right[i]]) {
      for (const char of line) {
        expect(allowed.has(char)).toBeTrue()
      }
    }
  }
}

describe("logo", () => {
  test("opengoal wordmark rows stay aligned", () => {
    expectLogoShape(logo, WORDMARK_WIDTH)
  })

  test("go wordmark rows stay aligned", () => {
    expectLogoShape(go, GO_WIDTH)
  })

  test("goal side uses four four-wide glyphs on letter rows", () => {
    for (const line of logo.right.slice(1)) {
      expect(line.length).toBe(WORDMARK_WIDTH)
      expect(line.split(" ").length).toBe(4)
      for (const block of line.split(" ")) {
        expect(block.length).toBe(4)
      }
    }
  })

  test("goal side reuses the go glyph for G and standard blocks for O", () => {
    const blocks = (line: string) => line.split(" ")
    expect(blocks(logo.right[1])[0]).toBe(go.left[1])
    expect(blocks(logo.right[2])[0]).toBe(go.left[2])
    expect(blocks(logo.right[3])[0]).toBe(go.left[3])
    expect(blocks(logo.right[1])[1]).toBe("█▀▀█")
    expect(blocks(logo.right[2])[1]).toBe("█__█")
    expect(blocks(logo.right[3])[1]).toBe("▀▀▀▀")
  })

  test("goal a uses a lowercase bowl and right stem", () => {
    const blocks = (line: string) => line.split(" ")
    expect(logo.right[0]).toBe(" ".repeat(WORDMARK_WIDTH))
    expect(blocks(logo.right[1])[2]).toBe("▄▀▀█")
    expect(blocks(logo.right[2])[2]).toBe("█__█")
    expect(blocks(logo.right[3])[2]).toBe(".▀▀▀")
  })

  test("goal L top shadow starts below the cap", () => {
    const blocks = (line: string) => line.split(" ")
    expect(blocks(logo.right[1])[3]).toBe("█...")
    expect(blocks(logo.right[2])[3]).toBe("█___")
    expect(blocks(logo.right[3])[3]).toBe("▀▀▀▀")
  })
})
