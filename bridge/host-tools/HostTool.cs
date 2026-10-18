// VibeRoller host tools: start-viberoller-host.exe (black heart) and stop-viberoller-host.exe (knife).
// Built by build-host-tools.ps1 from this one file (START or STOP is defined at compile time).
//
// They switch THIS machine's AI hosting on or off, never the system:
//   stop  writes bridge\ai-host.off. The hub keeps running, so chat, board, calls, notes and people's own AI
//         keys keep working for everyone. Only the AI that runs here pauses: the host's Claude login and other
//         Claude Code logins here (Work runs, API answers), Unity's assistant brains and Unity in the chat.
//   start removes the file, so the AI is back at once; if the hub is not running on this machine at all,
//         it starts bridge\start.bat minimized (the supervisor; a second copy is harmless).
// Once the hub lives on the server, the same two tools will start and stop this machine's AI node instead.
using System;
using System.Diagnostics;
using System.IO;
using System.Net;
using System.Text.RegularExpressions;
using System.Windows.Forms;

static class HostTool {
  [STAThread]
  static int Main() {
    string tools = AppDomain.CurrentDomain.BaseDirectory;
    string bridge = Path.GetFullPath(Path.Combine(tools, ".."));
    string flag = Path.Combine(bridge, "ai-host.off");
    if (!File.Exists(Path.Combine(bridge, "server.js"))) {
      MessageBox.Show("Put this file in VibeRoller's bridge\\host-tools folder (it looks for bridge\\server.js next to it).", "VibeRoller host", MessageBoxButtons.OK, MessageBoxIcon.Error);
      return 1;
    }
#if START
    try { if (File.Exists(flag)) File.Delete(flag); } catch (Exception e) { return Fail("Could not switch the AI back on: " + e.Message); }
    bool running = HubAnswers(bridge);
    if (!running) {
      var psi = new ProcessStartInfo("cmd.exe", "/c start \"VibeRoller (leave this window open)\" /min \"" + Path.Combine(bridge, "start.bat") + "\"");
      psi.WorkingDirectory = bridge; psi.UseShellExecute = false; psi.CreateNoWindow = true;
      try { Process.Start(psi); } catch (Exception e) { return Fail("Could not start VibeRoller: " + e.Message); }
    }
    MessageBox.Show(running
      ? "This machine hosts AI for VibeRoller again.\n\nWork, the API and Unity can use it right away."
      : "VibeRoller is starting on this machine (a minimized window), and its AI is switched on.\n\nGive it a few seconds.",
      "VibeRoller host: on", MessageBoxButtons.OK, MessageBoxIcon.Information);
#else
    try { File.WriteAllText(flag, "AI paused by this machine's host at " + DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss") + "\r\n"); }
    catch (Exception e) { return Fail("Could not pause the AI: " + e.Message); }
    MessageBox.Show("This machine's AI is paused.\n\nVibeRoller keeps running for everyone (chat, board, calls, notes, their own AI keys). Only the AI hosted here waits until you start it again with the heart.",
      "VibeRoller host: off", MessageBoxButtons.OK, MessageBoxIcon.Information);
#endif
    return 0;
  }
  static int Fail(string text) { MessageBox.Show(text, "VibeRoller host", MessageBoxButtons.OK, MessageBoxIcon.Error); return 1; }
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
