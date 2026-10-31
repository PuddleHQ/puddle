// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  checkRows,
  clashes,
  directoryFor,
  directoryOf,
  directoryProblem,
  folderOf,
  freeDirectory,
  cleanPath,
  localPathProblem,
  MAX_REPOSITORIES,
  newRow,
  refusedRow,
  rowProblems,
  sameRepositoryKey,
  startFolderProblem,
} from "./repo-rows.ts";

const row = (key: number, url: string, directory = "") => ({
  ...newRow(key, url),
  directory,
});

describe("directoryOf", () => {
  it.each([
    ["https://github.com/acme/api.git", "api"],
    ["https://github.com/acme/api", "api"],
    ["https://github.com/acme/api/", "api"],
    ["https://github.com/acme/api.git//", "api"],
    ["git@github.com:acme/web", "web"],
    ["https://example.org/", "example.org"],
    ["https://h/x/..git", "src"],
    ["https://h/x/.hidden", "src"],
    ["https://h/x/-dash", "src"],
    ["https://h/x/lost+found", "src"],
    ["https://h/x/ünï", "src"],
    ["", "src"],
    [`https://h/x/${"a".repeat(101)}`, "src"],
  ])("%j is cloned into %s", (url, dir) => {
    expect(directoryOf(url)).toBe(dir);
  });
});

describe("directoryProblem", () => {
  it("accepts what git makes and refuses what the service refuses", () => {
    expect(directoryProblem("api-2_v1.0")).toBeNull();
    expect(directoryProblem("a".repeat(100))).toBeNull();
    expect(directoryProblem("")).toMatch(/Give the directory/);
    expect(directoryProblem("a".repeat(101))).toMatch(/at most 100/);
    expect(directoryProblem(".git")).toMatch(/dot or a hyphen/);
    expect(directoryProblem("-x")).toMatch(/dot or a hyphen/);
    expect(directoryProblem("lost+found")).toMatch(/lost\+found/);
    expect(directoryProblem("a/b")).toMatch(/ASCII letters/);
    expect(directoryProblem("a b")).toMatch(/ASCII letters/);
  });
});

describe("directoryFor and freeDirectory", () => {
  it("takes what was typed, else what the address gives", () => {
    expect(directoryFor({ url: "https://h/o/api.git", directory: "" })).toBe(
      "api",
    );
    expect(directoryFor({ url: "https://h/o/api.git", directory: " x " })).toBe(
      "x",
    );
  });

  it("finds the first free suffix and keeps the name valid at its length", () => {
    expect(freeDirectory("api", ["api"])).toBe("api-2");
    expect(freeDirectory("api", ["api", "api-2", "api-3"])).toBe("api-4");
    const long = "a".repeat(100);
    expect(freeDirectory(long, [long])).toBe(`${"a".repeat(98)}-2`);
  });
});

describe("sameRepositoryKey", () => {
  it("reads two spellings of one repository as one", () => {
    const key = sameRepositoryKey("https://GitHub.com/Acme/API.git");
    expect(key).toBe("github.com/acme/api");
    expect(sameRepositoryKey("https://github.com/acme/api/")).toBe(key);
  });
  it("has none for what is not an https address", () => {
    expect(sameRepositoryKey("git@github.com:a/b")).toBeNull();
    expect(sameRepositoryKey("http://github.com/a/b")).toBeNull();
    expect(sameRepositoryKey("nonsense")).toBeNull();
  });
});

describe("clashes", () => {
  it("flags two rows with one directory, each naming the other and a free name", () => {
    const found = clashes([
      row(1, "https://github.com/a/api.git"),
      row(2, "https://gitlab.com/b/api.git"),
    ]);
    expect(found.get(1)?.directory).toBe(
      "Repository 2, the later one, also goes into api. Use another directory, such as api-2.",
    );
    expect(found.get(2)?.directory).toContain("Repository 1, the earlier one");
  });

  it("counts a typed directory, and lets a blank row alone", () => {
    expect(
      clashes([row(1, "https://h/a/x.git", "same"), row(2, "", "same")]).size,
    ).toBe(2);
    expect(clashes([row(1, "https://h/a/api.git"), row(2, "")]).size).toBe(0);
    expect(clashes([row(1, ""), row(2, "")]).size).toBe(0);
  });

  it("flags the later of two spellings of one repository", () => {
    const found = clashes([
      row(1, "https://github.com/a/api.git", "one"),
      row(2, "https://github.com/A/API", "two"),
    ]);
    expect(found.get(1)).toBeUndefined();
    expect(found.get(2)?.url).toBe(
      "This is the same repository as repository 1. List it once.",
    );
  });
});

