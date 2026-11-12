// Installs so far, shown on the home page.
// Read at build time from the public download statistics: npm's
// downloads API and pypistats.org (without mirrors), for both names: the
// packages were called `criogen` until 0.1.6 and `criogenai` from 0.1.7;
// installs of either count (platform addons are not counted again).
//
// Limit: pypistats keeps the last 180 days only. Before the first release
// is that old, switch PyPI to an all-time source (pepy.tech's API or
// Google BigQuery's PyPI dataset).

const FIRST_DAY = "2026-10-01";
/** Every name the packages have had. */
const NAMES = ["criogen", "criogenai"];

export type Installs =
  | { status: "counted"; total: number; npm: number; pypi: number; asOf: string }
  | { status: "unpublished"; asOf: string }
  | { status: "unavailable"; asOf: string };

type Count = number | "missing";

async function getJson(url: string): Promise<{ status: number; body: unknown }> {
  const response = await fetch(url, { signal: AbortSignal.timeout(8000), headers: { accept: "application/json" } });
  const body = response.status === 200 ? await response.json() : null;
  return { status: response.status, body };
}

const day = (d: Date) => d.toISOString().slice(0, 10);

async function npmDownloadsOf(NPM: string): Promise<Count> {
  // The range API allows at most 18 months per request.
  let total = 0;
  let from = new Date(`${FIRST_DAY}T00:00:00Z`);
  const today = new Date();
  while (from <= today) {
    const to = new Date(from);
    to.setUTCMonth(to.getUTCMonth() + 17);
    const end = to < today ? to : today;
    const { status, body } = await getJson(`https://api.npmjs.org/downloads/range/${day(from)}:${day(end)}/${NPM}`);
    if (status === 404) return "missing";
    if (status !== 200) throw new Error(`npm downloads API: HTTP ${status}`);
    for (const d of (body as { downloads?: { downloads: number }[] }).downloads ?? []) total += d.downloads;
    from = new Date(end);
    from.setUTCDate(from.getUTCDate() + 1);
  }
  return total;
}

async function pypiDownloadsOf(PYPI: string): Promise<Count> {
  const { status, body } = await getJson(`https://pypistats.org/api/packages/${PYPI}/overall?mirrors=false`);
  if (status === 404) return "missing";
  if (status !== 200) throw new Error(`pypistats: HTTP ${status}`);
  const rows = (body as { data?: { category: string; downloads: number }[] }).data ?? [];
  return rows.filter((r) => r.category === "without_mirrors").reduce((sum, r) => sum + r.downloads, 0);
}

/** The sum over all names; "missing" only when no name was ever published. */
async function sum(get: (name: string) => Promise<Count>): Promise<Count> {
  const counts = await Promise.all(NAMES.map(get));
  const found = counts.filter((c): c is number => c !== "missing");
  return found.length ? found.reduce((a, b) => a + b, 0) : "missing";
}

const npmDownloads = () => sum(npmDownloadsOf);
const pypiDownloads = () => sum(pypiDownloadsOf);

let cached: Promise<Installs> | null = null;

/** Fetched once per build; a network failure never fails the build. */
export function installs(): Promise<Installs> {
  cached ??= (async (): Promise<Installs> => {
    const asOf = day(new Date());
    try {
      const [npm, pypi] = await Promise.all([npmDownloads(), pypiDownloads()]);
      if (npm === "missing" && pypi === "missing") return { status: "unpublished", asOf };
      const n = npm === "missing" ? 0 : npm;
      const p = pypi === "missing" ? 0 : pypi;
      return { status: "counted", total: n + p, npm: n, pypi: p, asOf };
    } catch {
      return { status: "unavailable", asOf };
    }
  })();
  return cached;
}
