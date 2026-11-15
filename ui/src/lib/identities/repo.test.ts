// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { parseRepoUrl, repoName } from "./repo.ts";

interface Case {
  url: string;
  repo: [string, string, string] | null;
}

// The same file drives the host's parser (`RepoRef::from_https_url`).
const cases = (
  JSON.parse(
    readFileSync(
      // Vitest runs in `ui/`.
      resolve(
        process.cwd(),
        "../crates/puddle-store/tests/data/repo-urls.json",
      ),
      "utf8",
    ),
  ) as { cases: Case[] }
).cases;

describe("parseRepoUrl with control characters in the path", () => {
  it.each([
    "https://github.com/o/re\u0001po",
    "https://github.com/o/re\u007fpo",
  ])("refuses %j", (url) => {
    expect(parseRepoUrl(url).ok).toBe(false);
  });
});

describe("parseRepoUrl", () => {
  it.each(cases)("reads $url like the host does", ({ url, repo }) => {
    const got = parseRepoUrl(url);
    if (repo === null) {
      expect(got.ok).toBe(false);
    } else {
      expect(got).toEqual({
        ok: true,
        host: repo[0],
        owner: repo[1],
        repo: repo[2],
      });
    }
  });

  it("says why an address is refused, in the create form's words", () => {
    const message = (url: string) => {
      const got = parseRepoUrl(url);
      return got.ok ? "" : got.message;
    };
    expect(message("")).toMatch(/Enter the repository/);
    expect(message("git@github.com:a/b.git")).toMatch(
      /SSH remotes are not supported/,
    );
    expect(message("https://me@github.com/a/b")).toMatch(
      /Remove the user name/,
    );
    expect(message("https://github.com/a")).toMatch(
      /not a repository address Puddle can read/,
    );
  });

  it.each([
    ["git@github.com:Acme/Web-Shop.git", ["github.com", "acme", "web-shop"]],
    ["ssh://git@github.com/acme/web", ["github.com", "acme", "web"]],
    [
      "ssh://git@ssh.github.com:443/acme/web.git",
      ["github.com", "acme", "web"],
    ],
    ["git@gitlab.com:grp/sub/web.git", ["gitlab.com", "grp", "sub/web"]],
    ["git@altssh.gitlab.com:grp/web.git", ["gitlab.com", "grp", "web"]],
    ["git@bitbucket.org:acme/web.git", ["bitbucket.org", "acme", "web"]],
    [
      "git@ssh.dev.azure.com:v3/Org/My%20Project/Repo",
      ["dev.azure.com", "org", "my project/repo"],
    ],
    [
      "ssh://git@vs-ssh.visualstudio.com/v3/org/proj/repo",
      ["org.visualstudio.com", "org", "proj/repo"],
    ],
  ])(
    "lists SSH address %s as its HTTPS repository",
    (url, [host, owner, repo]) => {
      expect(parseRepoUrl(url, { ssh: true })).toEqual({
        ok: true,
        host,
        owner,
        repo,
      });
    },
  );

  it("keeps refusing SSH where a workspace is cloned, and says why on the Git tab", () => {
    const ssh = "git@github.com:acme/web.git";
    expect(parseRepoUrl(ssh).ok).toBe(false);
    expect(parseRepoUrl(ssh, { ssh: false }).ok).toBe(false);
    const message = (url: string) => {
      const got = parseRepoUrl(url, { ssh: true });
      return got.ok ? "" : got.message;
    };
    expect(message("git@example.com:acme/web.git")).toMatch(
      /GitHub, GitLab, Bitbucket and Azure DevOps/,
    );
    expect(message("git@github.com:web")).toMatch(
      /not a repository address Puddle can read/,
    );
    expect(message("git@github.com:")).toMatch(/not an SSH address/);
    expect(message("git@github.com:acme/my repo.git")).toMatch(
      /not an SSH address/,
    );
    expect(message("ssh://git:pw@github.com/acme/web")).toMatch(
      /not an SSH address/,
    );
    expect(message("git@ssh.dev.azure.com:Org/Proj/Repo")).toMatch(
      /not an SSH address/,
    );
    expect(message("https://github.com/a")).toMatch(
      /not a repository address Puddle can read/,
    );
  });

  it("names a repository the way the table does", () => {
    expect(repoName({ host: "github.com", owner: "acme", repo: "web" })).toBe(
      "github.com/acme/web",
    );
  });
});
