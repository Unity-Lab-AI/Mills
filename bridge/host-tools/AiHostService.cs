// VibeRollerAIHost.exe: the Windows SERVICE that keeps this PC's VibeRoller AI node running from the moment the
// computer boots, before anyone signs in. The node (ai-node.js, one folder up) lends Unity's local engines to the
// VibeRoller hub on the server. Built by build-host-tools.ps1; installed once by install-ai-host-service.cmd (which
// asks for administrator rights, registers the service to start at boot, and lets signed-in users start and stop it
// without administrator rights, so the heart and the knife on the Desktop just work).
//
//   VibeRollerAIHost.exe              run by Windows as the service
//   VibeRollerAIHost.exe --console    the same, as a plain console program (for a look at what it does)
//   argument 1 (optional)             the full path of node.exe, recorded by the installer; else PATH / the usual places
//
// It starts node ai-node.js, writes its output to ai-node.log next to it (kept under 5 MB), restarts it 5 s after it
// exits, and waits half a minute when another node already runs here (exit code 2: the node's single-instance lock).
// Stopping the service stops the node. The knife (stop-viberoller-ai-host) stops the service; the heart starts it.
using System;
using System.Diagnostics;
using System.IO;
using System.ServiceProcess;
using System.Threading;

public class AiHostService : ServiceBase {
  const string NAME = "VibeRollerAIHost";
  readonly string dir, log, nodeExe;
  Process node;
  Thread worker;
  volatile bool stopping;

  public AiHostService(string nodeArg) {
    ServiceName = NAME; CanStop = true; CanShutdown = true; CanPauseAndContinue = false; AutoLog = false;
    dir = Path.GetFullPath(Path.Combine(AppDomain.CurrentDomain.BaseDirectory, ".."));
    log = Path.Combine(dir, "ai-node.log");
    nodeExe = FindNode(nodeArg);
  }

  static void Main(string[] args) {
    string nodeArg = null;
    bool console = false;
    foreach (var a in args) { if (a == "--console") console = true; else if (nodeArg == null && a.EndsWith("node.exe", StringComparison.OrdinalIgnoreCase)) nodeArg = a; }
    var svc = new AiHostService(nodeArg);
    if (console) {
      Console.WriteLine("VibeRollerAIHost (console): node = " + svc.nodeExe + ", folder = " + svc.dir + ". Press Enter to stop.");
      svc.OnStart(args); Console.ReadLine(); svc.OnStop();
      return;
    }
    ServiceBase.Run(svc);
  }

  protected override void OnStart(string[] args) {
    stopping = false;
    Log("service starting (node: " + nodeExe + ")");
    worker = new Thread(Loop) { IsBackground = true };
    worker.Start();
  }

  void Loop() {
    while (!stopping) {
      int code = -1;
      try { code = RunNode(); } catch (Exception e) { Log("could not start node: " + e.Message); }
      if (stopping) break;
      if (code == 2) { Log("another node already runs here (ai-node.lock); waiting 30 s"); Sleep(30000); }
      else Sleep(5000);
    }
    Log("service stopped");
  }

  int RunNode() {
    string script = Path.Combine(dir, "ai-node.js");
    if (!File.Exists(script)) { Log("ai-node.js is missing in " + dir); Sleep(25000); return -1; }
    if (!File.Exists(Path.Combine(dir, "ai-node.json"))) { Log("ai-node.json is missing in " + dir + ": copy ai-node.example.json to ai-node.json and fill in the hub, name and token"); Sleep(25000); return -1; }
    var psi = new ProcessStartInfo(nodeExe, "\"" + script + "\"") {
      WorkingDirectory = dir, UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true,
    };
    var p = Process.Start(psi);
    node = p;
    p.OutputDataReceived += (s, e) => { if (e.Data != null) Log(e.Data); };
    p.ErrorDataReceived += (s, e) => { if (e.Data != null) Log(e.Data); };
    p.BeginOutputReadLine(); p.BeginErrorReadLine();
    Log("node started (pid " + p.Id + ")");
    p.WaitForExit();
    int code = p.ExitCode;
    node = null;
    Log("node exited with code " + code);
    return code;
  }

  void Sleep(int ms) { for (int t = 0; t < ms && !stopping; t += 250) Thread.Sleep(250); }

  protected override void OnStop() {
    stopping = true;
    try { var p = node; if (p != null && !p.HasExited) { p.Kill(); p.WaitForExit(5000); } } catch { }
    try { if (worker != null) worker.Join(3000); } catch { }
  }
  protected override void OnShutdown() { OnStop(); }

  static string FindNode(string given) {
    if (!string.IsNullOrEmpty(given) && File.Exists(given)) return given;
    foreach (var c in new[] { "%ProgramFiles%\\nodejs\\node.exe", "%ProgramFiles(x86)%\\nodejs\\node.exe", "%ProgramW6432%\\nodejs\\node.exe" }) {
      var f = Environment.ExpandEnvironmentVariables(c);
      if (File.Exists(f)) return f;
    }
    foreach (var p in (Environment.GetEnvironmentVariable("PATH") ?? "").Split(';')) {
      try { var f = Path.Combine(p.Trim().Trim('"'), "node.exe"); if (p.Trim().Length > 0 && File.Exists(f)) return f; } catch { }
    }
    return "node.exe";
  }

  void Log(string line) {
    try {
      var fi = new FileInfo(log);
      if (fi.Exists && fi.Length > 5 * 1024 * 1024) File.WriteAllText(log, "(log trimmed)\r\n");
      File.AppendAllText(log, DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + " [service] " + line + "\r\n");
    } catch { }
  }
}
