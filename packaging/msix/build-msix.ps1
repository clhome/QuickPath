<#
.SYNOPSIS
    将 WindPath 打包为 Microsoft Store 规范的 MSIX 格式安装包。
.DESCRIPTION
    读取 packaging/msix/AppxManifest.xml 与 Assets 目录，
    将指定 Exe 封装为可直接提交微软 Partner Center 的 .msix 包。
#>

[CmdletBinding()]
param(
    [string]$ExePath = "target/release/windpath.exe",
    [string]$OutputDir = "dist",
    [string]$OutputFileName = "WindPath.msix",
    [string]$Version = "",
    [string]$IdentityName = "",
    [string]$Publisher = "",
    [string]$PublisherDisplayName = ""
)

$ErrorActionPreference = "Stop"

# 1. 校验输入可执行文件
if (-not (Test-Path $ExePath)) {
    Write-Error "待打包可执行文件不存在: $ExePath"
    exit 1
}

# 2. 解析版本号（若未显式指定，从 Cargo.toml 自动解析）
if (-not $Version) {
    if (Test-Path "Cargo.toml") {
        $cargoContent = Get-Content "Cargo.toml" -Raw
        if ($cargoContent -match '(?m)^version\s*=\s*"([^"]+)"') {
            $Version = $Matches[1]
        }
    }
}
if (-not $Version) {
    $Version = "1.0.0"
}
# 清理预发布后缀（如 1.2.6-rc.1 -> 1.2.6）
$cleanVersion = ($Version -replace '^v', '') -replace '(?i)(-rc|-alpha|-beta|-pre).*$', ''
# MSIX 要求四段式版本号：Major.Minor.Build.Revision (如 1.2.6.0)
$parts = $cleanVersion.Split('.')
while ($parts.Count -lt 4) {
    $parts += "0"
}
$quadVersion = ($parts[0..3] -join '.')
Write-Host "MSIX 四段式版本号: $quadVersion"

# 3. 解析发布者与身份信息（支持环境变量覆盖）
if (-not $IdentityName) {
    $IdentityName = if ($env:MS_STORE_IDENTITY_NAME) { $env:MS_STORE_IDENTITY_NAME } else { "WindPath" }
}
if (-not $Publisher) {
    $Publisher = if ($env:MS_STORE_PUBLISHER) { $env:MS_STORE_PUBLISHER } else { "CN=WindPath" }
}
if (-not $PublisherDisplayName) {
    $PublisherDisplayName = if ($env:MS_STORE_PUBLISHER_DISPLAY_NAME) { $env:MS_STORE_PUBLISHER_DISPLAY_NAME } else { "WindPath" }
}

Write-Host "MSIX Identity Name: $IdentityName"
Write-Host "MSIX Publisher: $Publisher"
Write-Host "MSIX Publisher Display Name: $PublisherDisplayName"

# 4. 定位 Windows SDK makeappx.exe
$makeappx = Get-Command "makeappx.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1
if (-not $makeappx) {
    $kitSearch = "C:\Program Files (x86)\Windows Kits\10\bin"
    if (Test-Path $kitSearch) {
        $found = Get-ChildItem -Path $kitSearch -Filter "makeappx.exe" -Recurse -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\makeappx\.exe$' } |
            Sort-Object FullName -Descending |
            Select-Object -First 1 -ExpandProperty FullName
        if ($found) {
            $makeappx = $found
        }
    }
}

if (-not $makeappx -or -not (Test-Path $makeappx)) {
    Write-Error "未找到 Windows SDK makeappx.exe，请确保已安装 Windows 10/11 SDK"
    exit 1
}
Write-Host "使用 makeappx: $makeappx"

# 5. 组装 staging 暂存目录
$stagingDir = Join-Path $env:TEMP ("qp_msix_staging_" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $stagingDir | Out-Null

try {
    # 5.1 复制可执行文件为规范命名 WindPath.exe
    $destExe = Join-Path $stagingDir "WindPath.exe"
    Copy-Item -Path $ExePath -Destination $destExe -Force

    # 5.2 复制 Assets
    $assetsSrc = "packaging/msix/Assets"
    if (-not (Test-Path $assetsSrc)) {
        Write-Error "未找到 Assets 目录: $assetsSrc"
        exit 1
    }
    $assetsDest = Join-Path $stagingDir "Assets"
    Copy-Item -Path $assetsSrc -Destination $assetsDest -Recurse -Force

    # 5.3 模板渲染 AppxManifest.xml
    $manifestTplPath = "packaging/msix/AppxManifest.xml"
    if (-not (Test-Path $manifestTplPath)) {
        Write-Error "未找到 AppxManifest.xml 模板: $manifestTplPath"
        exit 1
    }
    $manifestContent = Get-Content $manifestTplPath -Raw -Encoding utf8
    $manifestRendered = $manifestContent `
        -replace '\$\{IDENTITY_NAME\}', $IdentityName `
        -replace '\$\{PUBLISHER\}', $Publisher `
        -replace '\$\{VERSION\}', $quadVersion `
        -replace '\$\{PUBLISHER_DISPLAY_NAME\}', $PublisherDisplayName

    $manifestDest = Join-Path $stagingDir "AppxManifest.xml"
    [System.IO.File]::WriteAllText($manifestDest, $manifestRendered, [System.Text.Encoding]::UTF8)

    # 6. 执行 makeappx 打包
    if (-not (Test-Path $OutputDir)) {
        New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
    }
    $targetMsix = Join-Path $OutputDir $OutputFileName
    Write-Host "正在生成 MSIX 安装包: $targetMsix ..."

    $packArgs = @("pack", "/d", $stagingDir, "/p", $targetMsix, "/o")
    & $makeappx @packArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Error "makeappx 执行失败，退出码: $LASTEXITCODE"
        exit 1
    }

    Write-Host "成功生成 MSIX 产物: $targetMsix (大小: $((Get-Item $targetMsix).Length) 字节)"
}
finally {
    # 清理暂存目录
    if (Test-Path $stagingDir) {
        Remove-Item -Path $stagingDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

