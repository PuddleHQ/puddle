// SPDX-License-Identifier: AGPL-3.0-or-later
// Which forms hold typing that is not saved yet. The page must not reload itself over one: the
// build guard waits until none is left, so an automatic reload cannot drop what a person typed.

const holders = new Set<() => boolean>();

/** Registers a form's "has unsaved edits" test; returns the function that unregisters it. */
export function holdReload(hasEdits: () => boolean): () => void {
  // A fresh entry per call, so two forms registering the same function stay separate.
  const holder = () => hasEdits();
  holders.add(holder);
  return () => void holders.delete(holder);
}

/** True while any registered form has edits that are not saved. */
export function hasUnsavedEdits(): boolean {
  for (const holder of holders) if (holder()) return true;
  return false;
}
