// SPDX-License-Identifier: GPL-3.0-or-later
// Runs ci/cache-drop.sh <mode> <prefix>. The script never fails the job, and neither does a missing script.
const { spawnSync } = require("node:child_process");

module.exports = (mode) => {
  const script = `${process.env.GITHUB_WORKSPACE}/ci/cache-drop.sh`;
  const run = spawnSync(script, [mode, process.env.INPUT_PREFIX], { stdio: "inherit" });
  if (run.error) console.log(`::warning::cache-drop: ${run.error.message}`);
};
