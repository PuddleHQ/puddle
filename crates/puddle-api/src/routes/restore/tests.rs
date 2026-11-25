// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const _: () = assert!(MAX_UPLOAD_BYTES > 4 * 1024 * 1024 * 1024);
const _: () = assert!(MAX_UPLOAD_BYTES < 5 * 1024 * 1024 * 1024);