describe("checkRows", () => {
  it("is empty for a list that can be sent, and for no rows", () => {
    expect(checkRows([]).size).toBe(0);
    expect(
      checkRows([
        row(1, "https://github.com/a/api.git"),
        row(2, "https://github.com/a/web.git", "front"),
      ]).size,
    ).toBe(0);
  });

  it("names a bad address and a bad typed directory, and both clash and bad together", () => {
    const found = checkRows([
      row(1, "git@github.com:a/api.git", "-x"),
      row(2, ""),
    ]);
    expect(found.get(1)?.url).toMatch(/SSH/);
    expect(found.get(1)?.directory).toMatch(/dot or a hyphen/);
    expect(found.get(2)?.url).toMatch(/Enter the repository/);
  });

  it("refuses more rows than a workspace holds, at the last", () => {
    const rows = Array.from({ length: MAX_REPOSITORIES + 1 }, (_, i) =>
      row(i + 1, `https://github.com/a/r${i}.git`),
    );
    expect(checkRows(rows).get(rows.length)?.url).toMatch(/at most 100/);
  });
});

describe("rowProblems", () => {
  it("leaves out what two rows share, which clashes reports", () => {
    const rows = [
      row(1, "https://github.com/a/api.git"),
      row(2, "https://gitlab.com/b/api.git"),
    ];
    expect(rowProblems(rows).size).toBe(0);
    expect(checkRows(rows).size).toBe(2);
  });
});

