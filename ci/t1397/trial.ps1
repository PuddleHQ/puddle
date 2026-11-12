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
    # Only the run home and the runtime's temp folder get the low label. A low label on a folder
    # with executables would start them at low integrity (Windows takes the lower of parent and file).
    New-Item -ItemType Directory -Force "$rt\pvm", "$rt\lowtmp" | Out-Null
    icacls "$rt\pvm" /setintegritylevel '(OI)(CI)low' | Out-Host
    icacls "$rt\lowtmp" /setintegritylevel '(OI)(CI)low' | Out-Host
    "PUDDLE_SANDBOX_TRIAL=low" >> $env:GITHUB_ENV
    "PUDDLE_SANDBOX_TRIAL_TEMP=$rt\lowtmp" >> $env:GITHUB_ENV
  }
  '^lowbox$' {
    New-Item -ItemType Directory -Force "$rt\pvm", "$rt\actmp" | Out-Null
    foreach ($d in "$rt\pvm", "$rt\actmp") { icacls $d /grant '*S-1-15-2-1:(OI)(CI)F' | Out-Host }
    foreach ($d in "$rt\msb", "$rt\bundle", $env:GITHUB_WORKSPACE) { icacls $d /grant '*S-1-15-2-1:(OI)(CI)RX' | Out-Host }
    "PUDDLE_SANDBOX_TRIAL=lowbox:puddle-trial:hypervisorPlatform,internetClient,internetClientServer,privateNetworkClientServer" >> $env:GITHUB_ENV
    "PUDDLE_SANDBOX_TRIAL_TEMP=$rt\actmp" >> $env:GITHUB_ENV
  }
  default { throw "unknown trial $Trial" }
}
Get-ProcessMitigation -Name msb.exe -ErrorAction SilentlyContinue | Out-String -Width 200 | Select-String -Pattern 'ON' | Out-Host
