# Read-only check of the service installed by the installer integration test.
param([Parameter(Mandatory = $true)][string]$ExpectedDirectory)
$ErrorActionPreference = 'Stop'
$service = Get-Service -Name AtlasSearchService -ErrorAction Stop
try {
    $service.WaitForStatus('Running', [TimeSpan]::FromSeconds(30))
} finally { $service.Dispose() }
$registration = Get-CimInstance Win32_Service -Filter "Name='AtlasSearchService'"
$expected = Join-Path $ExpectedDirectory 'atlas-search-service.exe'
if ($registration.PathName.Trim('"') -ne $expected) {
    throw "Search service points outside this installation: $($registration.PathName)"
}
$pipe = [System.IO.Pipes.NamedPipeClientStream]::new('.', 'AtlasSearchService-v1', [System.IO.Pipes.PipeDirection]::InOut)
try {
    $pipe.Connect(10000)
    $payload = [System.Text.Encoding]::UTF8.GetBytes('{"Hello":{"protocol":1}}')
    $pipe.Write($payload, 0, $payload.Length)
    $pipe.Flush()
    $buffer = [byte[]]::new(4096)
    $read = $pipe.ReadAsync($buffer, 0, $buffer.Length)
    if (-not $read.Wait(10000)) { throw 'Search service did not answer its IPC handshake' }
    $response = [System.Text.Encoding]::UTF8.GetString($buffer, 0, $read.Result)
    if (-not $response) { throw 'Search service disconnected before delivering its IPC reply' }
    $reply = $response | ConvertFrom-Json
    if ($reply.Hello.protocol -ne 1) { throw "Search service uses an unexpected IPC protocol: $response" }
    Write-Host 'Installed Windows search service is running and answers protocol 1.'
} finally { $pipe.Dispose() }
