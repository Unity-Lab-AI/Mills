// VibeRoller AI host tools: start-viberoller-ai-host.exe (black heart) and stop-viberoller-ai-host.exe (knife).
// Built by build-host-tools.ps1 from this one file (START or STOP is defined at compile time).
//
// They switch THIS machine's AI hosting on or off, never the system, and never anyone else's AI host:
//   - with the VibeRollerAIHost service installed (install-ai-host-service.cmd; it starts at boot by itself):
//       heart  = start the service (and clear the pause switch)      knife = stop the service
//   - without the service (the older sign-in launcher / a hub running here):
//       heart  = clear the pause switch bridge\ai-host.off and start the node (bridge\ai-node.json present) or the hub
//       knife  = write bridge\ai-host.off: the AI that runs here pauses; the hub and everyone's chat, board, calls,
//                notes and own AI keys keep working
// Stopping affects one AI host among possibly several lending engines to the hub; the hub itself lives on the server.
using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.ServiceProcess;
using System.Text.RegularExpressions;
using System.Windows.Forms;

static class HostTool {
  const string SERVICE = "VibeRollerAIHost";
  [STAThread]
  static int Main() {
    string tools = AppDomain.CurrentDomain.BaseDirectory;
    string bridge = Path.GetFullPath(Path.Combine(tools, ".."));
    string flag = Path.Combine(bridge, "ai-host.off");
    if (!File.Exists(Path.Combine(bridge, "ai-node.js")) && !File.Exists(Path.Combine(bridge, "server.js"))) {
      MessageBox.Show("Put this file in the host-tools folder next to ai-node.js (the VibeRoller AI Host folder, or VibeRoller's bridge folder).", "VibeRoller AI host", MessageBoxButtons.OK, MessageBoxIcon.Error);
      return 1;
    }
    ServiceController svc = FindService();
#if START
    try { if (File.Exists(flag)) File.Delete(flag); } catch (Exception e) { return Fail("Could not switch the AI back on: " + e.Message); }
    if (svc != null) {
      try {
        if (svc.Status != ServiceControllerStatus.Running) { svc.Start(); svc.WaitForStatus(ServiceControllerStatus.Running, TimeSpan.FromSeconds(30)); }
      } catch (Exception e) { return Fail("Could not start the VibeRollerAIHost service: " + e.Message + "\n\nRun install-ai-host-service.cmd once so signed-in users may start and stop it."); }
      MessageBox.Show("This PC is an AI host for VibeRoller again.\n\nThe VibeRollerAIHost service is running: it lends Unity's engines to the VibeRoller server, and starts by itself at boot.", "VibeRoller AI host: on", MessageBoxButtons.OK, MessageBoxIcon.Information);
      return 0;
    }
    // No service: two older shapes of AI host. The hub itself on this machine (no bridge\ai-node.json), or this machine's
    // AI node lending its engines to the hub on the server (bridge\ai-node.json present). Start whichever is not running yet.
    bool nodeMode = File.Exists(Path.Combine(bridge, "ai-node.json"));
    bool running = nodeMode ? NodeRuns(bridge) : HubAnswers(bridge);
    if (!running) {
      string script = Path.Combine(bridge, nodeMode ? "start-node.bat" : "start.bat");
      var psi = new ProcessStartInfo("cmd.exe", "/c start \"VibeRoller (leave this window open)\" /min \"" + script + "\"");
      psi.WorkingDirectory = bridge; psi.UseShellExecute = false; psi.CreateNoWindow = true;
      try { Process.Start(psi); } catch (Exception e) { return Fail("Could not start VibeRoller: " + e.Message); }
    }
    MessageBox.Show(running
      ? "This machine hosts AI for VibeRoller again.\n\nWork, the API and Unity can use it right away."
      : (nodeMode ? "This PC's AI node is starting (a minimized window): it lends Unity's engines to the VibeRoller server.\n\nGive it a few seconds. Tip: install-ai-host-service.cmd makes it a service that starts at boot."
                  : "VibeRoller is starting on this machine (a minimized window), and its AI is switched on.\n\nGive it a few seconds."),
      "VibeRoller AI host: on", MessageBoxButtons.OK, MessageBoxIcon.Information);
#else
    if (svc != null) {
      try {
        if (svc.Status != ServiceControllerStatus.Stopped && svc.Status != ServiceControllerStatus.StopPending) { svc.Stop(); svc.WaitForStatus(ServiceControllerStatus.Stopped, TimeSpan.FromSeconds(30)); }
      } catch (Exception e) { return Fail("Could not stop the VibeRollerAIHost service: " + e.Message + "\n\nRun install-ai-host-service.cmd once so signed-in users may start and stop it."); }
      MessageBox.Show("This PC's AI host is stopped.\n\nVibeRoller keeps running on the server for everyone (chat, board, calls, notes, their own AI keys); Unity says her AI host is offline until you start it again with the heart. It starts again by itself at the next boot.",
        "VibeRoller AI host: off", MessageBoxButtons.OK, MessageBoxIcon.Information);
      return 0;
    }
    try { File.WriteAllText(flag, "AI paused by this machine's host at " + DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "\r\n"); }
    catch (Exception e) { return Fail("Could not pause the AI: " + e.Message); }
    MessageBox.Show("This machine's AI is paused.\n\nVibeRoller keeps running for everyone (chat, board, calls, notes, their own AI keys). Only the AI hosted here waits until you start it again with the heart.",
      "VibeRoller AI host: off", MessageBoxButtons.OK, MessageBoxIcon.Information);
#endif
    return 0;
  }
  static int Fail(string text) { MessageBox.Show(text, "VibeRoller AI host", MessageBoxButtons.OK, MessageBoxIcon.Error); return 1; }
  static ServiceController FindService() {
    try { var sc = new ServiceController(SERVICE); var s = sc.Status; return sc; } catch { return null; } // no such service → null
  }
  static bool NodeRuns(string bridge) { // the node keeps bridge\ai-node.lock with its pid while it runs
    try { int pid = int.Parse(File.ReadAllText(Path.Combine(bridge, "ai-node.lock")).Trim()); Process.GetProcessById(pid); return true; } catch { return false; }
  }
  static bool HubAnswers(string bridge) { // is a hub already serving on this machine? (its port from bridge\config.json)
    int port = 7777;
    try { var m = Regex.Match(File.ReadAllText(Path.Combine(bridge, "config.json")), "\"port\"\\s*:\\s*(\\d+)"); if (m.Success) port = int.Parse(m.Groups[1].Value); } catch { }
    try {
      var req = (HttpWebRequest)WebRequest.Create("http://127.0.0.1:" + port + "/api/status");
      req.Timeout = 2500;
      using (var res = (HttpWebResponse)req.GetResponse()) return (int)res.StatusCode == 200;
    } catch { return false; }
  }
}
