//! The container engine behind one trait. [`Cli`] drives the `podman` or `docker` command line (Docker on Windows,
//! Podman on Linux); a program or a test may bring its own [`Runtime`].
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Which pipe a piece of output came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stream { Stdout, Stderr }

/// How a command ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExecEnd { pub code: Option<i32>, pub timed_out: bool }

/// A container that exists (running or not) under the sandbox name prefix.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Found { pub name: String, pub running: bool }

/// Everything one copy is started with. [`RunSpec::args`] turns it into the `run` command line; the flags are the
/// security posture (see the crate README): no host mount except the exchange folders, no network unless asked,
/// resource limits, every capability dropped except the listed ones, no privilege gain.
#[derive(Clone, Debug)]
pub struct RunSpec {
    pub name: String,
    pub image: String,
    pub network: bool,
    pub cpus: String,
    pub memory: String,
    pub pids_limit: u32,
    pub cap_add: Vec<String>,
    pub in_dir: PathBuf,
    pub out_dir: PathBuf,
}
impl RunSpec {
    /// The arguments after `run` (the same for Podman and Docker).
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = ["-d", "--rm", "--pull=never", "--name"].iter().map(|s| s.to_string()).collect();
        a.push(self.name.clone());
        a.push("--hostname".into()); a.push(self.name.clone());
        a.push("--label".into()); a.push("pgm.sandbox=1".into());
        if !self.network { a.push("--network".into()); a.push("none".into()); }
        if !self.cpus.is_empty() { a.push("--cpus".into()); a.push(self.cpus.clone()); }
        if !self.memory.is_empty() { a.push("--memory".into()); a.push(self.memory.clone()); }
        if self.pids_limit > 0 { a.push("--pids-limit".into()); a.push(self.pids_limit.to_string()); }
        a.push("--cap-drop".into()); a.push("ALL".into());
        for c in &self.cap_add { a.push("--cap-add".into()); a.push(c.clone()); }
        a.push("--security-opt".into()); a.push("no-new-privileges".into());
        // --mount (not -v): a Windows path has a colon in it
        for (src, dst) in [(&self.in_dir, "/work/in"), (&self.out_dir, "/work/out")] {
            a.push("--mount".into()); a.push(format!("type=bind,src={},dst={}", src.display(), dst));
        }
        a.push("-w".into()); a.push("/work".into());
        // keep the copy alive whatever the image's own entrypoint is
        a.push("--entrypoint".into()); a.push("sleep".into());
        a.push(self.image.clone());
        a.push("infinity".into());
        a
    }
}

