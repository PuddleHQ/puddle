// SPDX-License-Identifier: AGPL-3.0-or-later
// Moved lines for the diff-coverage gate (docs/STANDARDS.md, "Coverage"): a file split cuts code out of one
// place and pastes it in another, and `git diff --no-renames` shows every pasted line as added. The gate
// has no coverage report of the base, so it cannot tell whether such a line was tested before; but a line
// that only moved is not a change this diff makes, so the gate does not judge it as one.

/** A line a diff adds or removes, with the hunk it sits in (a run is only ever taken inside one hunk). */
interface Side {
  path: string;
  line: number;
  text: string;
  hunk: number;
}

/** The least letters and digits a moved run needs: git's own `--color-moved` rule, so `}` and `)?;` never count. */
export const MIN_MOVED_ALNUM = 20;

const alnum = (s: string) => s.replace(/[^\p{L}\p{N}]/gu, "").length;

/** Splits `git diff -U0` output into its added and removed lines, each with its path, line and hunk. */
function sides(diff: string): { added: Side[]; removed: Side[] } {
  const added: Side[] = [];
  const removed: Side[] = [];
  let path: string | undefined;
  let hunk = 0;
  let inHunk = false;
  let nextNew = 0;
  let nextOld = 0;
  for (const raw of diff.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      inHunk = false;
    } else if (!inHunk && raw.startsWith("+++ ")) {
      path =
        raw === "+++ /dev/null"
          ? undefined
          : raw.slice(4).replace(/^b\//, "").split("\\").join("/");
    } else if (!inHunk && raw.startsWith("--- ")) {
      // The old path is not needed: a removed line is only compared by its text.
    } else if (raw.startsWith("@@")) {
      inHunk = true;
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(raw);
      hunk += 1;
      nextOld = m ? Number(m[1]) : 0;
      nextNew = m ? Number(m[2]) : 0;
    } else if (raw.startsWith("-")) {
      removed.push({
        path: path ?? "",
        line: nextOld,
        text: raw.slice(1).trim(),
        hunk,
      });
      nextOld += 1;
    } else if (path && raw.startsWith("+")) {
      added.push({ path, line: nextNew, text: raw.slice(1).trim(), hunk });
      nextNew += 1;
    }
  }
  return { added, removed };
}

/**
 * The added lines (as `path:line`) that are a run of lines the same diff removes elsewhere: the same
 * trimmed text in the same order, at least {@link MIN_MOVED_ALNUM} letters and digits long. A removed
 * line is used once, so a copy (the old place still has it) is never a move, and a run only
 * reaches across lines that sit next to each other on both sides.
 */
export function movedLines(diff: string): Set<string> {
  const { added, removed } = sides(diff);
  const positions = new Map<string, number[]>();
  for (const [i, r] of removed.entries()) {
    if (alnum(r.text) === 0) continue;
    positions.set(r.text, [...(positions.get(r.text) ?? []), i]);
  }
  const used = new Array<boolean>(removed.length).fill(false);
  const moved = new Set<string>();
  let j = 0;
  while (j < added.length) {
    const a = added[j] as Side;
    let best = { from: -1, len: 0 };
    for (const r of positions.get(a.text) ?? []) {
      if (used[r]) continue;
      let len = 0;
      while (
        j + len < added.length &&
        r + len < removed.length &&
        !used[r + len] &&
        (added[j + len] as Side).hunk === a.hunk &&
        (removed[r + len] as Side).hunk === (removed[r] as Side).hunk &&
        (added[j + len] as Side).text === (removed[r + len] as Side).text
      )
        len += 1;
      if (len > best.len) best = { from: r, len };
    }
    const text = added.slice(j, j + best.len).map((s) => s.text);
    if (best.len > 0 && alnum(text.join("")) >= MIN_MOVED_ALNUM) {
      for (let k = 0; k < best.len; k += 1) {
        used[best.from + k] = true;
        const s = added[j + k] as Side;
        moved.add(`${s.path}:${s.line}`);
      }
      j += best.len;
    } else j += 1;
  }
  return moved;
}
