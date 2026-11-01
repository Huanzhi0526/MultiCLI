param([switch]$RequireNativePerl)
$ErrorActionPreference = 'Stop'
$multicliAppRoot = Split-Path (Split-Path $PSScriptRoot -Parent) -Parent
$multicliWorkspaceRoot = Split-Path $multicliAppRoot -Parent
$multicliToolsRoot = Join-Path $multicliWorkspaceRoot 'scratch/tools'
if (Test-Path (Join-Path $multicliToolsRoot 'cargo/bin/cargo.exe')) {
    $env:CARGO_HOME = Join-Path $multicliToolsRoot 'cargo'
    $env:RUSTUP_HOME = Join-Path $multicliToolsRoot 'rustup'
    $env:PATH = "$env:CARGO_HOME\bin;$env:PATH"
    $env:COREPACK_HOME = Join-Path $multicliToolsRoot 'corepack'
}
$env:CARGO_BUILD_JOBS = '2'
$multicliPerlCandidates = @(
    (Join-Path $multicliToolsRoot 'strawberry/perl/bin/perl.exe'),
    'C:\Strawberry\perl\bin\perl.exe'
)
$multicliPerlCommand = Get-Command perl.exe -ErrorAction SilentlyContinue
if ($multicliPerlCommand) { $multicliPerlCandidates += $multicliPerlCommand.Source }
if ($env:PERL) { $multicliPerlCandidates = @($env:PERL) + $multicliPerlCandidates }
$multicliPerlFound = $false
foreach ($multicliPerl in ($multicliPerlCandidates | Select-Object -Unique)) {
    if (-not (Test-Path -LiteralPath $multicliPerl -PathType Leaf)) { continue }
    & $multicliPerl -MIPC::Cmd -MLocale::Maketext::Simple -e 'exit 0' 2>$null
    if ($LASTEXITCODE -eq 0) {
        $env:PERL = $multicliPerl
        $multicliPerlFound = $true
        break
    }
}
if ($RequireNativePerl -and -not $multicliPerlFound) {
    throw 'Native OpenSSL builds require complete Perl with IPC::Cmd and Locale::Maketext::Simple. Prepare project-local Strawberry Perl or install it on the build host.'
}
