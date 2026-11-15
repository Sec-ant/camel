// Authored 16px line icons (1.25px, mitred) for the tool strip.

const paths = {
  select: ["M3.5 2.5 12 8.2l-3.9.8 2.3 4.2-1.6.8-2.3-4.2-3 2.6z"],
  erase: ["M10.2 2.8 13.2 5.8 7 12H4v-3z", "M8.2 4.8l3 3", "M3 14.25h10"],
  interior: [
    "M8 2.5c3.3 0 5.5 1.9 5.5 4.3S11.3 11.1 8 11.1c-.7 0-1.4-.1-2-.3L3 13l.8-3.1C3 9.1 2.5 8 2.5 6.8 2.5 4.4 4.7 2.5 8 2.5z",
    "M6 6.8h4",
  ],
  rect: ["M2.5 5V2.5H5M11 2.5h2.5V5M13.5 11v2.5H11M5 13.5H2.5V11", "M8 5.5v5M5.5 8h5"],
  pick: [
    "m9.5 4 2.5 2.5",
    "M10.6 2.9a1.6 1.6 0 0 1 2.5 2.5L11.4 7.1 8.9 4.6z",
    "M8.9 5.4 3.6 10.7 3 13l2.3-.6 5.3-5.3",
  ],
  pan: [
    "M5.5 8.5V4a1 1 0 0 1 2 0v3.5M7.5 7V3a1 1 0 0 1 2 0v4M9.5 7.2V4a1 1 0 0 1 2 0v5c0 2.8-1.7 4.5-4.2 4.5-1.6 0-2.6-.7-3.4-1.9L2.6 9.4a1 1 0 0 1 1.6-1.1l1.3 1.3",
  ],
} as const;

export type IconName = keyof typeof paths;

export function Icon({ name }: { name: IconName }) {
  return (
    <svg
      class="icon"
      viewBox="0 0 16 16"
      width="16"
      height="16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.25"
      stroke-linecap="square"
      stroke-linejoin="miter"
      aria-hidden="true"
    >
      {paths[name].map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}