describe("startFolderProblem", () => {
  it("accepts nothing (the root) and a relative path", () => {
    expect(startFolderProblem("")).toBeNull();
    expect(startFolderProblem("   ")).toBeNull();
    expect(startFolderProblem("api")).toBeNull();
    expect(startFolderProblem(" api/src/ü ")).toBeNull();
    expect(startFolderProblem("a/.puddle")).toBeNull();
  });

  it.each([
    ["/abs", /leading \//],
    ["a//b", /empty/],
    ["a/", /empty/],
    ["a/../b", /\.\./],
    ["./a", /\.\./],
    ["a\\b", /backslash/],
    ["a\u0007b", /control/],
    [".puddle", /belongs to Puddle/],
    [".puddle/editor", /belongs to Puddle/],
    ["lost+found/x", /belongs to Puddle/],
    ["é".repeat(128), /at most 255 bytes/],
  ])("refuses %j", (text, reason) => {
    expect(startFolderProblem(text)).toMatch(reason);
  });

  it("allows exactly 255 bytes", () => {
    expect(startFolderProblem("a".repeat(255))).toBeNull();
    expect(startFolderProblem("a".repeat(256))).toMatch(/255 bytes/);
  });
});

describe("refusedRow", () => {
  it("finds the row a refusal names", () => {
    expect(
      refusedRow(
        "repositories 1 (https://a/x) and 3 (https://b/x) both go into x; give one",
      ),
    ).toEqual({ row: 3, field: "directory" });
    expect(
      refusedRow(
        "repositories 1 (https://a/x) and 2 (https://a/X) are the same repository, a/x; list it once",
      ),
    ).toEqual({ row: 2, field: "url" });
    expect(
      refusedRow("repository 2 (https://a/x): use an https:// URL"),
    ).toEqual({ row: 2, field: "url" });
    expect(
      refusedRow(
        "repository 4 (https://a/x): is the volume's own lost+found folder",
      ),
    ).toEqual({ row: 4, field: "directory" });
    expect(
      refusedRow("repository 1 (https://a/x): there is no identity 9"),
    ).toEqual({ row: 1, field: "url" });
  });
  it("is null for anything else", () => {
    expect(refusedRow("a workspace holds at most 100 repositories")).toBeNull();
    expect(refusedRow("")).toBeNull();
  });
});

const folderRow = (key: number, localPath: string, url = "") => ({
  ...newRow(key, url),
  source: "folder" as const,
  localPath,
});

describe("a checkout on this computer", () => {
  it.each([
    ["/home/you/shop", "shop"],
    ["/home/you/shop/", "shop"],
    ["/home/you/shop/.git", "shop"],
    ["C:\\Users\\you\\Shop", "Shop"],
    ["C:\\Users\\you\\Shop\\.git\\", "Shop"],
    ["/home/you/.hidden", null],
    ["/", null],
  ])("%j is cloned into %j", (path, dir) => {
    expect(folderOf(path)).toBe(dir);
  });

  it.each([
    ["/home/you/shop", null],
    ["C:\\Users\\you\\shop", null],
    ["c:/Users/you/shop", null],
    ["\\\\server\\share\\shop", null],
    ['"C:\\Users\\you\\shop"', null],
    ['  "/home/you/shop"  ', null],
    ["/", "not the root of a drive"],
    ["//", "not the root of a drive"],
    ["C:\\", "not the root of a drive"],
    ["c:/", "not the root of a drive"],
    ["\\\\", "not the root of a drive"],
    ['"C:\\"', "not the root of a drive"],
    ["", "Give the folder of the checkout."],
    ['""', "Give the folder of the checkout."],
    ["shop", "Give the full path"],
    ["~/shop", "Give the full path"],
  ])("%j: %j", (path, problem) => {
    const found = localPathProblem(path);
    if (problem === null) expect(found).toBeNull();
    else expect(found).toContain(problem);
  });

  it("takes the folder's name as its directory, unless one is typed or the name is unusable", () => {
    expect(directoryFor(folderRow(1, "/home/you/shop"))).toBe("shop");
    expect(
      directoryFor({ ...folderRow(1, "/home/you/shop"), directory: "web" }),
    ).toBe("web");
    expect(
      directoryFor(folderRow(1, "/home/you/.x", "https://h/a/api.git")),
    ).toBe("api");
    // A folder typed but the address chosen: the folder does not count.
    expect(
      directoryFor({
        ...folderRow(1, "/home/you/shop", "https://h/a/api.git"),
        source: "address",
      }),
    ).toBe("api");
  });

  it("needs no address, and a typed one is still checked", () => {
    expect(
      rowProblems([folderRow(1, "/home/you/shop")]).get(1),
    ).toBeUndefined();
    expect(rowProblems([folderRow(1, "shop")]).get(1)?.localPath).toContain(
      "full path",
    );
    expect(rowProblems([folderRow(1, "")]).get(1)?.localPath).toBe(
      "Give the folder of the checkout.",
    );
    expect(
      rowProblems([folderRow(1, "/home/you/shop", "http://x/y")]).get(1)?.url,
    ).toBe("Use an https:// URL.");
    // An address row with a folder left over from switching is checked as an address.
    const switched = {
      ...folderRow(1, "/home/you/shop"),
      source: "address" as const,
    };
    expect(rowProblems([switched]).get(1)?.url).toBe(
      "Enter the repository's HTTPS URL.",
    );
  });

  it("clashes on the folder's directory like any other", () => {
    const found = clashes([
      folderRow(1, "/home/you/api"),
      row(2, "https://github.com/acme/api.git"),
    ]);
    expect(found.get(2)?.directory).toContain("also goes into api");
    expect(checkRows([folderRow(1, "")]).get(1)?.localPath).toBeDefined();
  });

  it("puts a refusal about the checkout on its folder field", () => {
    expect(
      refusedRow(
        "repository 2 (the checkout /home/you/shop): it has no origin remote",
      ),
    ).toEqual({
      row: 2,
      field: "localPath",
    });
  });
});

describe("cleanPath", () => {
  it("drops the quotes Windows' Copy as path adds, and only a matching outer pair", () => {
    expect(cleanPath(' "C:\\a b\\shop" ')).toBe("C:\\a b\\shop");
    expect(cleanPath("/home/you/shop")).toBe("/home/you/shop");
    expect(cleanPath('"/home/you/shop')).toBe('"/home/you/shop');
    expect(folderOf('"C:\\Users\\you\\shop"')).toBe("shop");
  });
});
