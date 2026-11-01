"use client";
// Load a results file from the visitor's machine. It is read and checked in
// the browser; nothing is uploaded anywhere.
import { useRef, useState, type DragEvent } from "react";
import { validate, type BenchFile } from "@/lib/bench";

const MAX_BYTES = 20 * 1024 * 1024;

export default function DropZone({ onLoad, loaded, onClear }: { onLoad: (data: BenchFile, name: string) => void; loaded: string | null; onClear: () => void }) {
  const input = useRef<HTMLInputElement>(null);
  const [over, setOver] = useState(false);
  const [errors, setErrors] = useState<string[]>([]);
  const [warnings, setWarnings] = useState<string[]>([]);

  const read = async (file: File | undefined) => {
    if (!file) return;
    setErrors([]);
    setWarnings([]);
    if (file.size > MAX_BYTES) return setErrors([`${file.name} is larger than 20 MB; a results file is much smaller.`]);
    let raw: unknown;
    try {
      raw = JSON.parse(await file.text());
    } catch (e) {
      return setErrors([`${file.name} is not valid JSON: ${(e as Error).message}`]);
    }
    const v = validate(raw);
    if (!v.ok) return setErrors(v.errors);
    setWarnings(v.warnings);
    onLoad(v.data, file.name);
  };

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setOver(false);
    void read(e.dataTransfer.files[0]);
  };

  return (
    <div className="drop-area">
      <div
        className={`drop${over ? " over" : ""}`}
        onDragOver={(e) => {
          e.preventDefault();
          setOver(true);
        }}
        onDragLeave={() => setOver(false)}
        onDrop={onDrop}
      >
        <p className="drop-title">Drop your results file here</p>
        <p className="lab-muted">
          A <code>.json</code> file written by <code>run.py</code> (schema <code>criogen-compare/1</code>). It is read and checked in your
          browser; nothing is uploaded.
        </p>
        <button type="button" className="lab-button" onClick={() => input.current?.click()}>
          Choose a file
        </button>
        <input
          ref={input}
          type="file"
          accept=".json,application/json"
          className="sr-only"
          tabIndex={-1}
          aria-hidden="true"
          onChange={(e) => {
            void read(e.target.files?.[0]);
            e.target.value = "";
          }}
        />
      </div>
      <div aria-live="polite">
        {loaded && (
          <p className="drop-ok">
            Showing <b>{loaded}</b> beside the recorded results, marked “your machine”.{" "}
            <button type="button" className="link-button" onClick={onClear}>
              Remove it
            </button>
          </p>
        )}
        {errors.length > 0 && (
          <div className="drop-errors" role="alert">
            <p>This file cannot be shown:</p>
            <ul>
              {errors.map((e) => (
                <li key={e}>{e}</li>
              ))}
            </ul>
          </div>
        )}
        {warnings.length > 0 && (
          <ul className="drop-warnings">
            {warnings.map((w) => (
              <li key={w}>{w}</li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
