// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { credential, identity } from "#lib/testing/fake-identities.ts";
import { previewIdentity } from "./preview.ts";

const work = identity(1, {
  label: "Work",
  is_default: false,
  credentials: [credential({ owners: ["acme"], rest: false })],
});
const personal = identity(2, {
  label: "Personal",
  is_default: true,
  credentials: [credential({ owners: [], rest: true })],
});
const gitlab = identity(3, {
  label: "GitLab",
  is_default: false,
  credentials: [credential({ host: "gitlab.com", owners: [], rest: true })],
});
const all = [work, personal, gitlab];

describe("previewIdentity", () => {
  it("names the identity that covers the owner, owner before rest of host", () => {
    const got = previewIdentity(
      [personal, work],
      [],
      null,
      "github.com",
      "ACME",
    );
    expect(got).toMatchObject({ basis: "covers", covered: true });
    expect(got.identity).toBe(work);
    expect(got.text).toBe("Uses Work: it covers ACME on github.com.");
  });

  it("falls back to a rest-of-host identity, and to the one on the list first", () => {
    expect(previewIdentity(all, [], null, "github.com", "me").identity).toBe(
      personal,
    );
    const other = identity(4, {
      label: "Other",
      is_default: false,
      credentials: [credential({ rest: true })],
    });
    const everyone = [personal, other];
    expect(
      previewIdentity(everyone, [], null, "github.com", "me").identity,
    ).toBe(personal);
    expect(
      previewIdentity(everyone, [other], null, "github.com", "me").identity,
    ).toBe(other);
    // Naming the owner still beats one already on the list that covers the rest.
    expect(
      previewIdentity([...everyone, work], [other], null, "github.com", "acme")
        .identity,
    ).toBe(work);
  });

  it("uses the default and says it has no credential when nothing covers the owner", () => {
    const got = previewIdentity([work, personal], [], null, "gitlab.com", "x");
    expect(got).toMatchObject({ basis: "default", covered: false });
    expect(got.identity).toBe(personal);
    expect(got.text).toContain("No identity covers x on gitlab.com");
    expect(got.text).toContain("the default, Personal");
    expect(got.text).toContain("without a credential");
  });

  it("says there is no identity when there is no default either", () => {
    const got = previewIdentity([work], [], null, "github.com", "me");
    expect(got).toMatchObject({
      basis: "none",
      identity: null,
      covered: false,
    });
    expect(got.text).toContain("no commit author and no credential");
    expect(previewIdentity([], [], null, "github.com", "me").basis).toBe(
      "none",
    );
  });

  it("follows the pick, and warns when the picked identity cannot reach the repository", () => {
    const covered = previewIdentity(all, [], 1, "github.com", "acme");
    expect(covered).toMatchObject({ basis: "chosen", covered: true });
    expect(covered.text).toBe("Uses Work: it covers acme on github.com.");
    const bare = previewIdentity(all, [], 3, "github.com", "acme");
    expect(bare).toMatchObject({ basis: "chosen", covered: false });
    expect(bare.text).toContain(
      "GitLab, which has no credential for acme on github.com",
    );
  });

  it("treats a pick that no longer exists as no pick", () => {
    expect(previewIdentity(all, [], 99, "github.com", "acme").identity).toBe(
      work,
    );
  });
});
