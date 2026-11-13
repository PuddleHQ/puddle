// SPDX-License-Identifier: GPL-3.0-or-later
// The SSH agent field on the Settings screen: what a typed pipe or socket may be. Matches the
// API's `ui.ssh_agent_pipe` (blank is unset, at most 1024 characters, no control characters).

export const SSH_AGENT_PIPE_MAX = 1024;

/** A C0 or C1 control character, the set Rust's `char::is_control` names. */
const isControl = (ch: string) => {
  const code = ch.codePointAt(0) ?? 0;
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
};

/** The pipe or socket from what was typed (`null` for blank, meaning unset), or why it cannot be used. */
export function parseSshAgentPipe(
  text: string,
): { ok: true; pipe: string | null } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (trimmed === "") return { ok: true, pipe: null };
  if (trimmed.length > SSH_AGENT_PIPE_MAX)
    return {
      ok: false,
      message: `Use at most ${SSH_AGENT_PIPE_MAX} characters.`,
    };
  if ([...trimmed].some(isControl))
    return {
      ok: false,
      message: "A pipe or socket path cannot hold a control character.",
    };
  return { ok: true, pipe: trimmed };
}
