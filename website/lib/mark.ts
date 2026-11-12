// The CriogenAI mark: a rocket whose body is a hexagonal ice-crystal cell
// (cryogenic fuel, the name's root), an amber window that is a run's core,
// and an amber flame. Line duotone: green outline over a faint green fill.
// One source for the header logo, the watermark, the favicon and the card.
export const MARK = {
  viewBox: "0 0 32 32",
  body: "M16 2.5 L21.5 8.5 V22 L16 25 L10.5 22 V8.5 Z",
  fins: "M10.5 16 L6.5 20.5 V24.5 L10.5 22.5 M21.5 16 L25.5 20.5 V24.5 L21.5 22.5",
  flame: "M12.4 24.4 C12.8 27 14.4 28.4 16 31.2 C17.6 28.4 19.2 27 19.6 24.4",
  /** The hot inner flame, drawn filled inside the outer one. */
  flameCore: "M14.4 24.6 C14.7 26.5 15.3 27.6 16 29.2 C16.7 27.6 17.3 26.5 17.6 24.6 Z",
  core: { cx: 16, cy: 12.5, r: 2.7 },
  green: "#3ecf8e",
  amber: "#f5b544",
} as const;
