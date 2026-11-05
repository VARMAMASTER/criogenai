// The Criogen mark: a rocket whose body is a hexagonal ice-crystal cell
// (cryogenic fuel, the name's root), an amber window that is a run's core,
// and an amber flame. Line duotone: green outline over a faint green fill.
// One source for the header logo, the watermark, the favicon and the card.
export const MARK = {
  viewBox: "0 0 32 32",
  body: "M16 2.5 L21.5 8.5 V22 L16 25 L10.5 22 V8.5 Z",
  fins: "M10.5 16 L6.5 20.5 V24.5 L10.5 22.5 M21.5 16 L25.5 20.5 V24.5 L21.5 22.5",
  flame: "M12.6 25 L16 30 L19.4 25",
  core: { cx: 16, cy: 12.5, r: 2.7 },
  green: "#3ecf8e",
  amber: "#f5b544",
} as const;
