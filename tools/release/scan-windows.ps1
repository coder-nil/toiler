# 发布前扫描真实应用与两种安装器；不修改 Defender 配置，也不自动恢复隔离文件。
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$ReleaseDirectory
)

$ErrorActionPreference = 'Stop'
$release = (Resolve-Path -LiteralPath $ReleaseDirectory).Path
$status = Get-MpComputerStatus
if (-not $status.AntivirusEnabled -or -not $status.AMServiceEnabled) {
    throw 'Defender 不可用，不能把未扫描产物作为扫描通过发布。'
}
Update-MpSignature
$status = Get-MpComputerStatus
Write-Host "Defender 引擎：$($status.AMEngineVersion)，病毒库：$($status.AntivirusSignatureVersion)"

$scanner = Join-Path $env:ProgramFiles 'Windows Defender/MpCmdRun.exe'
if (-not (Test-Path -LiteralPath $scanner -PathType Leaf)) {
    throw '找不到 Defender 命令行扫描器。'
}
$app = Join-Path $release 'toiler-desktop.exe'
$nsis = @(Get-ChildItem -Path (Join-Path $release 'bundle/nsis/*.exe') -File)
$msi = @(Get-ChildItem -Path (Join-Path $release 'bundle/msi/*.msi') -File)
if (-not (Test-Path -LiteralPath $app -PathType Leaf) -or $nsis.Count -eq 0 -or $msi.Count -eq 0) {
    throw '应用或安装包缺失（也可能已被实时防护隔离），停止发布。'
}
$files = @($app) + @($nsis.FullName) + @($msi.FullName)
foreach ($file in $files) {
    Write-Host "扫描：$file"
    # 禁止自动修复，仅报告扫描结果；实时防护仍保持原状。
    & $scanner -Scan -ScanType 3 -File $file -DisableRemediation
    if ($LASTEXITCODE -ne 0) {
        throw "Defender 扫描未通过：$file，退出码 $LASTEXITCODE；请检查扫描输出并向微软提交样本。"
    }
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
        throw "扫描后文件缺失：$file，停止发布。"
    }
    Get-FileHash -LiteralPath $file -Algorithm SHA256 | Format-List
}
