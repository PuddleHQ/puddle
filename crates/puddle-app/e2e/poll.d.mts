// SPDX-License-Identifier: AGPL-3.0-or-later
export class GiveUp extends Error {}
export function until<T>(
  what: string,
  probe: () => T | Promise<T>,
  within?: number,
  every?: number,
): Promise<NonNullable<Awaited<T>>>;
