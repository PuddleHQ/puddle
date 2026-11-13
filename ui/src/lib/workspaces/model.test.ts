// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  cleanCheck,
  dirtyCheck,
  workspace,
} from "#lib/testing/fake-workspaces.ts";
import {
  MAX_NAME_LENGTH,
  MAX_ROOT_DISK_GIB,
  canAttach,
  canDelete,
  canGrowRootDisk,
  canReclaim,
  canStart,
  canStop,
  canResetRootDisk,
  diskLabel,
  diskPercent,
  diskWarned,
  rootDiskGrowProblem,
  rootDiskLabel,
  suggestedRootDiskGib,
  formatMib,
  isDown,
  losses,
  nameProblem,
  operationLabel,
  parseMemoryGib,
  repoUrlProblem,
  sortWorkspaces,
  canRename,
  statusLabel,
  operationOfStep,
  statusTone,
  stepLabel,
  stepPosition,
  suggestName,
  type Status,
  type Step,
} from "./model.ts";

const STATUSES: Status[] = [
  "created",
  "starting",
  "running",
  "draining",
  "paused",
  "stopped",
  "crashed",
  "volume_missing",
];

describe("what can be done in each state", () => {
  it("down states can start and delete, running can stop and attach, nothing else", () => {
    const table = STATUSES.map((status) => {
      const w = workspace("w", { status });
      return [
        status,
        canStart(w),
        canStop(w),
        canAttach(w),
        canDelete(w),
        canReclaim(w),
      ];
    });
    expect(table).toEqual([
      ["created", true, false, false, true, false],
      ["starting", false, false, false, false, false],
      ["running", false, true, true, false, true],
      ["draining", false, false, false, false, false],
      ["paused", false, false, false, false, false],
      ["stopped", true, false, false, true, true],
      ["crashed", true, false, false, true, false],
      ["volume_missing", true, false, false, true, false],
    ]);
    expect(STATUSES.filter(isDown)).toEqual([
      "created",
      "stopped",
      "crashed",
      "volume_missing",
    ]);
  });

  it("nothing is allowed while an operation runs", () => {
    for (const status of STATUSES) {
      const w = workspace("w", { status, busy: "reclaiming" });
      expect([
        canStart(w),
        canStop(w),
        canAttach(w),
        canDelete(w),
        canReclaim(w),
      ]).toEqual([false, false, false, false, false]);
    }
  });
});

describe("words", () => {
  it("has a label and a tone for every status", () => {
    for (const status of STATUSES) {
      expect(statusLabel(status)).not.toBe("");
      expect(["ok", "busy", "warn", "bad", "idle"]).toContain(
        statusTone(status),
      );
    }
    expect(statusLabel("created")).toBe("Not started");
    expect(statusTone("running")).toBe("ok");
    expect(statusTone("crashed")).toBe("bad");
  });

  it("has a label for every step and operation", () => {
    const steps: Step[] = [
      "preparing_volume",
      "pulling_image",
      "cloning",
      "starting",
      "syncing",
      "checking",
      "reclaiming",
      "stopping",
      "removing",
      "done",
      "failed",
    ];
    for (const step of steps) expect(stepLabel(step)).not.toBe("");
    expect(operationLabel("deleting")).toBe("Deleting");
  });

  it("tells the operation from a step alone", () => {
    expect(operationOfStep("cloning")).toBe("creating");
    expect(operationOfStep("syncing")).toBe("starting");
    expect(operationOfStep("stopping")).toBe("stopping");
    expect(operationOfStep("removing")).toBe("deleting");
    expect(operationOfStep("reclaiming")).toBe("reclaiming");
    expect(operationOfStep("done")).toBeNull();
    expect(operationOfStep("failed")).toBeNull();
  });

  it("places a step in its operation", () => {
    expect(stepPosition("creating", "cloning")).toEqual({ index: 3, of: 3 });
    expect(stepPosition("starting", "starting")).toEqual({ index: 1, of: 2 });
    expect(stepPosition("deleting", "removing")).toEqual({ index: 2, of: 2 });
    expect(stepPosition("reclaiming", "reclaiming")).toEqual({
      index: 1,
      of: 1,
    });
    expect(stepPosition("creating", "stopping")).toBeNull();
  });
});

