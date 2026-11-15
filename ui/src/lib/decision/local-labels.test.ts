// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  COMPANY_SIGNAL_TEXT,
  LOCAL_LABELS,
  companySignalText,
} from "./local-labels.ts";

describe("local labels", () => {
  it("has a label for every category", () => {
    expect(Object.keys(LOCAL_LABELS).sort()).toEqual([
      "company_network",
      "link_local",
      "loopback",
      "metadata",
      "private",
      "special",
    ]);
  });

  it("says why a company-network signal applies, and nothing for one it doesn't know", () => {
    expect(companySignalText("vpn_route")).toBe(
      COMPANY_SIGNAL_TEXT["vpn_route"],
    );
    expect(companySignalText("later")).toBeNull();
    expect(companySignalText(null)).toBeNull();
  });
});
