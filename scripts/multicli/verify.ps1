param([ValidateSet('Frontend','Rust','Test','Package','All')][string]$Scope = 'All')
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'enter-env.ps1') -RequireNativePerl:($Scope -ne 'Frontend')
Push-Location $multicliAppRoot
try {
    if ($Scope -in @('Frontend','All')) {
        & corepack pnpm build
        if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed' }
        & node --experimental-strip-types --test tests/platform-build.test.ts
        if ($LASTEXITCODE -ne 0) { throw 'Build boundary checks failed' }
    }
    if ($Scope -in @('Rust','All')) {
        & cargo check --locked --manifest-path src-tauri/Cargo.toml
        if ($LASTEXITCODE -ne 0) { throw 'Rust check failed' }
    }
    if ($Scope -in @('Test','All')) {
        & corepack pnpm exec vitest run src/multicli
        if ($LASTEXITCODE -ne 0) { throw 'MultiCLI frontend tests failed' }
        $multicliSavedProfile = $env:MULTICLI_PROFILE
        try {
            $env:MULTICLI_PROFILE = 'test'
            & cargo test --locked --manifest-path src-tauri/Cargo.toml --lib 'multicli::' -- --test-threads=1
            if ($LASTEXITCODE -ne 0) { throw 'MultiCLI domain tests failed' }
            & cargo test --locked --manifest-path src-tauri/Cargo.toml --lib multicli_bootstrap -- --test-threads=1
            if ($LASTEXITCODE -ne 0) { throw 'MultiCLI native tests failed' }
            & cargo test --locked --manifest-path src-tauri/Cargo.toml --example multicli_fixture -- --test-threads=1
            if ($LASTEXITCODE -ne 0) { throw 'MultiCLI fixture protection tests failed' }
        } finally {
            if ($null -eq $multicliSavedProfile) { Remove-Item Env:MULTICLI_PROFILE -ErrorAction SilentlyContinue }
            else { $env:MULTICLI_PROFILE = $multicliSavedProfile }
        }
    }
    if ($Scope -in @('Package','All')) {
        & corepack pnpm exec tauri build --bundles nsis --no-sign -- --locked
        if ($LASTEXITCODE -ne 0) { throw 'Windows installer build failed' }
    }
} finally { Pop-Location }