describe("suggested names", () => {
  it("suggests the repository's own name as typed, without a slug", () => {
    expect(suggestName("https://github.com/acme/Ledger_Service.git")).toBe(
      "Ledger_Service",
    );
    expect(suggestName("https://github.com/acme/web-shop/")).toBe("web-shop");
    expect(suggestName("https://example.org/a/b.git?x=1#y")).toBe("b");
    expect(suggestName("")).toBe("");
    expect(suggestName("https://github.com/acme/tauri.git")).toBe("tauri");
    expect(suggestName("/home/me/Shop API \u00fc")).toBe("Shop API \u00fc");
    expect(suggestName(`https://h/${"x".repeat(80)}`)).toHaveLength(
      MAX_NAME_LENGTH,
    );
    expect(
      nameProblem(
        suggestName("https://github.com/acme/Ledger_Service.git"),
        null,
        [],
      ),
    ).toBeNull();
  });

  it("allows the longest name and refuses one more", () => {
    expect(nameProblem("a".repeat(MAX_NAME_LENGTH), null, [])).toBeNull();
    expect(nameProblem("a".repeat(MAX_NAME_LENGTH + 1), null, [])).toMatch(
      /at most 60/,
    );
  });

  it("takes the old reserved and label-only names as plain text", () => {
    for (const ok of ["tauri", "m--x", "Web", "a b", "a_b", "-a", "a-"])
      expect(nameProblem(ok, null, []), ok).toBeNull();
  });
});

describe("repository URLs", () => {
  const ssh =
    "SSH remotes are not supported yet; use the repository's HTTPS URL instead.";
  it.each([
    ["https://github.com/acme/web-shop.git", null],
    ["  https://github.com/acme/web-shop.git  ", null],
    ["HTTPS://GitHub.com/acme/x", null],
    ["https://host:8443/x.git", null],
    ["", "Enter the repository's HTTPS URL."],
    ["git@github.com:acme/web-shop.git", ssh],
    ["ssh://git@github.com/acme/x.git", ssh],
    ["github.com:acme/x.git", ssh],
    ["http://github.com/x.git", "Use an https:// URL."],
    ["ftp://x/y", "Use an https:// URL."],
    ["github.com/acme/x", "Use an https:// URL."],
    ["https://github.com/a b", "The URL must not contain spaces."],
    [
      "https://user:pw@github.com/x.git",
      "Remove the user name and password from the URL; Puddle supplies credentials itself.",
    ],
    ["https://", "The URL has no host."],
    ["https:///x", "The URL has no host."],
    ["https://:8443/x", "The URL has no host."],
  ])("%j -> %j", (url, problem) => {
    expect(repoUrlProblem(url)).toBe(problem);
  });

  it("does not take a Windows drive letter for an SSH host", () => {
    expect(repoUrlProblem("C:/repos/x")).toBe("Use an https:// URL.");
  });
});

