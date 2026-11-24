$ErrorActionPreference = 'Stop'
$appDir = Join-Path $PSScriptRoot 'app'
$nativeDir = Join-Path $appDir 'src-tauri'
$outputDir = Join-Path $PSScriptRoot 'release/System Bio Mon'

Push-Location $appDir
try {
    if (!(Test-Path -LiteralPath 'node_modules/.bin/tauri.cmd')) {
        & npm.cmd ci --no-fund --no-audit
        if ($LASTEXITCODE) { throw 'Tauri CLI installation failed.' }
    }
    & ./sensors/build.ps1
    & ./node_modules/.bin/tauri.cmd build --bundles nsis
    if ($LASTEXITCODE) { throw 'Windows release build failed.' }
} finally {
    Pop-Location
}

$version = (Get-Content -Raw (Join-Path $nativeDir 'tauri.conf.json') | ConvertFrom-Json).version
$build = Join-Path $nativeDir 'target/release'
New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
$portable = Join-Path $build 'system-bio-mon.exe'
try {
    Copy-Item -LiteralPath $portable -Destination (Join-Path $outputDir 'System Bio Mon.exe') -Force
} catch [System.IO.IOException] {
    if (($_.Exception.HResult -band 0xffff) -notin 32, 33) { throw }
    # A running portable app may still need its original sensor binaries.
    $outputDir = Join-Path $outputDir $version
    New-Item -ItemType Directory -Force -Path $outputDir | Out-Null
    Copy-Item -LiteralPath $portable -Destination (Join-Path $outputDir 'System Bio Mon.exe') -Force
}
$resources = Join-Path $outputDir 'resources'
New-Item -ItemType Directory -Force -Path $resources | Out-Null
Copy-Item -LiteralPath (Join-Path $build 'resources/sensors') -Destination $resources -Recurse -Force
Copy-Item -LiteralPath (Join-Path $build 'resources/presentmon') -Destination $resources -Recurse -Force
Copy-Item -LiteralPath (Join-Path $build 'resources/AMD-ADLX-LICENSE.txt') -Destination $resources -Force
Copy-Item -LiteralPath (Join-Path $build 'resources/RUST-NOTICES.txt') -Destination $resources -Force
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'LICENSE') -Destination (Join-Path $outputDir 'LICENSE.txt') -Force
Copy-Item -LiteralPath (Join-Path $build "bundle/nsis/System Bio Mon_${version}_x64-setup.exe") -Destination (Join-Path $outputDir 'System-Bio-Mon-Setup.exe') -Force
$zipItems = 'System Bio Mon.exe', 'resources', 'LICENSE.txt' | ForEach-Object { Join-Path $outputDir $_ }
Compress-Archive -LiteralPath $zipItems -DestinationPath (Join-Path $outputDir 'System-Bio-Mon-Portable.zip') -CompressionLevel Optimal -Force
Write-Host "Windows release ready in $outputDir"
