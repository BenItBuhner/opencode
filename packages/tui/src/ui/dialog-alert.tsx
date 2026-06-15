import { TextAttributes } from "@opentui/core"
import { useTerminalDimensions } from "@opentui/solid"
import { useTheme } from "../context/theme"
import { useDialog, type DialogContext } from "./dialog"
import { useBindings } from "../keymap"
import { getScrollAcceleration } from "../util/scroll"

export type DialogAlertProps = {
  title: string
  message: string
  onConfirm?: () => void
}

export function DialogAlert(props: DialogAlertProps) {
  const dialog = useDialog()
  const { theme } = useTheme()
  const dimensions = useTerminalDimensions()
  const scrollAcceleration = getScrollAcceleration()
  const bodyHeight = () => Math.max(3, dimensions().height - 12)

  useBindings(() => ({
    bindings: [
      {
        key: "return",
        desc: "Confirm alert",
        group: "Dialog",
        cmd: () => {
          props.onConfirm?.()
          dialog.clear()
        },
      },
    ],
  }))
  return (
    <box paddingLeft={2} paddingRight={2} gap={1}>
      <box flexDirection="row" justifyContent="space-between">
        <text attributes={TextAttributes.BOLD} fg={theme.text}>
          {props.title}
        </text>
        <text fg={theme.textMuted} onMouseUp={() => dialog.clear()}>
          esc
        </text>
      </box>
      <scrollbox
        paddingBottom={1}
        maxHeight={bodyHeight()}
        scrollbarOptions={{ visible: true }}
        scrollAcceleration={scrollAcceleration}
      >
        <text fg={theme.textMuted}>{props.message}</text>
      </scrollbox>
      <box flexDirection="row" justifyContent="flex-end" paddingBottom={1}>
        <box
          paddingLeft={3}
          paddingRight={3}
          backgroundColor={theme.primary}
          onMouseUp={() => {
            props.onConfirm?.()
            dialog.clear()
          }}
        >
          <text fg={theme.selectedListItemText}>ok</text>
        </box>
      </box>
    </box>
  )
}

DialogAlert.show = (dialog: DialogContext, title: string, message: string) => {
  return new Promise<void>((resolve) => {
    dialog.replace(
      () => <DialogAlert title={title} message={message} onConfirm={() => resolve()} />,
      () => resolve(),
    )
  })
}
