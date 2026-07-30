import type { TuiPlugin, TuiPluginApi } from "@opencode-ai/plugin/tui"
import type { BuiltinTuiPlugin } from "../builtins"
import { Show } from "solid-js"
import { DialogAlert } from "../../ui/dialog-alert"
import { DialogGoalSummaries } from "../../component/dialog-goal-summaries"
import { createGoalElapsed, createGoalStatus, goalDetailsMessage, GoalSidebarStatus } from "../../component/goal-status"

const id = "internal:sidebar-goal"

function View(props: { api: TuiPluginApi; sessionID: string }) {
  const goal = createGoalStatus({
    sessionID: () => props.sessionID,
    goal: (sessionID) => props.api.state.session.get(sessionID)?.metadata?.goal,
    messages: (sessionID) => props.api.state.session.messages(sessionID),
    status: (sessionID) => props.api.state.session.status(sessionID),
  })
  const elapsed = createGoalElapsed(goal)

  const openDetails = () => {
    if (props.api.renderer.getSelection()?.getSelectedText()) return
    const item = goal()
    if (!item) return
    props.api.ui.dialog.replace(() => (
      <DialogAlert title="Goal Details" message={goalDetailsMessage(item, elapsed())} />
    ))
  }

  const openSummaries = () => {
    if (props.api.renderer.getSelection()?.getSelectedText()) return
    const item = goal()
    if (!item) return
    props.api.ui.dialog.replace(() => <DialogGoalSummaries goal={item} />)
  }

  return (
    <Show when={goal()}>
      {(item) => (
        <GoalSidebarStatus goal={item()} elapsed={elapsed()} onDetails={openDetails} onSummaries={openSummaries} />
      )}
    </Show>
  )
}

const tui: TuiPlugin = async (api) => {
  api.slots.register({
    order: 90,
    slots: {
      sidebar_content(_ctx, props) {
        return <View api={api} sessionID={props.session_id} />
      },
    },
  })
}

const plugin: BuiltinTuiPlugin = {
  id,
  tui,
}

export default plugin
