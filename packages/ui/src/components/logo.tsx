import { type ComponentProps } from "solid-js"

export const Mark = (props: { class?: string }) => {
  return (
    <svg
      data-component="logo-mark"
      data-brand="opengoal"
      data-glyph="g"
      classList={{ [props.class ?? ""]: !!props.class }}
      viewBox="0 0 24 30"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label="OpenGoal"
    >
      <title>OpenGoal</title>
      <path
        data-slot="logo-logo-mark-shadow"
        d="M18 24H6V6H18V12H12V18H18V24Z"
        fill="var(--icon-weak-base)"
      />
      <path
        data-slot="logo-logo-mark-g"
        d="M0 0H24V6H0V0ZM0 0H6V30H0V0ZM0 24H24V30H0V24ZM18 12H24V30H18V12ZM12 12H24V18H12V12Z"
        fill="var(--icon-strong-base)"
      />
    </svg>
  )
}

export const Splash = (props: Pick<ComponentProps<"svg">, "ref" | "class">) => {
  return (
    <svg
      ref={props.ref}
      data-component="logo-splash"
      data-brand="opengoal"
      data-glyph="g"
      classList={{ [props.class ?? ""]: !!props.class }}
      viewBox="0 0 80 100"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      role="img"
      aria-label="OpenGoal"
    >
      <title>OpenGoal</title>
      <path d="M60 80H20V20H60V40H40V60H60V80Z" fill="var(--icon-base)" />
      <path
        d="M0 0H80V20H0V0ZM0 0H20V100H0V0ZM0 80H80V100H0V80ZM60 40H80V100H60V40ZM40 40H80V60H40V40Z"
        fill="var(--icon-strong-base)"
      />
    </svg>
  )
}

export const Logo = (props: { class?: string }) => {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      data-component="logo-wordmark"
      data-brand="opengoal"
      data-wordmark="opengoal"
      viewBox="0 0 234 42"
      fill="none"
      classList={{ [props.class ?? ""]: !!props.class }}
      role="img"
      aria-label="OpenGoal"
    >
      <title>OpenGoal</title>
      <g>
        <path d="M18 30H6V18H18V30Z" fill="var(--icon-weak-base)" />
        <path d="M18 12H6V30H18V12ZM24 36H0V6H24V36Z" fill="var(--icon-base)" />
        <path d="M48 30H36V18H48V30Z" fill="var(--icon-weak-base)" />
        <path d="M36 30H48V12H36V30ZM54 36H36V42H30V6H54V36Z" fill="var(--icon-base)" />
        <path d="M84 24V30H66V24H84Z" fill="var(--icon-weak-base)" />
        <path d="M84 24H66V30H84V36H60V6H84V24ZM66 18H78V12H66V18Z" fill="var(--icon-base)" />
        <path d="M108 36H96V18H108V36Z" fill="var(--icon-weak-base)" />
        <path d="M108 12H96V36H90V6H108V12ZM114 36H108V12H114V36Z" fill="var(--icon-base)" />
        <path
          data-letter="g"
          d="M120 6H144V12H120V6ZM120 6H126V36H120V6ZM120 30H144V36H120V30ZM138 18H144V36H138V18ZM132 18H144V24H132V18Z"
          fill="var(--icon-strong-base)"
        />
        <path data-letter="o" d="M168 30H156V18H168V30Z" fill="var(--icon-weak-base)" />
        <path
          data-letter="o"
          d="M168 12H156V30H168V12ZM174 36H150V6H174V36Z"
          fill="var(--icon-strong-base)"
        />
        <path
          data-letter="a"
          d="M186 6H198V12H186V6ZM180 12H186V36H180V12ZM198 12H204V36H198V12ZM180 18H204V24H180V18Z"
          fill="var(--icon-strong-base)"
        />
        <path
          data-letter="l"
          d="M210 6H216V36H210V6ZM210 30H234V36H210V30Z"
          fill="var(--icon-strong-base)"
        />
      </g>
    </svg>
  )
}
