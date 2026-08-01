import { describe, expect, test } from "bun:test"
import { DESKTOP_MENU, type DesktopMenuItem } from "./desktop-menu"

describe("desktop menu", () => {
  test("uses the OpenGoal product name", () => {
    expect(DESKTOP_MENU.find((menu) => menu.id === "app")?.label).toBe("OpenGoal")
  })

  test("points product help and feedback at the OpenGoal fork", () => {
    const help = DESKTOP_MENU.find((menu) => menu.id === "help")?.items ?? []
    const item = (label: string) =>
      help.find((entry): entry is DesktopMenuItem => entry.type === "item" && entry.label === label)

    expect(item("OpenGoal Documentation")?.href).toContain("BenItBuhner/opengoal")
    expect(item("Share Feedback")?.href).toContain("BenItBuhner/opengoal")
    expect(item("Report a Bug")?.href).toContain("BenItBuhner/opengoal")
  })

  test("navigates between tabs", () => {
    const items = DESKTOP_MENU.flatMap((menu) => menu.items ?? []).filter(
      (item) => item.type === "item" && (item.label === "Previous Tab" || item.label === "Next Tab"),
    )

    expect(items).toEqual([
      { type: "item", label: "Previous Tab", command: "tab.prev", accelerator: { macos: "Option+Up" } },
      { type: "item", label: "Next Tab", command: "tab.next", accelerator: { macos: "Option+Down" } },
    ])
  })

  test("exports logs through the desktop command registry", () => {
    const items = DESKTOP_MENU.flatMap((menu) => menu.items ?? []).filter(
      (item) => item.type === "item" && item.label === "Export Logs...",
    )

    expect(items).toHaveLength(2)
    expect(items.every((item) => item.type === "item" && item.command === "logs.export" && !item.action)).toBe(true)
  })
})
