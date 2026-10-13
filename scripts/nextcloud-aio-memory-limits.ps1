[CmdletBinding(DefaultParameterSetName = 'Status')]
param(
    [Parameter(Mandatory = $true, ParameterSetName = 'Apply')]
    [switch] $Apply,

    [Parameter(Mandatory = $true, ParameterSetName = 'Restore')]
    [switch] $Restore
)

$ErrorActionPreference = 'Stop'
$toolPath = Join-Path $PSScriptRoot 'nextcloud_aio_memory_limits.py'
$action = if ($Apply) { '--apply' } elseif ($Restore) { '--restore' } else { $null }
$python = Get-Command python -ErrorAction Stop
$arguments = @($toolPath)
if ($action) { $arguments += $action }
& $python.Source @arguments
if ($LASTEXITCODE -ne 0) {
    throw 'Nextcloud AIO memory-limit helper failed. See its redacted JSON error above.'
}
