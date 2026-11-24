$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$installer = Join-Path ([IO.Path]::GetTempPath()) ('PawnIO-' + [guid]::NewGuid().ToString('N') + '.exe')
$expectedSize = 3410960
$expectedHash = '1F519A22E47187F70A1379A48CA604981C4FCF694F4E65B734AAA74A9FBA3032'
$lock = $null
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    $request = [Net.WebRequest]::Create('https://github.com/namazso/PawnIO.Setup/releases/download/2.2.0/PawnIO_setup.exe')
    $request.UserAgent = 'System Bio Mon'
    $request.Timeout = 30000
    $response = $request.GetResponse()
    try {
        if ($response.ContentLength -ne $expectedSize) { throw 'Unexpected driver download size.' }
        $stream = $response.GetResponseStream()
        $dest = [IO.File]::Open($installer, 'CreateNew', 'Write', 'None')
        try { $stream.CopyTo($dest) } finally { $dest.Dispose(); $stream.Dispose() }
    } finally { $response.Dispose() }
    $lock = [IO.File]::Open($installer, 'Open', 'Read', 'Read')
    if ((Get-FileHash -InputStream $lock -Algorithm SHA256).Hash -ne $expectedHash) { throw 'Driver checksum verification failed.' }
    if ((Get-AuthenticodeSignature -LiteralPath $installer).Status -ne 'Valid') { throw 'Windows could not verify the driver installer signature.' }
    $setup = Start-Process -FilePath $installer -Verb RunAs -PassThru -Wait
    if ($setup.ExitCode -ne 0) { throw 'Driver setup was cancelled or failed.' }
    if (!(Test-Path 'HKLM:\SYSTEM\CurrentControlSet\Services\PawnIO')) { throw 'Driver setup finished without installing PawnIO.' }
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
} finally {
    if ($lock) { $lock.Dispose() }
    if ([IO.File]::Exists($installer)) { [IO.File]::Delete($installer) }
}
