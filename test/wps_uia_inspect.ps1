# ==========================================================
# QuickPath 测试机 UIA 元素侦测工具 (WPS 专用)
# 功能：枚举 WPS 对话框内所有 UI Automation 元素与属性
# ==========================================================

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class WinSpy {
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    public static extern int GetClassNameW(IntPtr hWnd, System.Text.StringBuilder text, int count);
}
"@

Clear-Host
Write-Host "==========================================================" -ForegroundColor Cyan
Write-Host "QuickPath WPS UIA 元素精确侦测工具" -ForegroundColor Yellow
Write-Host "请在 5 秒内切换到 WPS 的【另存为】对话框使其获得焦点..." -ForegroundColor Green
Write-Host "==========================================================" -ForegroundColor Cyan

for ($i = 5; $i -ge 1; $i--) {
    Write-Host "倒计时 $i 秒..."
    Start-Sleep -Seconds 1
}

$fgHwnd = [WinSpy]::GetForegroundWindow()
$sb = New-Object System.Text.StringBuilder 256
[WinSpy]::GetClassNameW($fgHwnd, $sb, 256) | Out-Null
$cls = $sb.ToString()

Write-Host "`n前台窗口 HWND: $fgHwnd (0x$([Convert]::ToString($fgHwnd.ToInt64(), 16))) | Class: $cls" -ForegroundColor Magenta

try {
    $element = [System.Windows.Automation.AutomationElement]::FromHandle($fgHwnd)
    if ($element -eq $null) {
        Write-Host "无法从该 HWND 创建 AutomationElement" -ForegroundColor Red
        exit
    }

    Write-Host "成功获取顶层 UIA 元素: $($element.Current.Name)" -ForegroundColor Green
    $allElements = $element.FindAll(
        [System.Windows.Automation.TreeScope]::Descendants,
        [System.Windows.Automation.Condition]::TrueCondition
    )

    Write-Host "找到子元素总数: $($allElements.Count)" -ForegroundColor Cyan
    Write-Host "----------------------------------------------------------"
    
    $index = 0
    foreach ($elem in $allElements) {
        $index++
        $cType = $elem.Current.ControlType.ProgrammaticName
        $cName = $elem.Current.ClassName
        $name = $elem.Current.Name
        $autoId = $elem.Current.AutomationId
        
        # 检查是否支持 ValuePattern
        $valPattern = $null
        $val = ""
        try {
            $valPattern = $elem.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
            if ($valPattern -ne $null) {
                $val = $valPattern.Current.Value
            }
        } catch {}

        # 重点高亮可能为编辑框或输入区域的控件
        $isEditLike = ($cType -match "Edit" -or $cName -match "Edit|LineEdit|Text" -or $val -ne "")
        if ($isEditLike) {
            Write-Host "[$index] EDIT_CANDIDATE:" -ForegroundColor Yellow
            Write-Host "    ControlType : $cType" -ForegroundColor Green
            Write-Host "    ClassName   : $cName" -ForegroundColor Cyan
            Write-Host "    Name        : $name"
            Write-Host "    AutomationId: $autoId"
            Write-Host "    CurrentValue: '$val'" -ForegroundColor Yellow
            Write-Host "----------------------------------------------------------"
        }
    }
} catch {
    Write-Host "UIA 查询异常: $_" -ForegroundColor Red
}
