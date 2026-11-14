// SPDX-License-Identifier: AGPL-3.0-or-later
// The keys a host of the SSH agent may be limited to, as the tab's box takes them: one SHA-256
// fingerprint per line, as `ssh-keygen -l` prints them. Matches the API (`SHA256:` and 43 base64
// characters, at most 64 keys).

export const MAX_KEYS = 64;
const FINGERPRINT = /^SHA256:[A-Za-z0-9+/]{43}$/;

/** The fingerprints typed (`null` for none: every key), or the first line that is not one. */
export function parseKeyList(
  text: string,
): { ok: true; keys: string[] | null } | { ok: false; message: string } {
  const lines = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter((l) => l !== "");
  if (lines.length === 0) return { ok: true, keys: null };
  const bad = lines.find((l) => !FINGERPRINT.test(l));
  if (bad !== undefined) {
    return {
      ok: false,
      message: `"${bad}" is not a SHA-256 key fingerprint (SHA256: and 43 characters, as ssh-keygen -l prints it).`,
    };
  }
  const keys = [...new Set(lines)];
  if (keys.length > MAX_KEYS) {
    return { ok: false, message: `Use at most ${MAX_KEYS} keys.` };
  }
  return { ok: true, keys };
}
