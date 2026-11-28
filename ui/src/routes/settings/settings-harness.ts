// SPDX-License-Identifier: AGPL-3.0-or-later
// The fake settings API the Settings tests share. A test file mocks the API client with it
// (vi.mock must stay in the file that uses it).
import { FakeSettings } from "#lib/testing/fake-settings.ts";

export const api = new FakeSettings();
