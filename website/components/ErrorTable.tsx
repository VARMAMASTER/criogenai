// The error catalog (schemas/error-codes.json) as tables, per layer: the
// same file the Rust, Python and TypeScript tests check against.
import { CLASS, LAYERS, errorCatalog } from "@/lib/errors";

const retry = (r: boolean | null) => (r === true ? "yes" : r === false ? "no" : "depends");

export default function ErrorTable() {
  const entries = errorCatalog();
  return (
    <>
      {LAYERS.map(([layer, title]) => {
        const http = layer === "model" || layer === "gateway";
        return (
          <section key={layer}>
            <h2 id={`layer-${layer}`}>
              {title}
              <a className="anchor" href={`#layer-${layer}`} aria-label={`Link to this section: ${title}`} data-pagefind-ignore="">
                #
              </a>
            </h2>
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th>Code</th>
                    <th>Meaning</th>
                    <th>Retry?</th>
                    <th>SDK class</th>
                    {http ? <th>HTTP</th> : null}
                  </tr>
                </thead>
                <tbody>
                  {entries
                    .filter((e) => e.layers.includes(layer))
                    .map((e) => (
                      <tr key={e.code}>
                        <td>
                          <code id={`code-${e.code}`}>{e.code}</code>
                        </td>
                        <td>{e.description}</td>
                        <td>{retry(e.retryable)}</td>
                        <td>{e.family ? <code>{CLASS[e.family]}</code> : "–"}</td>
                        {http ? <td>{e.http_status ?? "–"}</td> : null}
                      </tr>
                    ))}
                </tbody>
              </table>
            </div>
          </section>
        );
      })}
    </>
  );
}
