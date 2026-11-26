// SPDX-License-Identifier: AGPL-3.0-or-later
export const DUMPED: string[];
export const ERROR_TEXT: RegExp;
export function project(path: string, body: unknown): unknown;
export function missing(before: unknown, after: unknown, at?: string): string[];
export function compareDump(path: string, before: unknown, after: unknown): string[];
export function compareDumps(before: Record<string, unknown>, after: Record<string, unknown>): string[];
export function versionParts(text: string): number[];
export function compareVersions(a: string, b: string): number;
export function buildIsNotOlder(
  before: { version: string; api_version: string },
  after: { version: string; api_version: string },
): string[];
export function buildIsNewer(before: { version: string }, after: { version: string }): string[];
