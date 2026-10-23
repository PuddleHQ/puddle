// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { credential, identity } from "#lib/testing/fake-identities.ts";
import { checkout, listing, workspace } from "#lib/testing/fake-workspaces.ts";
import {
  addProblem,
  branchLabel,
  cloneAgainProblem,
  missingEntries,
  remoteDiffers,
  servingIdentity,
  stoppedWords,
  unreadableLabel,
} from "./checkouts.ts";

describe("what a checkout says about itself", () => {
  it("names the branch, or where a detached HEAD is, or that nothing is committed", () => {
    expect(branchLabel(checkout("a", { branch: "feature/x" }))).toBe(
      "feature/x",
    );
    expect(branchLabel(checkout("a", { branch: null, head: "9f8e7d6" }))).toBe(
      "detached at 9f8e7d6",
    );
    expect(branchLabel(checkout("a", { branch: null, head: null }))).toBe(
      "no commits yet",
    );
  });

  it("says why a checkout could not be read, and nothing for one that could", () => {
    expect(unreadableLabel(checkout("a"))).toBeNull();
    expect(
      unreadableLabel(checkout("a", { unreadable: "dubious ownership" })),
    ).toBe("Puddle could not read this checkout: dubious ownership");
  });

  it("notices when the list's address and the checkout's remote are different repositories", () => {
    const entry = { url: "https://github.com/acme/api.git", directory: "api" };
    expect(remoteDiffers(checkout("api"))).toBe(false);
    expect(
      remoteDiffers(checkout("api", { entry, remote_url: entry.url })),
    ).toBe(false);
    // The same repository spelled another way is not a difference.
    expect(
      remoteDiffers(
        checkout("api", {
          entry,
          remote_url: "https://GitHub.com/Acme/API",
        }),
      ),
    ).toBe(false);
    expect(
      remoteDiffers(
        checkout("api", {
          entry,
          remote_url: "https://github.com/acme/other.git",
        }),
      ),
    ).toBe(true);
    expect(remoteDiffers(checkout("api", { entry, remote_url: null }))).toBe(
      false,
    );
    // An address Puddle cannot read is compared as text.
    expect(
      remoteDiffers(
        checkout("api", {
          entry: { url: "https://github.com/a/b/c", directory: "api" },
          remote_url: "https://github.com/a/b/d",
        }),
      ),
    ).toBe(true);
  });
});

describe("which identity serves a repository", () => {
  const work = identity(1, {
    label: "Work",
    credentials: [credential({ owners: ["acme"], rest: false })],
  });

  it("is the one that covers the owner, or none", () => {
    expect(servingIdentity([work], "https://github.com/acme/api.git")).toBe(
      work,
    );
    expect(
      servingIdentity([work], "https://github.com/other/api.git"),
    ).toBeNull();
    expect(servingIdentity([work], "not an address")).toBeNull();
  });
});

describe("the list entries that are not on the disk", () => {
  const entries = [
    { url: "https://github.com/acme/a.git", directory: "a" },
    { url: "https://github.com/acme/b.git", directory: "b" },
  ];

  it("are the ones the listing names, in list order", () => {
    expect(missingEntries(entries, null)).toEqual([]);
    expect(missingEntries(entries, listing([], { missing: ["b"] }))).toEqual([
      entries[1],
    ]);
  });
});

describe("what a stopped workspace says", () => {
  it("shows the last listing's time, or says to start it", () => {
    expect(stoppedWords(null)).toBe(
      "Start the workspace to see its checkouts.",
    );
    expect(stoppedWords(listing([], { checked_at: null }))).toBe(
      "Start the workspace to see its checkouts.",
    );
    expect(stoppedWords(listing([]))).toBe(
      "The workspace is stopped. This is the last listing.",
    );
  });
});

describe("why Add repository and Clone again are off", () => {
  const running = workspace("w", { status: "running" });

  it("allows both for an idle running workspace", () => {
    expect(addProblem(running)).toBeNull();
    expect(cloneAgainProblem(running)).toBeNull();
  });

  it("says to start a workspace that is not running", () => {
    const stopped = workspace("w", { status: "stopped" });
    expect(addProblem(stopped)).toBe(
      "Start the workspace to add a repository.",
    );
    expect(cloneAgainProblem(stopped)).toBe(
      "Start the workspace to clone it again.",
    );
  });

  it("says to wait while something runs", () => {
    const busy = workspace("w", { status: "running", busy: "cloning" });
    expect(addProblem(busy)).toBe("Cloning: wait for it to finish.");
    expect(cloneAgainProblem(busy)).toBe("Cloning: wait for it to finish.");
  });

  it("refuses a hundred and first entry", () => {
    expect(addProblem(running, 100)).toBe(
      "A workspace lists at most 100 repositories.",
    );
    expect(addProblem(running, 99)).toBeNull();
  });
});
