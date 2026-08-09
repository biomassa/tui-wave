<#
.SYNOPSIS
    Set up the Python environment the tui-wave 'py' process group needs, on Windows.

.DESCRIPTION
    The Windows release archive bundles the praatAudioTools scripts beside tui-wave.exe, so
    unlike macOS and Linux there is nothing to clone and nothing to configure: the editor finds
    them on its own. This script covers the one remaining piece — a Python virtual environment
    for the 46 processes in the 'py' group, which drive a Python helper and need numpy, scipy
    and soundfile.

    Everything else in tui-wave works without this. Run it only if you want that group.

    The venv is created where tui-wave looks for it and nowhere else, and the system Python is
    never modified: only this venv gets packages installed into it.

.PARAMETER Yes
    Take every prompt as yes.

.PARAMETER DryRun
    Print what would be run and change nothing.

.EXAMPLE
    .\setup-python.ps1
    .\setup-python.ps1 -Yes
    .\setup-python.ps1 -DryRun

.NOTES
    This is the Windows counterpart to section 4 of setup-environment.sh, and only that section.
    The tkinter probe and the optional analysis/ML package tiers in install.sh are deliberately
    not ported: they would more than double this script for processes a Windows user reaches
    last, and both are reachable by hand from the venv this creates.
#>

[CmdletBinding()]
param(
    [switch]$Yes,
    [switch]$DryRun
)

# Stop on the first real error rather than carrying on with a half-built venv.
$ErrorActionPreference = 'Stop'

# The three the 'py' group cannot run without. install.sh also offers sounddevice and pillow for
# three interactive editors; add them by hand into the same venv if you want those.
$Packages = @('numpy', 'scipy', 'soundfile')

function Write-Step ($Text) { Write-Host "`n==> $Text" -ForegroundColor Cyan }
function Write-Info ($Text) { Write-Host "    $Text" }
function Write-Ok   ($Text) { Write-Host "    $Text" -ForegroundColor Green }
function Write-Warn ($Text) { Write-Host "    $Text" -ForegroundColor Yellow }
function Die        ($Text) { Write-Host "    $Text" -ForegroundColor Red; exit 1 }

function Confirm-Step ($Question) {
    if ($Yes -or $DryRun) { return $true }
    $answer = Read-Host "    $Question [Y/n]"
    return ($answer -eq '' -or $answer -match '^[Yy]')
}

# Must agree with `config::config_home` and `praat::runner::state_dir`, which is what the editor
# itself consults. XDG_CONFIG_HOME wins there on every platform, Windows included, so it wins
# here too — otherwise a user who sets it would get a venv the app never looks at.
function Get-ConfigHome {
    if ($env:XDG_CONFIG_HOME) { return $env:XDG_CONFIG_HOME }
    if ($env:APPDATA)         { return $env:APPDATA }
    if ($env:USERPROFILE)     { return (Join-Path $env:USERPROFILE 'AppData\Roaming') }
    return '.'
}

# The first interpreter that can actually build a venv. `py -3` (the Python launcher) is tried
# first because it is what a python.org installer puts on PATH and it resolves the newest
# installed 3.x; `python`/`python3` cover a Store install, a conda base, or a hand-managed one.
#
# `python` on a bare Windows without Python is a Store *stub* that prints an advert and exits 9009,
# so "the command exists" proves nothing here — this runs `-c "import venv"` and believes only
# an interpreter that answers.
function Find-Python {
    foreach ($candidate in @(@('py', '-3'), @('python3'), @('python'))) {
        $exe = $candidate[0]
        $prefix = @($candidate | Select-Object -Skip 1)
        if (-not (Get-Command $exe -ErrorAction SilentlyContinue)) { continue }
        try {
            & $exe @prefix -c 'import venv' 2>$null | Out-Null
            if ($LASTEXITCODE -eq 0) { return ,@($exe) + $prefix }
        } catch { continue }
    }
    return $null
}

function Invoke-Step {
    param([string[]]$Command)
    Write-Info ($Command -join ' ')
    if ($DryRun) { return }
    & $Command[0] @($Command | Select-Object -Skip 1)
    if ($LASTEXITCODE -ne 0) { throw "command failed: $($Command -join ' ')" }
}

# --- Locate the venv -------------------------------------------------------------------------

$venv = Join-Path (Get-ConfigHome) 'tui-wave\praat\pyenv'
$venvPython = Join-Path $venv 'Scripts\python.exe'
$venvPip = Join-Path $venv 'Scripts\pip.exe'

Write-Step "Python backend (optional — the 46 processes in the 'py' group)"
Write-Info "these scripts drive a Python helper and need $($Packages -join ', ')"
Write-Info "everything else in tui-wave works without them"
Write-Info "venv: $venv"
if ($DryRun) { Write-Warn 'dry run — nothing will be changed' }

# --- Create it -------------------------------------------------------------------------------

if (Test-Path $venvPython) {
    Write-Ok 'venv already exists'
} else {
    $python = Find-Python
    if (-not $python) {
        Write-Warn 'no Python 3 with a working venv module was found'
        Write-Info 'Install one from https://www.python.org/downloads/windows/ (tick "Add'
        Write-Info 'python.exe to PATH"), then re-run this script.'
        Write-Info "The rest of tui-wave works without it — only the 'py' group is affected."
        exit 1
    }
    Write-Info "interpreter: $($python -join ' ')"
    if (-not (Confirm-Step 'Create the virtual environment and install the packages?')) {
        Write-Info 're-run this script later to add them'
        exit 0
    }
    if (-not $DryRun) {
        New-Item -ItemType Directory -Path (Split-Path $venv -Parent) -Force | Out-Null
    }
    Invoke-Step ($python + @('-m', 'venv', $venv))
    Write-Ok 'venv created'
}

# --- Populate it -----------------------------------------------------------------------------

# A pip too old to understand a modern wheel tag downloads an sdist and tries to compile scipy,
# which on Windows without a toolchain fails after a long wait. Not fatal on its own, though:
# the version the venv shipped is usually fine.
try {
    Invoke-Step @($venvPip, 'install', '--quiet', '--disable-pip-version-check', '--upgrade', 'pip')
} catch {
    Write-Warn 'could not upgrade pip; continuing with the version the venv shipped'
}

foreach ($package in $Packages) {
    Write-Info "installing $package"
    try {
        Invoke-Step @($venvPip, 'install', '--quiet', '--disable-pip-version-check', $package)
    } catch {
        Die "$package failed to install — the 'py' group needs all three"
    }
}

# Importing, not `pip list`: a wheel can install and still fail to load (a missing runtime, an
# architecture mismatch), and the group needs them to import.
if (-not $DryRun) {
    & $venvPython -c 'import numpy, scipy, soundfile' 2>$null
    if ($LASTEXITCODE -ne 0) {
        Die 'the venv was created but the packages did not import'
    }
    Write-Ok "$($Packages -join ', ') import cleanly"
}

Write-Step 'Done'
Write-Info "tui-wave finds this venv on its own — there is nothing to configure."
Write-Info "Praat itself is a separate install: https://www.fon.hum.uva.nl/praat/"
