// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { deviceLabel } from "./device-label.ts";

describe("deviceLabel", () => {
  it.each([
    [
      "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0",
      "Firefox on Linux",
    ],
    [
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36 Edg/129.0.0.0",
      "Edge on Windows",
    ],
    [
      "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
      "Safari on macOS",
    ],
    [
      "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/129.0 Mobile/15E148 Safari/604.1",
      "Chrome on iOS",
    ],
    [
      "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36 Chrome/129.0 Mobile Safari/537.36 OPR/80.0",
      "Opera on Android",
    ],
    ["Mozilla/5.0 (X11; CrOS x86_64) Chrome/129.0", "Chrome on ChromeOS"],
  ])("%s", (agent, label) => {
    expect(deviceLabel(agent)).toBe(label);
  });

  it("keeps no version and says Browser when it knows nothing", () => {
    expect(deviceLabel("Firefox/130.0.1")).toBe("Firefox");
    expect(deviceLabel("Windows")).toBe("Windows");
    expect(deviceLabel("")).toBe("Browser");
    expect(deviceLabel("curl/8.0")).toBe("Browser");
  });
});