/// The container engine.
pub trait Runtime: Send + Sync {
    /// "podman" or "docker" (for messages).
    fn name(&self) -> &str;
    /// Whether the engine answers at all (Err = why not).
    fn available(&self) -> Result<(), String>;
    fn image_present(&self, image: &str) -> bool;
    /// `run -d` the copy; Ok once it is started.
    fn run(&self, spec: &RunSpec) -> Result<(), String>;
    /// Run a command inside, passing its output to `sink` as it comes; killed when `timeout` passes.
    fn exec(&self, name: &str, argv: &[String], timeout: Duration, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<ExecEnd, String>;
    /// Remove the copy (forced). Ok when it is already gone.
    fn remove(&self, name: &str) -> Result<(), String>;
    /// The containers whose name starts with `prefix`.
    fn list(&self, prefix: &str) -> Result<Vec<Found>, String>;
    /// Delete a folder a copy wrote into (the engine may need its own user mapping for that).
    fn purge_dir(&self, dir: &Path) { let _ = std::fs::remove_dir_all(dir); }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flavour { Podman, Docker }

/// The `podman` / `docker` command line.
pub struct Cli { program: String, flavour: Flavour, env: Vec<(String, String)> }
impl Cli {
    pub fn podman() -> Cli { Cli { program: "podman".into(), flavour: Flavour::Podman, env: vec![] } }
    pub fn docker() -> Cli { Cli { program: "docker".into(), flavour: Flavour::Docker, env: vec![] } }
    /// "podman" / "docker" by name; "" or "auto": Podman on Linux and macOS, Docker on Windows.
    pub fn named(which: &str) -> Cli {
        match which.to_lowercase().as_str() { "docker" => Cli::docker(), "podman" => Cli::podman(), _ => if cfg!(windows) { Cli::docker() } else { Cli::podman() } }
    }
    /// Extra environment for every engine command (a HOME whose storage works, DOCKER_HOST, CONTAINERS_STORAGE_CONF).
    pub fn with_env(mut self, env: Vec<(String, String)>) -> Cli { self.env = env; self }
    fn cmd(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        #[cfg(windows)] { use std::os::windows::process::CommandExt; c.creation_flags(0x0800_0000); } // no console window
        c.stdin(Stdio::null());
        c
    }
    fn out(&self, args: &[&str]) -> Result<(bool, String, String), String> {
        let o = self.cmd().args(args).output().map_err(|e| format!("{} is not available: {e}", self.program))?;
        Ok((o.status.success(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).trim().to_string()))
    }
}
impl Runtime for Cli {
    fn name(&self) -> &str { &self.program }
    fn available(&self) -> Result<(), String> {
        match self.out(&["version"]) { Ok((true, ..)) => Ok(()), Ok((_, _, e)) => Err(format!("{} does not answer: {}", self.program, e.lines().next().unwrap_or(""))), Err(e) => Err(e) }
    }
    fn image_present(&self, image: &str) -> bool {
        let args: [&str; 3] = match self.flavour { Flavour::Podman => ["image", "exists", image], Flavour::Docker => ["image", "inspect", image] };
        self.out(&args).map(|o| o.0).unwrap_or(false)
    }
    fn run(&self, spec: &RunSpec) -> Result<(), String> {
        let args = spec.args();
        let o = self.cmd().arg("run").args(&args).output().map_err(|e| format!("{} is not available: {e}", self.program))?;
        if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().lines().last().unwrap_or("the container did not start").to_string()) }
    }
    fn exec(&self, name: &str, argv: &[String], timeout: Duration, sink: &mut dyn FnMut(Stream, &[u8])) -> Result<ExecEnd, String> {
        let mut child = self.cmd().args(["exec", "-w", "/work", name]).args(argv).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{} is not available: {e}", self.program))?;
        let (tx, rx) = mpsc::channel::<(Stream, Vec<u8>)>();
        let mut readers = vec![];
        for (which, pipe) in [(Stream::Stdout, child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)), (Stream::Stderr, child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>))] {
            let Some(mut pipe) = pipe else { continue };
            let tx = tx.clone();
            readers.push(std::thread::spawn(move || { let mut buf = [0u8; 8192]; while let Ok(n) = pipe.read(&mut buf) { if n == 0 || tx.send((which, buf[..n].to_vec())).is_err() { break; } } }));
        }
        drop(tx);
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((w, bytes)) => sink(w, &bytes),
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // both pipes closed: the command ended
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() >= deadline && !timed_out { timed_out = true; let _ = child.kill(); }
            if timed_out && Instant::now() >= deadline + Duration::from_secs(3) { break; } // a grandchild holds the pipes open: give up on them
        }
        let status = if timed_out { child.kill().ok(); child.wait().ok() } else { child.wait().ok() };
        for r in readers { if !timed_out { let _ = r.join(); } }
        // what arrived while the last poll was waiting
        while let Ok((w, bytes)) = rx.try_recv() { sink(w, &bytes); }
        Ok(ExecEnd { code: status.and_then(|s| s.code()), timed_out })
    }
    fn remove(&self, name: &str) -> Result<(), String> {
        let (ok, _, err) = self.out(&["rm", "-f", "-t", "0", name])?;
        if ok || err.to_lowercase().contains("no such") { Ok(()) } else { Err(err.lines().last().unwrap_or("could not remove the container").to_string()) }
    }
    fn list(&self, prefix: &str) -> Result<Vec<Found>, String> {
        let (ok, out, err) = self.out(&["ps", "-a", "--filter", &format!("name={prefix}"), "--format", "{{.Names}}\t{{.State}}"])?;
        if !ok { return Err(err.lines().last().unwrap_or("could not list containers").to_string()); }
        Ok(out.lines().filter_map(|l| { let (n, s) = l.split_once('\t')?; let n = n.trim(); if n.starts_with(prefix) { Some(Found { name: n.to_string(), running: s.trim().eq_ignore_ascii_case("running") }) } else { None } }).collect())
    }
    fn purge_dir(&self, dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        // files written by a container user are owned by a sub-uid of the person (rootless Podman): remove them from inside the user namespace
        if dir.exists() && self.flavour == Flavour::Podman {
            let _ = self.cmd().args(["unshare", "rm", "-rf"]).arg(dir).output();
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn run_args_are_locked_down() {
        let s = RunSpec { name: "pgm-re-ab12cd34".into(), image: "localhost/pgm-re-base:1.0.0".into(), network: false, cpus: "2".into(), memory: "4g".into(), pids_limit: 1024, cap_add: vec!["SYS_PTRACE".into()], in_dir: "/x/in".into(), out_dir: "/x/out".into() };
        let a = s.args().join(" ");
        for must in ["--rm", "--network none", "--cap-drop ALL", "--cap-add SYS_PTRACE", "--security-opt no-new-privileges", "--pids-limit 1024", "--memory 4g", "--cpus 2", "dst=/work/in", "dst=/work/out", "--pull=never", "--name pgm-re-ab12cd34"] { assert!(a.contains(must), "{must} missing in {a}"); }
        assert!(!a.contains("--privileged") && !a.contains("-v "));
        assert_eq!(a.matches("--mount").count(), 2, "only the two exchange mounts");
        let mut on = s.clone(); on.network = true;
        assert!(!on.args().join(" ").contains("--network"), "network on = the engine's default network");
        assert!(a.ends_with("localhost/pgm-re-base:1.0.0 infinity"));
    }
}
