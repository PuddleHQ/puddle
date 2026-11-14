// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { parseSshAgentPipe, SSH_AGENT_PIPE_MAX } from "./ssh-agent.ts";

describe("parseSshAgentPipe", () => {
  it("trims a path and takes a Windows pipe as typed", () => {
    expect(parseSshAgentPipe(String.raw`  \\.\pipe\pageant.alice `)).toEqual({
      ok: true,
      pipe: String.raw`\\.\pipe\pageant.alice`,
    });
  });

  it("reads blank as unset", () => {
    expect(parseSshAgentPipe("   ")).toEqual({ ok: true, pipe: null });
  });

  it("refuses a control character and an over-long path", () => {
    expect(parseSshAgentPipe("/run/a\nb")).toEqual({
      ok: false,
      message: "A pipe or socket path cannot hold a control character.",
    });
    expect(parseSshAgentPipe("x".repeat(SSH_AGENT_PIPE_MAX)).ok).toBe(true);
    expect(parseSshAgentPipe("x".repeat(SSH_AGENT_PIPE_MAX + 1))).toEqual({
      ok: false,
      message: "Use at most 1024 characters.",
    });
  });
});
