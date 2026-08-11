import { describe, expect, test } from "bun:test"
import { formatQuestionTimeout } from "./session-question-dock"

describe("formatQuestionTimeout", () => {
  test("formats seconds and minute countdowns", () => {
    expect(formatQuestionTimeout(9.2)).toBe("10s")
    expect(formatQuestionTimeout(60)).toBe("1:00")
    expect(formatQuestionTimeout(125)).toBe("2:05")
  })

  test("clamps elapsed time", () => {
    expect(formatQuestionTimeout(-1)).toBe("0s")
  })
})
