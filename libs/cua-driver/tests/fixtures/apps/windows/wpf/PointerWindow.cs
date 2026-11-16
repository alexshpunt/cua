using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text.Json;
using System.Threading;
using System.Windows;
using System.Windows.Automation;
using System.Windows.Automation.Peers;
using System.Windows.Controls;
using System.Windows.Input;
using System.Windows.Interop;
using System.Windows.Media;
using System.Windows.Threading;

namespace CuaTestHarness.Wpf;

/// <summary>
/// An opt-in pointer receiver with an observable, bounded UI-thread workload.
/// Its timer publishes application state independently from Driver receipts.
/// Ordinary fixture launches do not create this window.
/// </summary>
public sealed class PointerWindow : Window
{
    public const string StateEnv = "CUA_WPF_POINTER_STATE";
    public const string RoleEnv = "CUA_WPF_POINTER_ROLE";
    public const string BusyEnv = "CUA_WPF_POINTER_BUSY_MS";
    public const string PumpEnv = "CUA_WPF_POINTER_PUMP_KEYS";
    public const string TriggerEnv = "CUA_WPF_POINTER_BUSY_TRIGGER";
    private const int MaximumEvents = 10000;
    private readonly string _statePath;
    private readonly string _role;
    private readonly PointerSurface _surface = new();
    private readonly List<object> _events = new();
    private readonly DispatcherTimer _timer = new() { Interval = TimeSpan.FromMilliseconds(100) };
    private int _busyMs, _down, _up, _moves, _heldMoves, _wheel, _droppedEvents;
    private bool _ready, _busyStarted, _busyFinished;
    private readonly bool _pumpKeyboard;
    private readonly string _busyTrigger;
    private Point? _pressedAt;
    private ulong _windowId;

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [StructLayout(LayoutKind.Sequential)]
    private struct NativePoint { public int X, Y; }
    [StructLayout(LayoutKind.Sequential)]
    private struct NativeMessage
    {
        public IntPtr Window;
        public uint Message;
        public UIntPtr WParam;
        public IntPtr LParam;
        public uint Time;
        public NativePoint Point;
        public uint Reserved;
    }
    [DllImport("user32.dll")]
    private static extern bool PeekMessage(out NativeMessage message, IntPtr window, uint first, uint last, uint remove);
    [DllImport("user32.dll")]
    private static extern bool TranslateMessage(ref NativeMessage message);
    [DllImport("user32.dll")]
    private static extern IntPtr DispatchMessage(ref NativeMessage message);

    public PointerWindow(string statePath, string role, int busyMs, bool pumpKeyboard)
    {
        if (role is not ("human" or "target")) throw new ArgumentException("Pointer role must be human or target.");
        if (busyMs is < 0 or > 5000) throw new ArgumentOutOfRangeException(nameof(busyMs));
        _statePath = Path.GetFullPath(statePath);
        Directory.CreateDirectory(Path.GetDirectoryName(_statePath)!);
        _role = role;
        _busyMs = busyMs;
        _pumpKeyboard = pumpKeyboard;
        _busyTrigger = Environment.GetEnvironmentVariable(TriggerEnv) ?? "held_move";
        if (_busyTrigger is not ("held_move" or "down" or "wheel")) throw new ArgumentException("Invalid pointer workload trigger.");
        Title = $"CuaTestHarness WPF Pointer {role}";
        Left = 80;
        Top = 80;
        Width = 780;
        Height = 520;
        WindowStartupLocation = WindowStartupLocation.Manual;
        var root = new StackPanel { Margin = new Thickness(16) };
        root.Children.Add(_surface);
        root.Children.Add(new TextBlock { Text = "Work on the first selected pointer event (milliseconds)", Margin = new Thickness(0, 12, 0, 4) });
        var workload = new TextBox { Text = busyMs.ToString(), Width = 120, HorizontalAlignment = HorizontalAlignment.Left };
        AutomationProperties.SetAutomationId(workload, "pointer-work-ms");
        AutomationProperties.SetName(workload, "Pointer move work in milliseconds");
        workload.TextChanged += (_, _) =>
        {
            if (int.TryParse(workload.Text, out var value) && value is >= 0 and <= 5000) _busyMs = value;
        };
        root.Children.Add(workload);
        Content = root;
        _surface.MouseLeftButtonDown += (_, e) =>
        {
            _down++;
            _pressedAt = e.GetPosition(_surface);
            Record("down", e);
            WorkIfTriggered("down", e);
            // This plain canvas does not capture the mouse. The gesture stays inside
            // it; moving the cursor or another window above it before UP is read
            // can therefore divert the unread release, as in a composition canvas.
        };
        _surface.MouseMove += (_, e) =>
        {
            _moves++;
            Record("move", e);
            if (e.LeftButton != MouseButtonState.Pressed) return;
            _heldMoves++;
            Record("held_move", e);
            var point = e.GetPosition(_surface);
            if (_pressedAt is Point start && (point - start).Length > 1)
                WorkIfTriggered("held_move", e);
        };
        _surface.MouseLeftButtonUp += (_, e) => { _up++; Record("up", e); };
        _surface.MouseWheel += (_, e) =>
        {
            _wheel++;
            Record("wheel", e, message: 0x020A, wheelDelta: e.Delta);
            WorkIfTriggered("wheel", e);
            e.Handled = true;
        };
        _surface.GotMouseCapture += (_, e) => Record("capture_gained", e);
        _surface.LostMouseCapture += (_, e) => Record("capture_lost", e);
        Activated += (_, _) => Record("activated");
        Deactivated += (_, _) => Record("deactivated");
        SourceInitialized += (_, _) =>
        {
            var source = HwndSource.FromHwnd(new WindowInteropHelper(this).Handle);
            source?.AddHook(ObserveNativeMessage);
        };
        Loaded += (_, _) =>
        {
            _windowId = unchecked((ulong)new WindowInteropHelper(this).Handle.ToInt64());
            _ready = true;
            Record("ready");
            Publish();
            _timer.Start();
        };
        _timer.Tick += (_, _) => Publish();
        Closed += (_, _) => { _timer.Stop(); Record("closed"); Publish(); };
    }

