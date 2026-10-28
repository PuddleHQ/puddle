// SPDX-License-Identifier: GPL-3.0-or-later
import { join, resolve } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { main, render } from "./licences.ts";

/** An in-memory file system keyed by absolute path; a directory exists when a file lies below it. */
const { files, norm } = vi.hoisted(() => ({
  files: new Map<string, string>(),
  norm: (p: string) => p.split("\\").join("/"),
}));
const put = (p: string, text: string) => void files.set(norm(p), text);
const has = (p: string) => files.has(norm(p));
const get = (p: string) => files.get(norm(p));
vi.mock("node:fs", () => ({
  existsSync: (p: string) =>
    has(p) || [...files.keys()].some((f) => f.startsWith(`${norm(p)}/`)),
  readdirSync: (path: string) => {
    const dir = norm(path);
    return [...files.keys()]
      .filter(
        (f) =>
          f.startsWith(`${dir}/`) && !f.slice(dir.length + 1).includes("/"),
      )
      .map((f) => f.slice(dir.length + 1));
  },
  readFileSync: (p: string) => {
    const text = get(p);
    if (text === undefined) throw new Error(`ENOENT ${p}`);
    return text;
  },
  writeFileSync: (p: string, text: string) => void put(p, text),
}));

const root = resolve(import.meta.dirname, "..");
const SHIPPED = join(root, ".svelte-kit", "shipped-packages.json");
const NOTICES = join(root, "THIRD-PARTY-NOTICES.txt");
const OVERRIDES = join(root, "licence-overrides.json");

const addPackage = (
  dir: string,
  manifest: object,
  licenceFile = "MIT text",
) => {
  put(join(dir, "package.json"), JSON.stringify(manifest));
  if (licenceFile) put(join(dir, "LICENSE"), licenceFile);
};

describe("ui-licences main", () => {
  let out: string[];
  let err: string[];

  beforeEach(() => {
    files.clear();
    put(OVERRIDES, JSON.stringify({ overrides: {} }));
    out = [];
    err = [];
    vi.spyOn(console, "log").mockImplementation(
      (m: string) => void out.push(m),
    );
    vi.spyOn(console, "error").mockImplementation(
      (m: string) => void err.push(m),
    );
  });
  afterEach(() => vi.restoreAllMocks());

  const ship = (...dirs: string[]) => put(SHIPPED, JSON.stringify(dirs));

  it("tells you to build first when the bundle's package list is missing", () => {
    expect(main([])).toBe(1);
    expect(err).toEqual([
      "ui-licences: run `npm run build` first (it lists the packages in the bundle)",
    ]);
  });

  it("lists every problem and fails without writing the notices", () => {
    addPackage("/p/a", {
      name: "a",
      version: "1.0.0",
      license: "GPL-3.0-only",
    });
    addPackage("/p/b", { name: "b", version: "2.0.0" }, "");
    ship("/p/a", "/p/b", "/p/missing");
    expect(main([])).toBe(1);
    expect(err).toEqual([
      "ui-licences: 4 problem(s)\n" +
        "  a@1.0.0: licence GPL-3.0-only is not on the allowlist\n" +
        "  b@2.0.0: no licence declared\n" +
        "  b@2.0.0: no licence file to include in the notices\n" +
        "  /p/missing: no package.json",
    ]);
    expect(has(NOTICES)).toBe(false);
  });

  it("takes a missing licence from the overrides file", () => {
    put(
      OVERRIDES,
      JSON.stringify({ overrides: { "b@2.0.0": { licence: "MIT" } } }),
    );
    addPackage("/p/b", { name: "b", version: "2.0.0" });
    ship("/p/b");
    expect(main([])).toBe(0);
    expect(get(NOTICES)).toContain("b 2.0.0 (MIT)");
  });

  it("writes the notices and says how many packages", () => {
    addPackage("/p/a", { name: "a", version: "1.0.0", license: "MIT" });
    ship("/p/a");
    expect(main([])).toBe(0);
    expect(out).toEqual([
      "ui-licences: wrote THIRD-PARTY-NOTICES.txt (1 packages)",
    ]);
    expect(get(NOTICES)).toBe(
      render([
        {
          name: "a",
          version: "1.0.0",
          licence: "MIT",
          text: "MIT text",
          repository: "",
        },
      ]),
    );
  });

  it("--check passes when the notices are current, whatever their line endings", () => {
    addPackage("/p/a", { name: "a", version: "1.0.0", license: "MIT" });
    ship("/p/a");
    main([]);
    put(NOTICES, (get(NOTICES) ?? "").replaceAll("\n", "\r\n"));
    out.length = 0;
    expect(main(["--check"])).toBe(0);
    expect(out).toEqual([
      "ui-licences ok: 1 shipped packages, notices current",
    ]);
  });

  it("--check fails when the notices are out of date and leaves them alone", () => {
    addPackage("/p/a", { name: "a", version: "1.0.0", license: "MIT" });
    ship("/p/a");
    put(NOTICES, "old\n");
    expect(main(["--check"])).toBe(1);
    expect(err).toEqual([
      "ui-licences: THIRD-PARTY-NOTICES.txt is stale; run `npm run licences` and commit it",
    ]);
    expect(get(NOTICES)).toBe("old\n");
  });

  it("--check fails when the notices file does not exist", () => {
    addPackage("/p/a", { name: "a", version: "1.0.0", license: "MIT" });
    ship("/p/a");
    expect(main(["--check"])).toBe(1);
    expect(err).toHaveLength(1);
  });
});
