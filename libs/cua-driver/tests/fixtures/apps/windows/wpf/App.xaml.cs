using System;
using System.Windows;

namespace CuaTestHarness.Wpf;

public partial class App : Application
{
    // Ordinary launches show MainWindow exactly as before (it was the
    // StartupUri). CUA_WPF_TASK_STATE selects the opt-in jev-use task window;
    // CUA_WPF_TASK_DENSITY adds its distractor controls (#4312).
    private void OnStartup(object sender, StartupEventArgs e)
    {
        var taskState = Environment.GetEnvironmentVariable(TaskWindow.StateEnv);
        var pointerState = Environment.GetEnvironmentVariable(PointerWindow.StateEnv);
        Window window;
        if (!string.IsNullOrWhiteSpace(pointerState))
        {
            if (!string.IsNullOrWhiteSpace(taskState)) throw new ArgumentException("Select one fixture mode.");
            var role = Environment.GetEnvironmentVariable(PointerWindow.RoleEnv) ?? "target";
            var rawBusy = Environment.GetEnvironmentVariable(PointerWindow.BusyEnv) ?? "0";
            if (!int.TryParse(rawBusy, out var busy)) throw new ArgumentException("Pointer work must be an integer.");
            var pumpKeys = Environment.GetEnvironmentVariable(PointerWindow.PumpEnv) == "1";
            window = new PointerWindow(pointerState, role, busy, pumpKeys);
        }
        else
        {
            window = string.IsNullOrWhiteSpace(taskState)
                ? new MainWindow()
                : new TaskWindow(taskState, TaskWindow.DensityFromEnvironment());
        }
        MainWindow = window;
        window.Show();
    }
}
