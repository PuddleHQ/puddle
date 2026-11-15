# SPDX-License-Identifier: AGPL-3.0-or-later
<#
.SYNOPSIS
Starts node once on an idle runner, so the tests' node processes find it in memory.

.DESCRIPTION
On a fresh hosted Windows runner the first start of node.exe reads its image from a lazily
provisioned disk. On an idle machine that takes 2 to 8 s; while the tests run and read their own
cold binaries it took 10 to 19 s in every run (the two tests of crates/puddle-host/tests/canary_github_auth.rs)
and 30 s or more in one, which was the test's whole budget. Measured on windows-2025 with nine GB of
files read at the same time: a first node start took 71 to 90 s, and one after this warm-up 0.04 to 0.06 s.
Runs the real kind of start (`node -e` loading a module) twice and prints both times.

PowerShell 7 (pwsh); keep this file ASCII-only.
#>
$node = (Get-Command node -ErrorAction Stop).Source
foreach ($n in 1, 2) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $null = & $node -e "require('node:module'); require('node:fs'); process.stdout.write('ok')"
    if ($LASTEXITCODE) { throw "node start $n exited $LASTEXITCODE" }
    '{0} start {1}: {2:0.000} s' -f $node, $n, $sw.Elapsed.TotalSeconds
}
