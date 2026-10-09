# ==========================================================
# QuickPath 测试机现场诊断脚本 (WPS 窗口侦测工具)
# 使用方法：在测试机打开 PowerShell，运行此脚本，并在 10 秒内点击激活 WPS 另存为窗口
# ==========================================================

$code = @"
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;

public class QpSpy {
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetWindowTextW(IntPtr hWnd, StringBuilder text, int count);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetClassNameW(IntPtr hWnd, StringBuilder text, int count);

    [DllImport("user32.dll")]
    public static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint processId);

    [DllImport("user32.dll")]
    public static extern bool IsWindowVisible(IntPtr hWnd);

    public delegate bool EnumChildProc(IntPtr hWnd, IntPtr lParam);

    [DllImport("user32.dll")]
    public static extern bool EnumChildWindows(IntPtr hWnd, EnumChildProc proc, IntPtr lParam);

    public static List<string> GetChildren(IntPtr parent) {
        List<string> list = new List<string>();
        EnumChildWindows(parent, (child, lparam) => {
            if (IsWindowVisible(child)) {
                StringBuilder cName = new StringBuilder(256);
                GetClassNameW(child, cName, 256);
                list.Add(cName.ToString());
            }
            return true;
        }, IntPtr.Zero);
        return list;
    }
}
"@

Add-Type -TypeDefinition $code

Clear-Host
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "QuickPath 现场窗口排错侦测工具" -ForegroundColor Yellow
Write-Host "请在接下来 15 秒内，用鼠标点击一下 WPS 的【另存为】对话框使其获得焦点..." -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Cyan

for ($i = 15; $i -ge 1; $i--) {
    $hwnd = [QpSpy]::GetForegroundWindow()
    $pidOut = 0
    [QpSpy]::GetWindowThreadProcessId($hwnd, [ref]$pidOut)

    $pName = ""
    $pPath = ""
    try {
        $p = Get-Process -Id $pidOut -ErrorAction SilentlyContinue
        $pName = $p.ProcessName
        $pPath = $p.Path
    } catch {}

    $sbTitle = New-Object System.Text.StringBuilder 512
    [QpSpy]::GetWindowTextW($hwnd, $sbTitle, 512) | Out-Null
    $title = $sbTitle.ToString()

    $sbClass = New-Object System.Text.StringBuilder 256
    [QpSpy]::GetClassNameW($hwnd, $sbClass, 256) | Out-Null
    $className = $sbClass.ToString()

    if ($pName -ne "" -and $pName -ne "powershell" -and $pName -ne "pwsh") {
        Write-Host ""
        Write-Host "-------------------- 捕获到前台窗口 --------------------" -ForegroundColor Yellow
        Write-Host "窗口句柄 (HWND) : $hwnd (0x$([System.Convert]::ToString($hwnd.ToInt64(), 16)))"
        Write-Host "所属进程名称    : $pName" -ForegroundColor Cyan
        Write-Host "进程完整路径    : $pPath"
        Write-Host "窗口类名 (Class): $className" -ForegroundColor Magenta
        Write-Host "窗口标题 (Title): $title" -ForegroundColor Green

        # 枚举子控件
        $children = [QpSpy]::GetChildren($hwnd)
        Write-Host "可见子控件类名  : ($($children.Count) 个)"
        $children | Group-Object | ForEach-Object {
            Write-Host "   - $($_.Name) (数量: $($_.Count))" -ForegroundColor Gray
        }

        # 规则预诊断
        $isWpsProc = ($pName.ToLower().Contains("wps") -or $pName.ToLower().Contains("kso") -or $pName.ToLower().Contains("kingsoft"))
        Write-Host ">>> 诊断分析结果:" -ForegroundColor Yellow
        Write-Host "   1. 是否命中 WPS 进程规则 : $isWpsProc"
        $hasList = ($children -contains "DirectUIHWND" -or $children -contains "SHELLDLL_DefView")
        Write-Host "   2. 是否包含文件列表视图  : $hasList"
        $hasEdit = ($children -contains "Edit" -or $children -contains "ComboBox" -or $children -contains "ComboBoxEx32")
        Write-Host "   3. 是否包含输入框控件    : $hasEdit"
        Write-Host "--------------------------------------------------------"
    }

    Write-Host "倒计时 $i 秒 (请切换窗口)..."
    Start-Sleep -Seconds 1
}

Write-Host "侦测结束。如果抓取到了 WPS 另存为窗口，请将上方打印的信息反馈。" -ForegroundColor Green
