# For smoke.mjs: prints the foreground window, and with -Press then presses Ctrl+Alt+Shift+F12,
# the test instance's hotkey. Waits until the keyboard and mouse have been idle for 2 seconds,
# so the keys never land in the middle of someone's typing; exits 3 if that doesn't happen.
param([switch]$Press)

Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Keys {
    [StructLayout(LayoutKind.Sequential)] struct LastInput { public uint Size; public uint Time; }
    [DllImport("user32.dll")] static extern bool GetLastInputInfo(ref LastInput info);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
    public static uint Idle() {
        var info = new LastInput { Size = 8 };
        GetLastInputInfo(ref info);
        return (uint)Environment.TickCount - info.Time;
    }
    public static void Chord(byte[] vks) {
        foreach (var vk in vks) keybd_event(vk, 0, 0, UIntPtr.Zero);
        for (int i = vks.Length - 1; i >= 0; i--) keybd_event(vks[i], 0, 2, UIntPtr.Zero);
    }
}
'@

if ($Press) {
    $deadline = (Get-Date).AddSeconds(60)
    while ([Keys]::Idle() -lt 2000) {
        if ((Get-Date) -gt $deadline) { exit 3 }
        Start-Sleep -Milliseconds 250
    }
}
[Keys]::GetForegroundWindow().ToInt64()
# Ctrl, Alt, Shift, F12.
if ($Press) { [Keys]::Chord([byte[]](0x11, 0x12, 0x10, 0x7B)) }
