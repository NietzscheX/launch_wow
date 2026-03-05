; === 设置区域 ===
TargetWidth := 500
TargetHeight := 500
WowPath := ".\Wow.exe"  ; 修改为你的 wow.exe 路径
; ================

Run, %WowPath%
WinWait, World of Warcraft, , 10 ; 等待游戏窗口出现，超时10秒

If ErrorLevel
{
    MsgBox, 没找到游戏窗口!
    Return
}

; 计算右下角坐标
; A_ScreenWidth 是屏幕宽，A_ScreenHeight 是屏幕高
PosX := A_ScreenWidth - TargetWidth
PosY := A_ScreenHeight - TargetHeight - 40 ; 减40是为了避开任务栏，你可以调整

; 强制移动并调整大小
; WinMove, 窗口标题, , X坐标, Y坐标, 宽度, 高度
WinMove, World of Warcraft, , %PosX%, %PosY%, %TargetWidth%, %TargetHeight%

; 可选：去掉标题栏边框，让它看起来更像个小监视器
; WinSet, Style, -0xC00000, World of Warcraft

