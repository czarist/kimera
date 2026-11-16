# Starts a Craft app for Kimera (src/launch.rs) and optionally hides or minimizes its windows.
# Set before this script: $line (command line), $dir (working folder), $show (-1 = leave the
# windows alone, 0 = hide, 7 = minimize), $tries and $poll (how long to wait for a window).
#
# WMI starts the process, so it is nobody's child: it inherits no handle from Kimera (a
# terminal pipe would stay open until the app closed) and lives on after Kimera exits.
$r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create `
    -Arguments @{ CommandLine = $line; CurrentDirectory = $dir }
if ($r.ReturnValue -ne 0) { [Console]::Error.WriteLine("Win32_Process.Create failed: $($r.ReturnValue)"); exit 1 }
if ($show -lt 0) { exit 0 }

# Windows built on winit show themselves whatever the start options say, so wait for them to
# appear and hide or minimize every top-level window of the process.
Add-Type -Namespace Kimera -Name Windows -MemberDefinition @'
[DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int c);
[DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
[DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
delegate bool Each(IntPtr h, IntPtr l);
[DllImport("user32.dll")] static extern bool EnumWindows(Each e, IntPtr l);
public static int Apply(uint pid, int code) {
    int n = 0;
    EnumWindows((h, l) => {
        uint owner;
        GetWindowThreadProcessId(h, out owner);
        if (owner == pid && IsWindowVisible(h)) { ShowWindow(h, code); n++; }
        return true;
    }, IntPtr.Zero);
    return n;
}
'@
for ($i = 0; $i -lt $tries; $i++) {
    if (-not (Get-Process -Id $r.ProcessId -ErrorAction SilentlyContinue)) { exit 0 }
    if ([Kimera.Windows]::Apply([uint32]$r.ProcessId, $show) -gt 0) {
        # Some apps open a second window a moment later; look once more.
        Start-Sleep -Seconds 1
        [void][Kimera.Windows]::Apply([uint32]$r.ProcessId, $show)
        exit 0
    }
    Start-Sleep -Milliseconds $poll
}