    private IntPtr ObserveNativeMessage(IntPtr window, int message, IntPtr wParam, IntPtr lParam, ref bool handled)
    {
        if (message is >= 0x0100 and <= 0x0109)
            Record("keyboard", message: (uint)message, virtualKey: unchecked((ulong)wParam.ToInt64()));
        return IntPtr.Zero;
    }

    private void WorkIfTriggered(string trigger, MouseEventArgs mouse)
    {
        if (_busyStarted || _busyMs == 0 || trigger != _busyTrigger) return;
        _busyStarted = true;
        Record("busy_start", mouse);
        // Publish before blocking so the independent test can establish that
        // later global input was inserted while this receiver was busy.
        Publish();
        if (_pumpKeyboard)
        {
            var end = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() + _busyMs;
            while (DateTimeOffset.UtcNow.ToUnixTimeMilliseconds() < end)
            {
                // Filtered retrieval may read keys while older pointer input
                // remains unread. The native hook journals actual dispatch.
                while (PeekMessage(out var message, IntPtr.Zero, 0x0100, 0x0109, 1))
                {
                    TranslateMessage(ref message);
                    DispatchMessage(ref message);
                }
                Thread.Sleep(10);
            }
        }
        else Thread.Sleep(_busyMs);
        _busyFinished = true;
        Record("busy_end", mouse);
        Publish();
    }

    private void Record(string kind, MouseEventArgs? mouse = null, uint? message = null, ulong? virtualKey = null, int? wheelDelta = null)
    {
        if (_events.Count == MaximumEvents) { _droppedEvents++; return; }
        Point? position = mouse?.GetPosition(_surface);
        _events.Add(new
        {
            kind,
            message,
            virtual_key = virtualKey,
            wheel_delta = wheelDelta,
            utc_ms = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            x = position?.X,
            y = position?.Y,
            left_button = mouse?.LeftButton == MouseButtonState.Pressed,
            captured = Mouse.Captured == _surface,
            foreground_window = unchecked((ulong)GetForegroundWindow().ToInt64()),
        });
    }

    private void Publish()
    {
        var state = new
        {
            schema = "cua.wpf_pointer_receiver.v1",
            role = _role,
            ready = _ready,
            pid = Environment.ProcessId,
            window_id = _windowId,
            published_utc_ms = DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(),
            busy_ms = _busyMs,
            pump_keyboard = _pumpKeyboard,
            busy_trigger = _busyTrigger,
            busy_started = _busyStarted,
            busy_finished = _busyFinished,
            down = _down,
            up = _up,
            moves = _moves,
            held_moves = _heldMoves,
            wheel = _wheel,
            dropped_events = _droppedEvents,
            events = _events,
        };
        var temporary = $"{_statePath}.{Environment.ProcessId}.tmp";
        File.WriteAllText(temporary, JsonSerializer.Serialize(state));
        var deadline = Environment.TickCount64 + 1000;
        while (true)
        {
            try
            {
                File.Move(temporary, _statePath, true);
                break;
            }
            catch (Exception error) when (
                error is IOException or UnauthorizedAccessException &&
                (error.HResult & 0xffff) is 5 or 32 or 33 &&
                Environment.TickCount64 < deadline)
            {
                // Readers and scanners can briefly hold a Windows delete lock.
                // Retry only the atomic publication, never application input.
                Thread.Sleep(5);
            }
        }
    }

    private sealed class PointerSurface : Canvas
    {
        public PointerSurface()
        {
            Width = 600;
            Height = 300;
            HorizontalAlignment = HorizontalAlignment.Left;
            Background = Brushes.White;
            AutomationProperties.SetAutomationId(this, "pointer-canvas");
            AutomationProperties.SetName(this, "Pointer canvas");
        }

        protected override AutomationPeer OnCreateAutomationPeer() => new SurfacePeer(this);

        private sealed class SurfacePeer : FrameworkElementAutomationPeer
        {
            public SurfacePeer(PointerSurface owner) : base(owner) { }
            protected override AutomationControlType GetAutomationControlTypeCore() => AutomationControlType.Group;
            protected override string GetClassNameCore() => "PointerReceiver";
            protected override bool IsControlElementCore() => true;
        }
    }
}