describe("memory", () => {
  it("reads GiB, rounding to MiB; empty means the default", () => {
    expect(parseMemoryGib("")).toEqual({ ok: true, mib: null });
    expect(parseMemoryGib("  ")).toEqual({ ok: true, mib: null });
    expect(parseMemoryGib("8")).toEqual({ ok: true, mib: 8192 });
    expect(parseMemoryGib("0.5")).toEqual({ ok: true, mib: 512 });
    expect(parseMemoryGib("1.5")).toEqual({ ok: true, mib: 1536 });
  });

  it("refuses text and sizes the API would refuse", () => {
    expect(parseMemoryGib("lots")).toEqual({
      ok: false,
      message: "Enter a number of GiB.",
    });
    expect(parseMemoryGib("0.1")).toEqual({
      ok: false,
      message: "Use between 0.25 GiB and 1024 GiB.",
    });
    expect(parseMemoryGib("2000").ok).toBe(false);
  });

  it("formats sizes", () => {
    expect(formatMib(512)).toBe("512 MiB");
    expect(formatMib(8192)).toBe("8 GiB");
    expect(formatMib(1536)).toBe("1.5 GiB");
  });

  it("says what the disk holds, or only its size when unknown", () => {
    expect(diskLabel(workspace("w", { disk_used_mib: 6144 }))).toBe(
      "6 GiB of 32 GiB used",
    );
    expect(diskLabel(workspace("w", { disk_used_mib: null }))).toBe(
      "32 GiB disk",
    );
  });

  it("says what the root disk holds, only its size when unknown, nothing without a sandbox", () => {
    expect(
      rootDiskLabel(
        workspace("w", { root_disk_size_mib: 4096, root_disk_used_mib: 3900 }),
      ),
    ).toBe("3.8 GiB of 4 GiB used");
    expect(
      rootDiskLabel(
        workspace("w", { root_disk_size_mib: 4096, root_disk_used_mib: null }),
      ),
    ).toBe("4 GiB root disk");
    expect(
      rootDiskLabel(
        workspace("w", { root_disk_size_mib: null, root_disk_used_mib: null }),
      ),
    ).toBeNull();
  });

  it("rounds the percent down, caps it at full and needs both figures", () => {
    const w = (over: Record<string, unknown>) => workspace("w", over);
    expect(
      diskPercent(
        w({ root_disk_size_mib: 1000, root_disk_used_mib: 949 }),
        "root",
      ),
    ).toBe(94);
    expect(
      diskPercent(
        w({ root_disk_size_mib: 1000, root_disk_used_mib: 5000 }),
        "root",
      ),
    ).toBe(100);
    expect(
      diskPercent(w({ disk_size_mib: 1000, disk_used_mib: 999 }), "volume"),
    ).toBe(99);
    expect(diskPercent(w({ root_disk_size_mib: null }), "root")).toBeNull();
    expect(diskPercent(w({ disk_used_mib: null }), "volume")).toBeNull();
    expect(
      diskPercent(w({ root_disk_size_mib: 0, root_disk_used_mib: 0 }), "root"),
    ).toBeNull();
  });

  it("flags the disks the host listed and offers a reset only to a stopped, idle workspace", () => {
    const w = workspace("w", { disk_warnings: ["volume"] });
    expect(diskWarned(w, "volume")).toBe(true);
    expect(diskWarned(w, "root")).toBe(false);
    for (const status of ["stopped", "crashed"] as const)
      expect(canResetRootDisk(workspace("w", { status }))).toBe(true);
    for (const status of [
      "running",
      "starting",
      "draining",
      "created",
    ] as const)
      expect(canResetRootDisk(workspace("w", { status }))).toBe(false);
    expect(
      canResetRootDisk(workspace("w", { status: "stopped", busy: "starting" })),
    ).toBe(false);
  });
});

describe("growing the root disk", () => {
  it("is offered to an idle workspace with a root disk, running or down, but not while it starts or stops", () => {
    const base = { root_disk_size_mib: 10_240 };
    for (const status of ["running", "stopped", "crashed", "created"] as const)
      expect(canGrowRootDisk(workspace("w", { ...base, status }))).toBe(true);
    for (const status of ["starting", "draining", "volume_missing"] as const)
      expect(canGrowRootDisk(workspace("w", { ...base, status }))).toBe(false);
    expect(
      canGrowRootDisk(workspace("w", { ...base, busy: "reclaiming" })),
    ).toBe(false);
    expect(canGrowRootDisk(workspace("w", { root_disk_size_mib: null }))).toBe(
      false,
    );
  });

  it("suggests ten GiB more, never past what the API takes", () => {
    expect(
      suggestedRootDiskGib(workspace("w", { root_disk_size_mib: 10_240 })),
    ).toBe(20);
    expect(
      suggestedRootDiskGib(workspace("w", { root_disk_size_mib: 10_752 })),
    ).toBe(20);
    expect(
      suggestedRootDiskGib(
        workspace("w", { root_disk_size_mib: MAX_ROOT_DISK_GIB * 1024 }),
      ),
    ).toBe(MAX_ROOT_DISK_GIB);
    expect(
      suggestedRootDiskGib(workspace("w", { root_disk_size_mib: null })),
    ).toBe(10);
  });

  it("names what is wrong with a size and accepts a bigger whole number", () => {
    const w = workspace("w", { root_disk_size_mib: 10_240 });
    expect(rootDiskGrowProblem("", w)).toBe("Enter a whole number of GiB.");
    expect(rootDiskGrowProblem("2.5", w)).toBe("Enter a whole number of GiB.");
    expect(rootDiskGrowProblem("-3", w)).toBe("Enter a whole number of GiB.");
    expect(rootDiskGrowProblem("10", w)).toBe(
      "A root disk only grows: choose more than 10 GiB.",
    );
    expect(rootDiskGrowProblem("3", w)).toContain("choose more than 10 GiB");
    expect(rootDiskGrowProblem("2048", w)).toBe("Choose at most 2047 GiB.");
    expect(rootDiskGrowProblem(" 11 ", w)).toBeNull();
    expect(rootDiskGrowProblem("2047", w)).toBeNull();
  });
});

