<#
.SYNOPSIS
    将 WindPath 编译打包为标准 Windows Installer (.msi) 安装包。
.DESCRIPTION
    调用 WiX Toolset 将 WindPath.exe 封装为支持开机快捷方式、Program Files 安装与控制面板卸载的标准 MSI 安装包。
    自动适配 GitHub Actions 环境（WiX v3.11 或 .NET WiX v4/v5）。
#>

[CmdletBinding()]
param(
    [string]$ExePath = "target/release/windpath.exe",
    [string]$OutputDir = "dist",
    [string]$OutputFileName = "WindPath.msi",
    [string]$Version = ""
)

$ErrorActionPreference = "Stop"

# 1. 校验输入可执行文件
if (-not (Test-Path $ExePath)) {
    Write-Error "待打包可执行文件不存在: $ExePath"
    exit 1
}
$resolvedExe = (Resolve-Path $ExePath).Path
$rootDir = (Resolve-Path ".").Path

# 2. 解析版本号（WiX Product Version 必须为 1 到 3 段十进制数，如 1.2.6）
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
$cleanVersion = ($Version -replace '^v', '') -replace '(?i)(-rc|-alpha|-beta|-pre).*$', ''
$vParts = $cleanVersion.Split('.')
if ($vParts.Count -gt 3) {
    $cleanVersion = ($vParts[0..2] -join '.')
}
elseif ($vParts.Count -eq 1) {
    $cleanVersion = "$cleanVersion.0.0"
}
elseif ($vParts.Count -eq 2) {
    $cleanVersion = "$cleanVersion.0"
}
Write-Host "MSI Product Version: $cleanVersion"

# 3. 准备输出目录
if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
}
$targetMsi = Join-Path (Resolve-Path $OutputDir).Path $OutputFileName
$wxsFile = Join-Path $rootDir "packaging/msi/windpath.wxs"

if (-not (Test-Path $wxsFile)) {
    Write-Error "未找到 WiX 配置文件: $wxsFile"
    exit 1
}

# 4. 探测 WiX 编译器（WiX 3.11 candle/light 或 WiX 4/5 CLI）
$wixV3Candle = Get-Command "candle.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1
$wixV3Light = Get-Command "light.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1

if (-not $wixV3Candle) {
    # 尝试在 GitHub Actions 标准安装路径寻找 WiX 3.11
    $wix3Path = "C:\Program Files (x86)\WiX Toolset v3.11\bin"
    if (Test-Path (Join-Path $wix3Path "candle.exe")) {
        $wixV3Candle = Join-Path $wix3Path "candle.exe"
        $wixV3Light = Join-Path $wix3Path "light.exe"
    }
}

$wixCli = Get-Command "wix.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1
if (-not $wixCli) {
    $dotnetToolsWix = Join-Path $env:USERPROFILE ".dotnet\tools\wix.exe"
    if (Test-Path $dotnetToolsWix) {
        $wixCli = $dotnetToolsWix
    }
}

# 5. 执行编译
if ($wixV3Candle -and $wixV3Light) {
    Write-Host "检测到 WiX v3 工具链: $wixV3Candle"
    $objFile = Join-Path $env:TEMP ("qp_wix_" + [System.Guid]::NewGuid().ToString("N") + ".wixobj")
    try {
        Write-Host "正在编译 WiX 源码..."
        & $wixV3Candle -arch x64 "-dVersion=$cleanVersion" "-dSourceDir=$rootDir" "-dExePath=$resolvedExe" $wxsFile -out $objFile
        if ($LASTEXITCODE -ne 0) {
            Write-Error "WiX candle 编译失败，退出码: $LASTEXITCODE"
            exit 1
        }

        Write-Host "正在链接生成 MSI..."
        & $wixV3Light -spdb $objFile -out $targetMsi
        if ($LASTEXITCODE -ne 0) {
            Write-Error "WiX light 链接失败，退出码: $LASTEXITCODE"
            exit 1
        }
    }
    finally {
        if (Test-Path $objFile) {
            Remove-Item -Force $objFile -ErrorAction SilentlyContinue
        }
    }
}
elseif ($wixCli) {
    Write-Host "检测到 WiX CLI 工具链: $wixCli"
    Write-Host "正在构建 MSI 安装包..."
    & $wixCli build -arch x64 "-d" "Version=$cleanVersion" "-d" "SourceDir=$rootDir" "-d" "ExePath=$resolvedExe" $wxsFile -o $targetMsi
    if ($LASTEXITCODE -ne 0) {
        Write-Error "WiX build 失败，退出码: $LASTEXITCODE"
        exit 1
    }
}
else {
    # 尝试通过 dotnet tool 在线安装 WiX
    $dotnet = Get-Command "dotnet.exe" -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source -First 1
    if ($dotnet) {
        Write-Host "未发现现有 WiX，尝试通过 dotnet tool 安装 WiX CLI..."
        & $dotnet tool install --global wix
        $wixCli = Join-Path $env:USERPROFILE ".dotnet\tools\wix.exe"
        if (Test-Path $wixCli) {
            Write-Host "WiX CLI 安装成功，正在构建 MSI..."
            & $wixCli build -arch x64 "-d" "Version=$cleanVersion" "-d" "SourceDir=$rootDir" "-d" "ExePath=$resolvedExe" $wxsFile -o $targetMsi
            if ($LASTEXITCODE -ne 0) {
                Write-Error "WiX build 失败，退出码: $LASTEXITCODE"
                exit 1
            }
        }
        else {
            Write-Error "未找到可用 WiX 工具链，且自动安装未就绪。"
            exit 1
        }
    }
    else {
        Write-Error "未找到 WiX Toolset（需 WiX v3.11 或 .NET wix tool）。请在环境预装 WiX。"
        exit 1
    }
}

if (-not (Test-Path $targetMsi)) {
    Write-Error "MSI 输出文件未能成功生成: $targetMsi"
    exit 1
}

Write-Host "成功生成 MSI 安装包: $targetMsi (大小: $((Get-Item $targetMsi).Length) 字节)"

