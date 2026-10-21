// The data of an animated run (Scene.tsx, scenes.ts).
export type State = "idle" | "run" | "wait" | "done" | "fail" | "skip";
export type Tone = "run" | "wait" | "done" | "fail" | "note";
export interface SceneNode {
  id: string;
  label: string;
  x: number;
  y: number;
  kind?: "model" | "tool" | "person" | "client" | "gateway" | "provider";
  /** State in the final frame (default done). */
  final?: State;
}
export interface SceneEdge {
  id: string;
  from: string;
  to: string;
  final?: State;
}
export interface Beat {
  /** Milliseconds from the start (or from the decision, in a branch). */
  at: number;
  set?: Record<string, State>;
  log?: string;
  tone?: Tone;
  /** A full-scene flash, e.g. the process being killed. */
  flash?: string;
  /** Stop and ask the viewer. */
  ask?: { prompt: string; approve: Beat[]; reject: Beat[] };
}
