// SPDX-License-Identifier: AGPL-3.0-or-later
// When a child process has ended and its stdout and stderr are read to the end.
//
// `exit` can fire before the last output chunks are read, so a caller that reads captured output
// once `exit` has fired can miss the lines that explain the failure. `close` fires after the
// streams are drained. A grandchild that keeps the pipes open would delay `close` for good, so
// the wait for it is bounded by `graceMs` after `exit`.

export const DRAIN_GRACE_MS = 2000;

/**
 * Calls `done(code, signal)` once, when the child has closed its streams, or `graceMs` after it
 * exited, or when it failed to spawn. Returns nothing.
 */
export function onChildEnd(child, done, graceMs = DRAIN_GRACE_MS) {
  let finished = false;
  let timer;
  const finish = (code, signal) => {
    if (finished) return;
    finished = true;
    clearTimeout(timer);
    done(code, signal);
  };
  child.once("exit", (code, signal) => {
    timer = setTimeout(() => finish(code, signal), graceMs);
    timer.unref?.();
  });
  child.once("close", finish);
  child.once("error", () => finish(null, null));
}
