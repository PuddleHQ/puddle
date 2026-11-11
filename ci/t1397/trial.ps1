# SPDX-License-Identifier: GPL-3.0-or-later
# Prepare one sandbox trial on the runner and export its environment for the test step.
param([Parameter(Mandatory)][string]$Trial)
$ErrorActionPreference = 'Stop'
$rt = $env:RUNNER_TEMP
"trial=$Trial"
whoami /groups | Select-String 'Mandatory Label|Administrators'
$ifeoStrong = @('DisallowChildProcessCreation','BlockDynamicCode','DisableExtensionPoints','DisableNonSystemFonts',
  'BlockRemoteImageLoads','BlockLowLabelImageLoads','PreferSystem32','StrictHandle','ForceRelocateImages','BottomUp',
  'HighEntropy','SEHOP','TerminateOnError','UserShadowStack','UserShadowStackStrictMode')
switch -Regex ($Trial) {
  '^base$' { }
  '^strong$' {
    Set-ProcessMitigation -Name msb.exe -Enable $ifeoStrong
    "PUDDLE_SANDBOX_TRIAL_JOB=ui,active1" >> $env:GITHUB_ENV
  }
  '^(restricted|rsids)$' {
    # The runner user is an administrator: folders it creates grant Administrators, which a restricted
    # token holds deny-only. Grant the user's own SID, as a normal (non-admin) install has it.
    icacls $rt /grant "$($env:USERNAME):(OI)(CI)F" | Out-Host
    "PUDDLE_SANDBOX_TRIAL=$Trial" >> $env:GITHUB_ENV
  }
  '^low$' {
    # Everything the runtime writes lives under the runner temp folder (run home, temp, extracted tests).
    icacls $rt /setintegritylevel '(OI)(CI)low' | Out-Host
    New-Item -ItemType Directory -Force "$rt\lowtmp" | Out-Null
    "PUDDLE_SANDBOX_TRIAL=low" >> $env:GITHUB_ENV
    "TEMP=$rt\lowtmp" >> $env:GITHUB_ENV
    "TMP=$rt\lowtmp" >> $env:GITHUB_ENV
  }
  '^lowbox$' {
    # ALL APPLICATION PACKAGES: every AppContainer, so the trial needs no SID lookup.
    icacls $rt /grant '*S-1-15-2-1:(OI)(CI)F' | Out-Host
    icacls $env:GITHUB_WORKSPACE /grant '*S-1-15-2-1:(OI)(CI)RX' | Out-Host
    New-Item -ItemType Directory -Force "$rt\actmp" | Out-Null
    "PUDDLE_SANDBOX_TRIAL=lowbox:puddle-trial:hypervisorPlatform,internetClient,internetClientServer,privateNetworkClientServer" >> $env:GITHUB_ENV
    "TEMP=$rt\actmp" >> $env:GITHUB_ENV
    "TMP=$rt\actmp" >> $env:GITHUB_ENV
  }
  default { throw "unknown trial $Trial" }
}
Get-ProcessMitigation -Name msb.exe -ErrorAction SilentlyContinue | Out-String -Width 200 | Select-String -Pattern 'ON' | Out-Host
