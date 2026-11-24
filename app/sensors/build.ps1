$ErrorActionPreference = 'Stop'
$resources = Join-Path $PSScriptRoot '../src-tauri/resources/sensors'
$framework = Join-Path $env:WINDIR 'Microsoft.NET/Framework64/v4.0.30319'
$library = Join-Path $resources 'LibreHardwareMonitorLib.dll'
$output = Join-Path $resources 'HardwareSensors.exe'
& "$framework/csc.exe" /nologo /target:exe /optimize+ /platform:x64 "/out:$output" "/reference:$library" /reference:System.Web.Extensions.dll (Join-Path $PSScriptRoot 'Program.cs') (Join-Path $PSScriptRoot 'SensorReader.cs')
if ($LASTEXITCODE) { throw 'Could not compile the LibreHardwareMonitor sensor helper.' }
