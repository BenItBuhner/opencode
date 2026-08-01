import { createUniqueId, type ComponentProps } from "solid-js"

export function WordmarkV2(props: Pick<ComponentProps<"svg">, "class">) {
  const mask = createUniqueId()
  const maskGradient = createUniqueId()

  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      data-component="wordmark-v2"
      data-brand="opengoal"
      data-wordmark="opengoal"
      viewBox="0 0 720 129"
      fill="none"
      classList={{ [props.class ?? ""]: !!props.class }}
      role="img"
      aria-label="OpenGoal"
    >
      <title>OpenGoal</title>
      <g opacity="0.6">
        <g mask={`url(#${mask})`}>
          <g opacity="0.16">
            <path
              data-letter="o"
              opacity="0.7"
              d="M55.3846 36.4286H18.4615V91.7143H55.3846V36.4286ZM73.8462 110.143H0V18H73.8462V110.143Z"
              fill="currentColor"
            />
            <path
              data-letter="p"
              opacity="0.7"
              d="M110.462 91.7143H147.385V36.4286H110.462V91.7143ZM165.846 110.143H110.462V128.571H92V18H165.846V110.143Z"
              fill="currentColor"
            />
            <path
              data-letter="e"
              opacity="0.7"
              d="M258.846 73.2857H203.462V91.7143H258.846V110.143H185V18H258.846V73.2857ZM203.462 54.8571H240.385V36.4286H203.462V54.8571Z"
              fill="currentColor"
            />
            <path
              data-letter="n"
              opacity="0.7"
              d="M332.385 36.4286H295.462V110.143H277V18H332.385V36.4286ZM350.846 110.143H332.385V36.4286H350.846V110.143Z"
              fill="currentColor"
            />
            <path
              data-letter="g"
              d="M369 18H442.846V36.4286H369V18ZM369 18H387.462V110.143H369V18ZM369 91.7143H442.846V110.143H369V91.7143ZM424.385 54.8571H442.846V110.143H424.385V54.8571ZM405.923 54.8571H442.846V73.2857H405.923V54.8571Z"
              fill="currentColor"
            />
            <path
              data-letter="o"
              d="M517.385 36.4286H480.462V91.7143H517.385V36.4286ZM535.846 110.143H462V18H535.846V110.143Z"
              fill="currentColor"
            />
            <path
              data-letter="a"
              d="M572.462 18H609.385V36.4286H572.462V18ZM554 36.4286H572.462V110.143H554V36.4286ZM609.385 36.4286H627.846V110.143H609.385V36.4286ZM554 54.8571H627.846V73.2857H554V54.8571Z"
              fill="currentColor"
            />
            <path
              data-letter="l"
              d="M646 18H664.462V110.143H646V18ZM646 91.7143H719.846V110.143H646V91.7143Z"
              fill="currentColor"
            />
          </g>
        </g>
      </g>
      <defs>
        <mask id={mask} style="mask-type:alpha" maskUnits="userSpaceOnUse" x="0" y="0" width="720" height="129">
          <rect width="720" height="129" fill={`url(#${maskGradient})`} />
        </mask>
        <linearGradient id={maskGradient} x1="360" y1="68" x2="360" y2="129" gradientUnits="userSpaceOnUse">
          <stop stop-color="white" stop-opacity="0.7" />
          <stop offset="1" stop-color="white" stop-opacity="0" />
        </linearGradient>
      </defs>
    </svg>
  )
}
