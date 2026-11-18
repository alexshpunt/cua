# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Aleksandr Shpuntenko

# Opt-in diagnostic. Run on an isolated interactive Windows desktop with the WPF fixture built.
param(
    [Parameter(Mandatory=$true)][string]$FixtureExe,
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory
)
$ErrorActionPreference = 'Stop'
if (![Environment]::Is64BitProcess) { throw 'The diagnostic requires a 64-bit process.' }
if (Test-Path $EvidenceDirectory) { throw 'Use a new evidence directory.' }
[IO.Directory]::CreateDirectory($EvidenceDirectory) | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.RegularExpressions;
using System.Threading;
public static class AttachedKeyReadProbe {
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int x,y; }
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int left,top,right,bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Key { public ushort vk,scan; public uint flags,time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Sequential)] public struct Mouse { public int x,y; public uint data,flags,time; public UIntPtr extra; }
    [StructLayout(LayoutKind.Explicit,Size=40)] public struct Input {
        [FieldOffset(0)] public uint type;
        [FieldOffset(8)] public Key key;
        [FieldOffset(8)] public Mouse mouse;
    }
    [StructLayout(LayoutKind.Sequential)] public struct Message { public IntPtr hwnd; public uint message; public UIntPtr wparam; public IntPtr lparam; public uint time; public Point point; public uint reserved; }
    public sealed class Sample { public string phase; public long utc_ms; public bool peek,ok; public int state; public long foreground; }
    public sealed class Report {
        public int mode; public long target; public uint own,reader; public bool attached,down_observed,up_observed,drained;
        public int pointer_down,pointer_up,key_down,key_up; public string error;
        public List<Sample> samples = new List<Sample>();
    }
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd,int command);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr hwnd,IntPtr pid);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint own,uint target,bool attach);
    [DllImport("user32.dll")] static extern bool GetKeyboardState([Out] byte[] state);
    [DllImport("user32.dll")] static extern bool PeekMessage(out Message message,IntPtr hwnd,uint first,uint last,uint remove);
    [DllImport("user32.dll")] static extern uint SendInput(uint count,Input[] input,int size);
    [DllImport("user32.dll")] static extern bool GetClientRect(IntPtr hwnd,out Rect rect);
    [DllImport("user32.dll")] static extern bool ClientToScreen(IntPtr hwnd,ref Point point);
    [DllImport("user32.dll")] static extern int GetSystemMetrics(int index);
    [DllImport("user32.dll")] static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
    [DllImport("user32.dll")] static extern IntPtr SendMessageTimeout(IntPtr hwnd,uint message,UIntPtr wparam,IntPtr lparam,uint flags,uint timeout,out UIntPtr result);
    static long Now() { return DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(); }
    static bool PeekOwn() { Message message; return PeekMessage(out message,IntPtr.Zero,0,0,0); }
    public static string Journal(string path) {
        try {
            using(var stream=new FileStream(path,FileMode.Open,FileAccess.Read,FileShare.ReadWrite|FileShare.Delete))
            using(var reader=new StreamReader(stream)) return reader.ReadToEnd();
        } catch(IOException) { return ""; }
    }
    static bool PointerRead(string text) { return text.Contains("\"up\":1"); }
    static bool KeyUpRead(string text) { return Regex.IsMatch(text,"\"message\"\\s*:\\s*257\\s*,\\s*\"virtual_key\"\\s*:\\s*112"); }
    static uint KeyInput(bool up) {
        var input = new Input { type=1,key=new Key { scan=0x3b,flags=8u|(up?2u:0u) } };
        return SendInput(1,new[]{input},40);
    }
    static uint MouseInput(uint flags,int x=0,int y=0) {
        return SendInput(1,new[]{new Input { type=0,mouse=new Mouse { x=x,y=y,flags=flags } }},40);
    }
    static bool Observe(Report report,string phase,bool expectedDown) {
        long deadline=Now()+2000;
        do {
            bool peek=report.mode==1 && PeekOwn();
            var state=new byte[256]; bool ok=GetKeyboardState(state);
            var sample=new Sample { phase=phase,utc_ms=Now(),peek=peek,ok=ok,state=state[0x70],foreground=GetForegroundWindow().ToInt64() };
            report.samples.Add(sample);
            if(sample.foreground!=report.target) throw new Exception("foreground_lost");
            if(!ok) throw new Exception("keyboard_state_unavailable");
            if(((sample.state&0x80)!=0)==expectedDown) return true;
            Thread.Sleep(10);
        } while(Now()<deadline);
        return false;
    }
    public static Report Run(IntPtr target,string statePath,int mode) {
        var report=new Report { mode=mode,target=target.ToInt64() };
        var thread=new Thread(()=> {
            IntPtr oldDpi=SetThreadDpiAwarenessContext(new IntPtr(-4));
            bool mouseHeld=false,keyHeld=false;
            try {
                if(oldDpi==IntPtr.Zero) throw new Exception("dpi_context_unavailable");
                if(GetForegroundWindow()!=target) throw new Exception("target_not_foreground");
                UIntPtr ignored;
                if(SendMessageTimeout(target,0,UIntPtr.Zero,IntPtr.Zero,2,500,out ignored)==IntPtr.Zero) throw new Exception("activation_not_ready");
                PeekOwn();
                report.own=GetCurrentThreadId(); report.reader=GetWindowThreadProcessId(target,IntPtr.Zero);
                report.attached=AttachThreadInput(report.own,report.reader,true);
                if(!report.attached) throw new Exception("attachment_unavailable");
                var initial=new byte[256];
                if(!GetKeyboardState(initial) || (initial[0x70]&0x80)!=0) throw new Exception("initial_key_not_up");
                Rect client; if(!GetClientRect(target,out client)) throw new Exception("client_rect_unavailable");
                var point=new Point { x=(client.right-client.left)/2,y=(client.bottom-client.top)/4 };
                if(!ClientToScreen(target,ref point)) throw new Exception("screen_point_unavailable");
                int vx=GetSystemMetrics(76),vy=GetSystemMetrics(77),vw=GetSystemMetrics(78),vh=GetSystemMetrics(79);
                int nx=(int)Math.Round((point.x-vx)*65535.0/(vw-1)),ny=(int)Math.Round((point.y-vy)*65535.0/(vh-1));
                if(MouseInput(0xc001,nx,ny)!=1) throw new Exception("move_not_inserted");
                Thread.Sleep(30);
                if(MouseInput(2)!=1) throw new Exception("pointer_down_not_inserted");
                report.pointer_down=1; mouseHeld=true;
                long deadline=Now()+2000;
                while(!Journal(statePath).Contains("\"busy_started\":true")) {
                    if(Now()>=deadline) throw new Exception("receiver_did_not_enter_workload");
                    Thread.Sleep(5);
                }
                if(Journal(statePath).Contains("\"busy_finished\":true")) throw new Exception("workload_finished_before_key_insertion");
                if(MouseInput(4)!=1) throw new Exception("pointer_up_not_inserted");
                report.pointer_up=1; mouseHeld=false;
                if(KeyInput(false)!=1) throw new Exception("key_down_not_inserted");
                report.key_down=1; keyHeld=true;
                report.down_observed=Observe(report,"down",true);
                if(KeyInput(true)!=1) throw new Exception("key_up_not_inserted");
                report.key_up=1; keyHeld=false;
                report.up_observed=Observe(report,"up",false);
            } catch(Exception error) { report.error=error.ToString(); }
            finally {
                // Cleanup only DOWNs inserted by this observer. Never replay a requested press.
                if(keyHeld) report.key_up=(int)KeyInput(true);
                if(mouseHeld) report.pointer_up=(int)MouseInput(4);
                long deadline=Now()+8000;
                do {
                    string text=Journal(statePath);
                    report.drained=(report.pointer_down==0 || PointerRead(text)) && (report.key_down==0 || KeyUpRead(text));
                    if(report.drained) break;
                    // Outside the sampled interval, service sent messages to allow owned cleanup.
                    PeekOwn(); Thread.Sleep(10);
                } while(Now()<deadline);
                if(report.attached && report.drained) AttachThreadInput(report.own,report.reader,false);
                if(oldDpi!=IntPtr.Zero) SetThreadDpiAwarenessContext(oldDpi);
                if(!report.drained && report.error==null) report.error="cleanup_unconfirmed";
            }
        });
        thread.SetApartmentState(ApartmentState.MTA); thread.Start(); thread.Join(); return report;
    }
}
'@
$results = @()
foreach ($mode in 0..1) {
    $state = Join-Path $EvidenceDirectory "target-$mode.json"
    $env:CUA_WPF_POINTER_STATE=$state
    $env:CUA_WPF_POINTER_ROLE='target'
    $env:CUA_WPF_POINTER_BUSY_MS='1200'
    $env:CUA_WPF_POINTER_PUMP_KEYS='0'
    $env:CUA_WPF_POINTER_BUSY_TRIGGER='down'
    $process=Start-Process -FilePath $FixtureExe -PassThru
    $report=$null
    try {
        $deadline=[DateTime]::UtcNow.AddSeconds(10)
        do { Start-Sleep -Milliseconds 50; $process.Refresh() } while ((!$process.MainWindowHandle -or !(Test-Path $state)) -and [DateTime]::UtcNow -lt $deadline)
        $target=$process.MainWindowHandle
        if (!$target) { throw 'Receiver did not start.' }
        [AttachedKeyReadProbe]::ShowWindow($target,6) | Out-Null
        [AttachedKeyReadProbe]::ShowWindow($target,9) | Out-Null
        [AttachedKeyReadProbe]::SetForegroundWindow($target) | Out-Null
        $report=[AttachedKeyReadProbe]::Run($target,$state,$mode)
        [IO.File]::WriteAllText((Join-Path $EvidenceDirectory "samples-$mode.json"),($report | ConvertTo-Json -Depth 8),(New-Object Text.UTF8Encoding($false)))
        if (!$report.drained) { throw "Owned cleanup remains unconfirmed; retain receiver PID $($process.Id)." }
        if ($report.error) { throw $report.error }
        $receiver=[AttachedKeyReadProbe]::Journal($state) | ConvertFrom-Json
        if ($receiver.down -ne 1 -or $receiver.up -ne 1 -or $receiver.dropped_events -ne 0) { throw 'Pointer journal differs from the inserted stream.' }
        $keyboard=@($receiver.events | Where-Object { $_.virtual_key -eq 112 })
        if (@($keyboard | Where-Object { $_.message -eq 256 }).Count -ne 1 -or @($keyboard | Where-Object { $_.message -eq 257 }).Count -ne 1) { throw 'Key journal differs from the inserted stream.' }
        $busyEnd=($receiver.events | Where-Object { $_.kind -eq 'busy_end' } | Select-Object -First 1).utc_ms
        $early=@($report.samples | Where-Object { $_.phase -eq 'down' -and ($_.state -band 128) -ne 0 -and $_.utc_ms -lt $busyEnd })
        $results+=@{ mode=$mode; down_observed=$report.down_observed; up_observed=$report.up_observed; busy_end=$busyEnd; earliest_key_read=($keyboard | Measure-Object utc_ms -Minimum).Minimum; false_positive_during_busy=($early.Count -gt 0) }
    } finally {
        if (!$process.HasExited -and ($null -eq $report -or $report.drained)) { Stop-Process -Id $process.Id }
    }
}
[IO.File]::WriteAllText((Join-Path $EvidenceDirectory 'summary.json'),($results | ConvertTo-Json -Depth 5),(New-Object Text.UTF8Encoding($false)))
Write-Output ($results | ConvertTo-Json -Compress)
if (@($results | Where-Object { $_.false_positive_during_busy }).Count -gt 0) { exit 1 }
if (@($results | Where-Object { !$_.down_observed -or !$_.up_observed }).Count -gt 0) { exit 2 }
exit 0
