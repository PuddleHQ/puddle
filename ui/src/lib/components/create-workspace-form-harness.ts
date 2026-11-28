// SPDX-License-Identifier: AGPL-3.0-or-later
// The switches the CreateWorkspaceForm tests share with their mocks. A test file imports `h` in a
// `vi.hoisted` call and mocks the repository lists, the row limit, the folder picker and
// navigation with the factories below (vi.mock must stay in the file that uses it).
import { vi } from "vitest";

// The limit is 100 (pinned in repo-rows.test.ts); a form with that many rows takes seconds to
// build in jsdom, so the dialog's use of the limit is tested with a small one.
export const h = {
  desktop: false,
  pick: vi.fn(),
  repos: undefined as unknown,
  rowLimit: undefined as number | undefined,
};

export function reposMock(mod: typeof import("#lib/stores/repos.svelte.ts")) {
  return {
    ...mod,
    RepoLists: class extends mod.RepoLists {
      constructor() {
        super({ api: h.repos as never });
      }
    },
  };
}

export function repoRowsMock(
  mod: typeof import("#lib/workspaces/repo-rows.ts"),
) {
  return {
    ...mod,
    get MAX_REPOSITORIES() {
      return h.rowLimit ?? mod.MAX_REPOSITORIES;
    },
  };
}

// The desktop app's folder dialog is a Tauri plugin; the page's wrapper is replaced here.
export const folderPickerMock = {
  inDesktopApp: () => h.desktop,
  pickFolder: (title: string) => h.pick(title),
};
