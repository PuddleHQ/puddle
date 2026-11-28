// SPDX-License-Identifier: AGPL-3.0-or-later
import type { RowDraft } from "./repo-rows.ts";

/**
 * What the New workspace screen holds, kept while you leave it and come back (a link elsewhere, the
 * back button). It lives in memory only: a reload starts fresh, and nothing here is a secret.
 */
export interface WorkspaceDraft {
  rows: RowDraft[];
  name: string;
  nameTouched: boolean;
  image: string;
  memoryGib: string;
  cpusChosen: number | null;
  startFolder: string;
}

let saved: WorkspaceDraft | null = null;

export const savedDraft = (): WorkspaceDraft | null => saved;
export const saveDraft = (draft: WorkspaceDraft): void => {
  saved = draft;
};
export const clearDraft = (): void => {
  saved = null;
};
