import { benchmark, expect } from "../benchmark"
import { expectSessionTitle } from "../../utils/waits"
import { measureNavigationMilestones } from "./navigation-milestones"
import { fixture } from "./session-timeline-stress.fixture"
import {
  createReviewDiffs,
  installStressSessionTabs,
  installTimelineSettings,
  mockStressTimeline,
  stressSessionHref,
} from "./timeline-test-helpers"
import { waitForStableTimeline } from "./session-tab-switch-probe"

const homeRow = '[data-component="home-session-row"]'
const homeShell = '[data-component="home-session-search"]'
const review = '#review-panel [data-component="file"][data-mode="diff"]'

benchmark.describe("performance: home and tab navigation", () => {
  benchmark("opens a home session and paints its titlebar tab", async ({ page, report }) => {
    await setup(page, [])
    await page.goto("/")
    const row = page.locator(homeRow).filter({ hasText: fixture.expected.targetTitle }).first()
    await expect(row).toBeVisible()
    const href = stressSessionHref(fixture.targetID)
    const result = await measureNavigationMilestones(page, {
      triggerSelector: homeRow,
      milestones: {
        content: { selector: messageSelector(fixture.expected.targetMessageIDs.at(-1)!) },
        tab: { selector: `[data-slot="titlebar-tabs"] a[href="${href}"]` },
      },
      navigate: async () => {
        await row.click()
        await expectSessionTitle(page, fixture.expected.targetTitle)
      },
    })
    report(result)
    await expect(page.locator(`[data-slot="titlebar-tabs"] a[href="${href}"]`)).toContainText(
      fixture.expected.targetTitle,
    )
  })

  benchmark("stages the review body after cold session content", async ({ page, report }) => {
    await setup(page, [], createReviewDiffs())
    await page.goto("/")
    const row = page.locator(homeRow).filter({ hasText: fixture.expected.targetTitle }).first()
    await expect(row).toBeVisible()
    const content = await page.evaluate(
      ({ rowSelector, title, contentSelector, reviewSelector }) =>
        new Promise<{ contentBeforeReview: boolean; samples: number }>((resolve) => {
          let samples = 0
          const sample = () => {
            samples++
            const content = !!document.querySelector(contentSelector)
            const reviewVisible = !!document.querySelector(reviewSelector)
            if (content && !reviewVisible) {
              resolve({ contentBeforeReview: true, samples })
              return
            }
            if (content && reviewVisible) {
              resolve({ contentBeforeReview: false, samples })
              return
            }
            requestAnimationFrame(sample)
          }
          const target = [...document.querySelectorAll<HTMLElement>(rowSelector)].find((item) =>
            item.textContent?.includes(title),
          )
          if (!target) throw new Error(`Home session row not found: ${title}`)
          target.click()
          requestAnimationFrame(sample)
        }),
      {
        rowSelector: homeRow,
        title: fixture.expected.targetTitle,
        contentSelector: messageSelector(fixture.expected.targetMessageIDs.at(-1)!),
        reviewSelector: review,
      },
    )
    expect(content.contentBeforeReview).toBe(true)
    const reviewResult = await measureNavigationMilestones(page, {
      triggerSelector: 'button[aria-label="Toggle review"]',
      milestones: { review: { selector: review } },
      navigate: async () => {
        await page.getByRole("button", { name: "Toggle review" }).click()
        await expect(page.locator(review)).toBeVisible()
      },
    })
    report({ content, review: reviewResult })
  })

  benchmark("closes the only session tab and paints home", async ({ page, report }) => {
    await setup(page, [fixture.sourceID])
    const href = stressSessionHref(fixture.sourceID)
    await page.goto(href)
    await expectSessionTitle(page, fixture.expected.sourceTitle)
    await waitForStableTimeline(page, fixture.expected.sourceMessageIDs.at(-1)!)
    const tab = page.locator(`[data-slot="titlebar-tabs"] a[href="${href}"]`).first()
    const close = tab.locator("..").locator('[data-component="icon-button-v2"]')
    await expect(close).toBeVisible()
    const result = await measureNavigationMilestones(page, {
      triggerSelector: '[data-slot="titlebar-tabs"] [data-component="icon-button-v2"]',
      milestones: {
        home: { selector: homeShell },
        row: { selector: homeRow },
        tabRemoved: { selector: `[data-slot="titlebar-tabs"] a[href="${href}"]`, visible: false },
      },
      navigate: async () => {
        await close.click()
        await expect(page).toHaveURL("/")
      },
    })
    report(result)
  })
})

async function setup(
  page: Parameters<typeof mockStressTimeline>[0],
  sessionIDs: string[],
  vcsDiff?: ReturnType<typeof createReviewDiffs>,
) {
  await mockStressTimeline(page, { vcsDiff })
  await installTimelineSettings(page)
  await installStressSessionTabs(page, { sessionIDs })
}

function messageSelector(id: string) {
  return `[data-message-id="${id}"]`
}