describe("the list", () => {
  it("sorts by name without touching the input", () => {
    const input = [workspace("b"), workspace("a"), workspace("c")];
    expect(sortWorkspaces(input).map((w) => w.name)).toEqual(["a", "b", "c"]);
    expect(input.map((w) => w.name)).toEqual(["b", "a", "c"]);
  });
});

describe("names", () => {
  const all = [workspace("a", { name: "Web shop" }), workspace("b")];
  const problem = (text: string) => nameProblem(text, all[1]!.id, all);

  it("sorts the list by what the user calls the workspaces", () => {
    expect(sortWorkspaces(all).map((w) => w.name)).toEqual(["b", "Web shop"]);
    expect(
      sortWorkspaces([
        workspace("z", { name: "Same" }),
        workspace("y", { name: "Same" }),
      ]).map((w) => w.id),
    ).toEqual(["w-y", "w-z"]);
  });

  it("accepts a name, trimmed, with spaces, capitals and other scripts", () => {
    for (const ok of ["Web", "  Web 2  ", "Café 日本", "é".repeat(60), "b"])
      expect(problem(ok), ok).toBeNull();
  });

  it("says what is wrong, in words for the person typing", () => {
    expect(problem("")).toBe("Give the workspace a name.");
    expect(problem("   ")).toBe("Give the workspace a name.");
    expect(problem("x".repeat(61))).toBe("Use at most 60 characters.");
    expect(problem("two\nlines")).toBe(
      "Use a single line, without control characters.",
    );
    expect(problem("web SHOP")).toBe(
      'Another workspace is already called "Web shop".',
    );
  });

  it("is not a clash with the workspace's own name", () => {
    expect(nameProblem("web shop", all[0]!.id, all)).toBeNull();
    // A new workspace has no id yet, so every name counts.
    expect(nameProblem("web shop", null, all)).toMatch(/already called/);
  });

  it("allows a rename whenever the workspace is not being created or deleted", () => {
    expect(canRename(workspace("w", { busy: null }))).toBe(true);
    for (const busy of ["starting", "stopping", "reclaiming"] as const)
      expect(canRename(workspace("w", { busy })), busy).toBe(true);
    for (const busy of ["creating", "deleting"] as const)
      expect(canRename(workspace("w", { busy })), busy).toBe(false);
  });
});

describe("what a delete would lose", () => {
  it("is nothing for a clean check", () => {
    expect(losses(cleanCheck("w"))).toEqual([]);
  });

  it("lists each kind per repository, with how many more there are", () => {
    expect(losses(dirtyCheck("w"))).toEqual([
      {
        where: "w",
        what: "Uncommitted changes",
        items: [" M a.md", "?? b.md"],
        more: 3,
      },
      {
        where: "w",
        what: "Commits that are on no remote branch",
        items: ["abc123 Fix it"],
        more: 0,
      },
      {
        where: "",
        what: "Files outside any repository",
        items: ["scratch"],
        more: 0,
      },
    ]);
  });

  it("counts a list that has only a remainder", () => {
    const check = cleanCheck("w", {
      clean: false,
      repos: [
        {
          dir: "r",
          clean: false,
          uncommitted: { items: [], more: 0 },
          unpushed: { items: [], more: 0 },
          stashes: { items: [], more: 2 },
        },
      ],
    });
    expect(losses(check)).toEqual([
      { where: "r", what: "Stashes", items: [], more: 2 },
    ]);
  });
});
