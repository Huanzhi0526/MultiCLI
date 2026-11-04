param(
    [ValidateSet('Frontend', 'Rust', 'Test', 'Package', 'All')]
    [string]$Scope = 'All',
    # Run every requested scope even after one fails. The summary still exits 1.
    [switch]$ContinueOnError
)

# Unified verification entry for MultiCLI (S07).
#
# It is a thin, honest wrapper around scripts/multicli/verify.ps1:
#   * it runs only the scopes that were actually requested,
#   * every scope that was not run is printed as NOT_RUN (never a silent green),
#   * a failing scope marks the whole run FAIL and exits 1.
#
# It does not add retries, does not relax assertions and does not treat a build
# artifact as a passed test.

$ErrorActionPreference = 'Stop'

$appRoot = Split-Path $PSScriptRoot -Parent
$inner = Join-Path $PSScriptRoot 'multicli\verify.ps1'
if (-not (Test-Path -LiteralPath $inner)) {
    throw "verification script not found: $inner"
}

# `pnpm exec <tool>` resolves node_modules/.bin through PATH in this workspace;
# without it the frontend step reports a missing `vitest` even though the
# package is installed. Adding it is local to this process.
$binDirectory = Join-Path $appRoot 'node_modules\.bin'
if (Test-Path -LiteralPath $binDirectory) {
    $env:PATH = "$binDirectory;$env:PATH"
}

$allScopes = @('Frontend', 'Rust', 'Test', 'Package')
$planned = if ($Scope -eq 'All') { $allScopes } else { @($Scope) }

$results = [ordered]@{}
foreach ($name in $allScopes) { $results[$name] = 'NOT_RUN' }

Write-Host "MultiCLI verification"
Write-Host "  app root : $appRoot"
Write-Host "  requested: $Scope"
$node = Get-Command node -ErrorAction SilentlyContinue
if ($node) { Write-Host "  node     : $($node.Source) $(& node --version)" }
if (-not (Get-Command corepack -ErrorAction SilentlyContinue)) {
    Write-Host "  WARNING  : corepack is not on PATH; the pnpm-based steps will fail instead of being skipped."
}
# Some IDE hosts inject a Node language/safe-delete shim through NODE_OPTIONS.
# The shim can block Vite's normal `dist` cleanup with SAFE_DELETE_BULK_CONFIRM_REQUIRED.
# It is not part of this project; report it instead of silently changing the
# caller's environment.
if ($env:NODE_OPTIONS -match 'node-language-shim|safe-delete') {
    Write-Host "  WARNING  : NODE_OPTIONS contains a host-injected shim ($env:NODE_OPTIONS)."
    Write-Host "             If the Package scope fails with SAFE_DELETE_BULK_CONFIRM_REQUIRED, clear NODE_OPTIONS"
    Write-Host "             and CODEBUDDY_SAFE_DELETE_BULK_GUARD for the build process."
}

$failed = $false
foreach ($name in $planned) {
    Write-Host ""
    Write-Host "[verify] RUN $name"
    if ($name -eq 'Package') {
        Write-Host "[verify] Package builds the NSIS installer; this is the slowest scope."
    }
    try {
        & $inner -Scope $name
        if (-not $?) { throw 'the verification script reported a failure' }
        $results[$name] = 'PASS'
        Write-Host "[verify] $name PASS"
    }
    catch {
        $results[$name] = 'FAIL'
        $failed = $true
        Write-Host "[verify] $name FAIL: $($_.Exception.Message)"
        if (-not $ContinueOnError) { break }
    }
}

Write-Host ""
Write-Host "SCOPE SUMMARY"
foreach ($name in $allScopes) {
    Write-Host ("  {0,-9} {1}" -f $name, $results[$name])
}
foreach ($name in $allScopes) {
    if ($results[$name] -eq 'NOT_RUN') {
        Write-Host "  NOTE: $name was not executed in this run (NOT_RUN is not a pass)."
    }
}

if ($failed) { exit 1 }
exit 0
