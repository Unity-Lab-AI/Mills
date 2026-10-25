//! The one path canonicaliser (linked-machines.md section 7.4), used by every machine tool.
//!
//! Two layers, kept apart so the Windows rules can be tested on Linux:
//! - **pure** ([`NormPath`], [`parse_abs`], [`resolve`]): separators, `.` / `..`, drive / UNC / `\\?\` forms,
//!   alternate data streams, reserved names, case rules. No file system is asked.
//! - **file system** ([`canonicalize`], [`canonicalize_in`], [`open_checked`]): the real path (symlinks, junctions,
//!   mounts resolved), containment in the allowed roots, and an open that does not follow a swapped link.
//!
//! What is enforced where (honest list, see also [`open_checked`]):
//! - Linux: lexical `..` removal, real path via `canonicalize`, dangling links refused, parent directory opened with
//!   `O_NOFOLLOW`, the opened directory re-checked through `/proc/self/fd`, the file opened with `openat(...,
//!   O_NOFOLLOW)` relative to that directory handle and its real path re-checked after the open.
//! - Windows: the same lexical rules, real path via `canonicalize` (resolves junctions, short names), the file opened
//!   with `FILE_FLAG_OPEN_REPARSE_POINT`, refused if it is a reparse point, real path re-checked after the open. This
//!   is path-based, not handle-based, so a parent directory swapped between check and open is only detected after the
//!   fact (nothing is written through a link at the last component).

use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Posix,
    Windows,
}

impl Flavor {
    pub fn native() -> Flavor {
        if cfg!(windows) {
            Flavor::Windows
        } else {
            Flavor::Posix
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathError {
    Nul,
    Empty,
    NotUnicode,
    /// A relative path where an absolute one is needed (also `C:x`, and a rooted path without a drive).
    Relative,
    /// `..` climbed above the root it is resolved against.
    Escapes,
    /// `\\.\`, `\\?\GLOBALROOT`, `\??\`, `\\?\Volume{..}`.
    DevicePath,
    /// `file:stream` (Windows alternate data stream).
    AlternateStream,
    /// CON, NUL, COM1 ...
    Reserved(String),
    /// A character Windows cannot have in a name (`<>"|?*`, control characters), or a name of only dots / spaces.
    InvalidName(String),
    /// A 8.3 short name that cannot be resolved because the file does not exist yet.
    ShortName(String),
    /// A symlink that points nowhere (writing "through" it would create the target elsewhere).
    DanglingLink,
    /// The real path is outside every allowed root.
    Outside,
    /// A link was swapped in, or the real path changed, between the check and the open.
    Swapped,
    Io(String),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PathError::Nul => write!(f, "path contains a NUL byte"),
            PathError::Empty => write!(f, "empty path"),
            PathError::NotUnicode => write!(f, "path is not valid Unicode"),
            PathError::Relative => write!(f, "path is not absolute"),
            PathError::Escapes => write!(f, "path climbs above its root"),
            PathError::DevicePath => write!(f, "device paths are refused"),
            PathError::AlternateStream => write!(f, "alternate data streams are refused"),
            PathError::Reserved(n) => write!(f, "reserved name {n}"),
            PathError::InvalidName(n) => write!(f, "invalid name {n:?}"),
            PathError::ShortName(n) => write!(f, "unresolvable short name {n}"),
            PathError::DanglingLink => write!(f, "dangling symbolic link"),
            PathError::Outside => write!(f, "real path is outside the allowed folders"),
            PathError::Swapped => write!(f, "a link was swapped in after the check"),
            PathError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PathError {}

/// A normalised absolute path: a prefix (`""` on Linux, `C:` or `\\server\share` on Windows) and its components.
/// Case rules follow the flavor: Windows compares case-insensitively, Linux exactly.
#[derive(Clone, Debug)]
pub struct NormPath {
    pub flavor: Flavor,
    prefix: String,
    parts: Vec<String>,
}

impl PartialEq for NormPath {
    fn eq(&self, o: &Self) -> bool {
        self.flavor == o.flavor && self.prefix_key() == o.prefix_key() && self.parts_eq(&o.parts)
    }
}
impl Eq for NormPath {}

impl NormPath {
    fn key(&self, s: &str) -> String {
        match self.flavor {
            Flavor::Windows => s.to_lowercase(),
            Flavor::Posix => s.to_string(),
        }
    }
    fn prefix_key(&self) -> String {
        self.prefix.to_lowercase()
    }
    fn parts_eq(&self, other: &[String]) -> bool {
        self.parts.len() == other.len() && self.parts.iter().zip(other).all(|(a, b)| self.key(a) == self.key(b))
    }
    pub fn root(flavor: Flavor, prefix: &str) -> NormPath {
        NormPath { flavor, prefix: prefix.to_string(), parts: vec![] }
    }
    pub fn prefix(&self) -> &str {
        &self.prefix
    }
    pub fn parts(&self) -> &[String] {
        &self.parts
    }
    pub fn depth(&self) -> usize {
        self.parts.len()
    }
    /// Is `self` equal to `root` or below it? (Component-wise, so `/ab` is not under `/a`.)
    pub fn starts_with(&self, root: &NormPath) -> bool {
        self.flavor == root.flavor
            && self.prefix_key() == root.prefix_key()
            && root.parts.len() <= self.parts.len()
            && root.parts.iter().zip(&self.parts).all(|(a, b)| self.key(a) == self.key(b))
    }
    pub fn file_name(&self) -> Option<&str> {
        self.parts.last().map(|s| s.as_str())
    }
    pub fn parent(&self) -> Option<NormPath> {
        if self.parts.is_empty() {
            return None;
        }
        let mut p = self.clone();
        p.parts.pop();
        Some(p)
    }
    /// Append one component (already a plain name, validated by the flavor's rules).
    pub fn child(&self, name: &str) -> Result<NormPath, PathError> {
        let mut p = self.clone();
        p.parts.push(check_component(self.flavor, name)?);
        Ok(p)
    }
    /// The lower-cased (Windows) or exact (Linux) components, for glob matching.
    pub fn keyed_parts(&self) -> Vec<String> {
        self.parts.iter().map(|s| self.key(s)).collect()
    }
    /// The path in the flavor's own spelling (`\` and `C:\` on Windows, `/` on Linux). Never `\\?\`.
    pub fn display(&self) -> String {
        match self.flavor {
            Flavor::Posix => {
                if self.parts.is_empty() {
                    "/".to_string()
                } else {
                    format!("/{}", self.parts.join("/"))
                }
            }
            Flavor::Windows => {
                if self.parts.is_empty() {
                    format!("{}\\", self.prefix)
                } else {
                    format!("{}\\{}", self.prefix, self.parts.join("\\"))
                }
            }
        }
    }
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(self.display())
    }
    /// The `\\?\` extended form (Windows only; Linux returns the plain path).
    pub fn extended(&self) -> String {
        match self.flavor {
            Flavor::Posix => self.display(),
            Flavor::Windows => {
                if let Some(unc) = self.prefix.strip_prefix("\\\\") {
                    format!("\\\\?\\UNC\\{}\\{}", unc, self.parts.join("\\"))
                } else {
                    format!("\\\\?\\{}", self.display())
                }
            }
        }
    }
    /// The same path as seen from the machine's own native flavor.
    pub fn from_native(p: &Path) -> Result<NormPath, PathError> {
        let s = p.to_str().ok_or(PathError::NotUnicode)?;
        parse_abs(Flavor::native(), s)
    }
}

const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"];

fn is_reserved_name(comp: &str) -> bool {
    let stem = comp.split('.').next().unwrap_or("").trim_end_matches(' ').to_uppercase();
    if RESERVED.contains(&stem.as_str()) || stem == "CLOCK$" {
        return true;
    }
    for p in ["COM", "LPT"] {
        if let Some(d) = stem.strip_prefix(p) {
            if d.len() == 1 && matches!(d.as_bytes()[0], b'0'..=b'9') {
                return true;
            }
            if d == "\u{b9}" || d == "\u{b2}" || d == "\u{b3}" {
                return true;
            }
        }
    }
    false
}

/// A 8.3 short name such as `PROGRA~1` or `LONGNA~2.TXT`.
pub fn looks_like_short_name(comp: &str) -> bool {
    let (stem, ext) = match comp.rsplit_once('.') {
        Some((s, e)) => (s, e),
        None => (comp, ""),
    };
    if stem.len() < 3 || stem.len() > 8 || ext.len() > 3 {
        return false;
    }
    match stem.rsplit_once('~') {
        Some((head, digits)) => !head.is_empty() && !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Validate one component under the flavor's rules; returns it as the OS will see it (Windows: trailing dots and
/// spaces removed). `.` and `..` are NOT accepted here.
fn check_component(flavor: Flavor, comp: &str) -> Result<String, PathError> {
    if comp.contains('\0') {
        return Err(PathError::Nul);
    }
    if flavor == Flavor::Posix {
        if comp.is_empty() || comp == "." || comp == ".." || comp.contains('/') {
            return Err(PathError::InvalidName(comp.to_string()));
        }
        return Ok(comp.to_string());
    }
    if comp.contains(':') {
        return Err(PathError::AlternateStream);
    }
    if comp.chars().any(|c| (c as u32) < 32 || matches!(c, '<' | '>' | '"' | '|' | '?' | '*' | '/' | '\\')) {
        return Err(PathError::InvalidName(comp.to_string()));
    }
    let trimmed = comp.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return Err(PathError::InvalidName(comp.to_string()));
    }
    if is_reserved_name(trimmed) {
        return Err(PathError::Reserved(trimmed.to_string()));
    }
    Ok(trimmed.to_string())
}

fn split_components(flavor: Flavor, rest: &str) -> Vec<String> {
    match flavor {
        Flavor::Posix => rest.split('/').map(|s| s.to_string()).collect(),
        Flavor::Windows => rest.split('\\').map(|s| s.to_string()).collect(),
    }
}

fn apply_components(
    flavor: Flavor,
    parts: &mut Vec<String>,
    floor_len: usize,
    comps: &[String],
) -> Result<(), PathError> {
    for c in comps {
        if c.is_empty() || c == "." {
            continue;
        }
        if c == ".." {
            if parts.len() <= floor_len {
                return Err(PathError::Escapes);
            }
            parts.pop();
            continue;
        }
        parts.push(check_component(flavor, c)?);
    }
    Ok(())
}

/// Split an absolute Windows path into (prefix, remainder after the prefix).
fn windows_prefix(s: &str) -> Result<(String, String), PathError> {
    let s = s.replace('/', "\\");
    let lower = s.to_lowercase();
    let mut rest: &str = &s;
    let mut verbatim = false;
    if lower.starts_with("\\\\?\\") || lower.starts_with("\\??\\") {
        verbatim = true;
        rest = &s[4..];
    } else if lower.starts_with("\\\\.\\") {
        return Err(PathError::DevicePath);
    }
    if verbatim {
        let rl = rest.to_lowercase();
        if rl.starts_with("globalroot") || rl.starts_with("volume{") || rl.starts_with("harddisk") {
            return Err(PathError::DevicePath);
        }
        if rl.starts_with("unc\\") {
            let unc = format!("\\\\{}", &rest[4..]);
            return windows_unc(&unc);
        }
    }
    let b = rest.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        if b.len() == 2 || b[2] != b'\\' {
            return Err(PathError::Relative); // `C:x` is relative to the drive's current directory
        }
        let drive = format!("{}:", (b[0] as char).to_ascii_uppercase());
        return Ok((drive, rest[3..].to_string()));
    }
    if !verbatim && rest.starts_with("\\\\") {
        return windows_unc(rest);
    }
    if verbatim {
        return Err(PathError::DevicePath);
    }
    Err(PathError::Relative)
}

fn windows_unc(s: &str) -> Result<(String, String), PathError> {
    let body = s.trim_start_matches('\\');
    let mut it = body.splitn(3, '\\');
    let server = it.next().unwrap_or("");
    let share = it.next().unwrap_or("");
    if server.is_empty() || share.is_empty() {
        return Err(PathError::Relative);
    }
    if server == "." || server == "?" {
        return Err(PathError::DevicePath);
    }
    let server = check_component(Flavor::Windows, server)?;
    let share = check_component(Flavor::Windows, share)?;
    Ok((format!("\\\\{server}\\{share}"), it.next().unwrap_or("").to_string()))
}

/// Parse an absolute path (no base) under `flavor`'s rules.
pub fn parse_abs(flavor: Flavor, s: &str) -> Result<NormPath, PathError> {
    if s.contains('\0') {
        return Err(PathError::Nul);
    }
    if s.is_empty() {
        return Err(PathError::Empty);
    }
    match flavor {
        Flavor::Posix => {
            if !s.starts_with('/') {
                return Err(PathError::Relative);
            }
            let mut parts = Vec::new();
            apply_components(flavor, &mut parts, 0, &split_components(flavor, s))?;
            Ok(NormPath { flavor, prefix: String::new(), parts })
        }
        Flavor::Windows => {
            let (prefix, rest) = windows_prefix(s)?;
            let mut parts = Vec::new();
            apply_components(flavor, &mut parts, 0, &split_components(flavor, &rest))?;
            Ok(NormPath { flavor, prefix, parts })
        }
    }
}

fn is_absolute_form(flavor: Flavor, s: &str) -> bool {
    match flavor {
        Flavor::Posix => s.starts_with('/'),
        Flavor::Windows => {
            let b = s.as_bytes();
            s.starts_with('\\') && (s.starts_with("\\\\") || s.starts_with("\\?")) || s.starts_with("//") || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
        }
    }
}

/// Join `given` onto `base` and normalise, lexically, before the file system is asked.
/// - an absolute `given` stands alone (the caller checks the roots afterwards);
/// - a relative `given` may use `..` but never below `base` ([`PathError::Escapes`]);
/// - an empty `given` is the base itself for a read (`for_write == false`) and an error for a write.
pub fn resolve(base: &NormPath, given: &str, for_write: bool) -> Result<NormPath, PathError> {
    let flavor = base.flavor;
    if given.contains('\0') {
        return Err(PathError::Nul);
    }
    if given.is_empty() {
        return if for_write { Err(PathError::Empty) } else { Ok(base.clone()) };
    }
    if is_absolute_form(flavor, given) {
        return parse_abs(flavor, given);
    }
    let mut out = base.clone();
    let floor_len;
    let mut rest = given.to_string();
    match flavor {
        Flavor::Posix => {
            floor_len = base.parts.len();
        }
        Flavor::Windows => {
            let b = given.as_bytes();
            if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
                return Err(PathError::Relative); // drive-relative
            }
            if given.starts_with('\\') || given.starts_with('/') {
                // rooted on the base's drive / share
                out.parts.clear();
                floor_len = 0;
                rest = given.trim_start_matches(['\\', '/']).to_string();
            } else {
                floor_len = base.parts.len();
            }
        }
    }
    let rest = if flavor == Flavor::Windows { rest.replace('/', "\\") } else { rest };
    apply_components(flavor, &mut out.parts, floor_len, &split_components(flavor, &rest))?;
    Ok(out)
}

// ---------------------------------------------------------------- file system layer

#[derive(Clone, Debug)]
pub struct Resolved {
    /// The path after lexical normalisation only (`..` gone, links NOT resolved).
    pub lexical: NormPath,
    /// The real path: symlinks, junctions, mounts and (Windows) short names resolved.
    pub real: NormPath,
    /// False for a path that does not exist yet (a write): `real` is the real nearest parent plus the new tail.
    pub exists: bool,
}

fn io_err(e: std::io::Error) -> PathError {
    PathError::Io(e.to_string())
}

/// Real path of `lexical`, also when its last components do not exist yet.
pub(crate) fn real_of(lexical: &NormPath) -> Result<(NormPath, bool), PathError> {
    let flavor = lexical.flavor;
    let mut tail: Vec<String> = Vec::new();
    let mut cur = lexical.clone();
    loop {
        let pb = cur.to_path_buf();
        match std::fs::canonicalize(&pb) {
            Ok(real) => {
                let mut r = NormPath::from_native(&real)?;
                r.flavor = flavor;
                let exists = tail.is_empty();
                for t in tail.iter().rev() {
                    if flavor == Flavor::Windows && looks_like_short_name(t) {
                        return Err(PathError::ShortName(t.clone()));
                    }
                    r.parts.push(t.clone());
                }
                return Ok((r, exists));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // a dangling symlink is NotFound too: writing through it would create the target elsewhere
                if std::fs::symlink_metadata(&pb).is_ok() {
                    return Err(PathError::DanglingLink);
                }
                match cur.parts.pop() {
                    Some(last) => tail.push(last),
                    None => return Err(PathError::Io(e.to_string())),
                }
            }
            Err(e) => return Err(io_err(e)),
        }
    }
}

/// Canonicalise `given` (joined onto `base`) against the real file system. No root check: see [`canonicalize_in`].
pub fn canonicalize(base: &Path, given: &str, for_write: bool) -> Result<Resolved, PathError> {
    let b = NormPath::from_native(base)?;
    let lexical = resolve(&b, given, for_write)?;
    let (real, exists) = real_of(&lexical)?;
    Ok(Resolved { lexical, real, exists })
}

/// The canonicaliser every tool uses: join `given` onto the first root (relative paths), resolve, and DENY when the
/// final real path is outside every allowed root. A link inside a root that leads outside is denied
/// (`follow_links == false`, the default); with `follow_links == true` the lexical path decides instead.
pub fn canonicalize_in(
    roots: &[PathBuf],
    given: &str,
    for_write: bool,
    follow_links: bool,
) -> Result<Resolved, PathError> {
    let first = roots.first().ok_or(PathError::Outside)?;
    let r = canonicalize(first, given, for_write)?;
    if root_contains(roots, &r, follow_links)? {
        Ok(r)
    } else {
        Err(PathError::Outside)
    }
}

fn root_contains(roots: &[PathBuf], r: &Resolved, follow_links: bool) -> Result<bool, PathError> {
    for root in roots {
        let lex_root = NormPath::from_native(root)?;
        if follow_links {
            if r.lexical.starts_with(&lex_root) {
                return Ok(true);
            }
        }
        // the real root: the root itself may be reached through a link (/tmp -> /private/tmp style)
        let real_root = match std::fs::canonicalize(root) {
            Ok(p) => NormPath::from_native(&p)?,
            Err(_) => continue,
        };
        if r.real.starts_with(&real_root) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    Read,
    /// Create if missing, truncate (after the check, never before).
    WriteTruncate,
    Append,
    /// Create, fail if it exists.
    CreateNew,
}

/// Open a path that [`canonicalize_in`] approved, without following a link swapped in since. See the module docs for
/// the exact guarantee per OS. `roots` are re-checked against what was really opened.
pub fn open_checked(r: &Resolved, roots: &[PathBuf], mode: OpenMode) -> Result<std::fs::File, PathError> {
    open_verified(r, &|p| real_inside(roots, p), mode)
}

/// Like [`open_checked`], with the re-check given as a function: it is called with the real path of the directory
/// and of the file that were actually opened, and must say whether they are still acceptable (the scope evaluator
/// passes itself here, so deny rules and the floor are applied to what was really opened).
pub fn open_verified(r: &Resolved, verify: &dyn Fn(&Path) -> bool, mode: OpenMode) -> Result<std::fs::File, PathError> {
    open_impl(r, verify, mode)
}

fn real_inside(roots: &[PathBuf], real: &Path) -> bool {
    let Ok(rp) = NormPath::from_native(real) else { return false };
    roots.iter().any(|root| match std::fs::canonicalize(root).ok().and_then(|p| NormPath::from_native(&p).ok()) {
        Some(rr) => rp.starts_with(&rr),
        None => false,
    })
}

#[cfg(target_os = "linux")]
fn open_impl(r: &Resolved, verify: &dyn Fn(&Path) -> bool, mode: OpenMode) -> Result<std::fs::File, PathError> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    struct Fd(OwnedFd);
    let parent = r.real.parent().ok_or(PathError::Outside)?;
    let name = r.real.file_name().ok_or(PathError::Outside)?.to_string();
    let cpar = CString::new(parent.display().as_bytes()).map_err(|_| PathError::Nul)?;
    let cname = CString::new(name.as_bytes()).map_err(|_| PathError::Nul)?;
    // 1. the parent directory, never through a link at its last component
    let dfd = unsafe { libc::open(cpar.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
    if dfd < 0 {
        return Err(swap_or_io(std::io::Error::last_os_error()));
    }
    let dir = Fd(unsafe { OwnedFd::from_raw_fd(dfd) });
    // 2. what was really opened must still be inside the roots (an ancestor swapped for a link shows here)
    let dreal = std::fs::read_link(format!("/proc/self/fd/{}", dir.0.as_raw_fd())).map_err(io_err)?;
    if !verify(&dreal) {
        return Err(PathError::Swapped);
    }
    // 3. the file, relative to that handle, never following a link at the last component
    let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK;
    match mode {
        OpenMode::Read => flags |= libc::O_RDONLY,
        OpenMode::WriteTruncate => flags |= libc::O_WRONLY | libc::O_CREAT,
        OpenMode::Append => flags |= libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
        OpenMode::CreateNew => flags |= libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
    }
    let fd = unsafe { libc::openat(dir.0.as_raw_fd(), cname.as_ptr(), flags, 0o666 as libc::c_uint) };
    if fd < 0 {
        return Err(swap_or_io(std::io::Error::last_os_error()));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    // 4. what was opened is a regular file or directory, and its real path is inside the roots
    let freal = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).map_err(io_err)?;
    if !verify(&freal) {
        return Err(PathError::Swapped);
    }
    if mode == OpenMode::WriteTruncate {
        file.set_len(0).map_err(io_err)?;
    }
    // O_NONBLOCK was only to stop a FIFO swapped in from hanging the open; clear it again
    unsafe {
        let fl = libc::fcntl(file.as_raw_fd(), libc::F_GETFL);
        libc::fcntl(file.as_raw_fd(), libc::F_SETFL, fl & !libc::O_NONBLOCK);
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn swap_or_io(e: std::io::Error) -> PathError {
    if e.raw_os_error() == Some(libc::ELOOP) || e.raw_os_error() == Some(libc::ENOTDIR) {
        PathError::Swapped
    } else {
        io_err(e)
    }
}

#[cfg(windows)]
fn open_impl(r: &Resolved, verify: &dyn Fn(&Path) -> bool, mode: OpenMode) -> Result<std::fs::File, PathError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let path = r.real.to_path_buf();
    let mut o = std::fs::OpenOptions::new();
    o.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    match mode {
        OpenMode::Read => {
            o.read(true);
        }
        OpenMode::WriteTruncate => {
            o.write(true).create(true);
        }
        OpenMode::Append => {
            o.append(true).create(true);
        }
        OpenMode::CreateNew => {
            o.write(true).create_new(true);
        }
    }
    let file = o.open(&path).map_err(io_err)?;
    let md = file.metadata().map_err(io_err)?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PathError::Swapped);
    }
    let again = std::fs::canonicalize(&path).map_err(io_err)?;
    if !verify(&again) || NormPath::from_native(&again)? != r.real {
        return Err(PathError::Swapped);
    }
    if mode == OpenMode::WriteTruncate {
        file.set_len(0).map_err(io_err)?;
    }
    Ok(file)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn open_impl(_r: &Resolved, _verify: &dyn Fn(&Path) -> bool, _mode: OpenMode) -> Result<std::fs::File, PathError> {
    Err(PathError::Io("checked open is only implemented for Linux and Windows".into()))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Result<NormPath, PathError> {
        parse_abs(Flavor::Windows, s)
    }
    fn p(s: &str) -> Result<NormPath, PathError> {
        parse_abs(Flavor::Posix, s)
    }

    #[test]
    fn windows_pure_table() {
        // (input, expected display or error)
        let ok: &[(&str, &str)] = &[
            ("C:\\x", "C:\\x"),
            ("c:/X", "C:\\X"),
            ("C:\\a\\.\\b\\..\\c", "C:\\a\\c"),
            ("C:\\a\\b.  ", "C:\\a\\b"),
            ("\\\\?\\C:\\People\\x", "C:\\People\\x"),
            ("\\\\?\\UNC\\srv\\share\\d", "\\\\srv\\share\\d"),
            ("\\\\srv\\share\\d\\..\\e", "\\\\srv\\share\\e"),
            ("D:\\", "D:\\"),
            ("\\??\\C:\\x", "C:\\x"),
        ];
        for (i, want) in ok {
            assert_eq!(w(i).unwrap().display(), *want, "{i}");
        }
        let bad: &[(&str, PathError)] = &[
            ("C:\\a\\..\\..", PathError::Escapes),
            ("\\\\.\\PhysicalDrive0", PathError::DevicePath),
            ("\\\\?\\GLOBALROOT\\Device\\x", PathError::DevicePath),
            ("\\\\?\\Volume{1234}\\x", PathError::DevicePath),
            ("C:\\x\\file.txt:stream", PathError::AlternateStream),
            ("C:\\x\\file.txt::$DATA", PathError::AlternateStream),
            ("C:\\dir:ads\\f", PathError::AlternateStream),
            ("C:relative", PathError::Relative),
            ("relative\\x", PathError::Relative),
            ("\\rooted", PathError::Relative),
            ("C:\\a\\...\\b", PathError::InvalidName("...".into())),
            ("C:\\a\\b*", PathError::InvalidName("b*".into())),
            ("C:\\a\\x|y", PathError::InvalidName("x|y".into())),
            ("\\\\server", PathError::Relative),
            ("C:\\a\0b", PathError::Nul),
            ("", PathError::Empty),
        ];
        for (i, want) in bad {
            let got = w(i);
            assert_eq!(got.unwrap_err(), *want, "{i:?}");
        }
    }

    #[test]
    fn windows_reserved_names() {
        for n in ["CON", "con", "NUL", "nul.txt", "Com1", "COM9.log", "LPT3", "aux", "PRN.", "CONIN$", "CONOUT$", "NUL  "] {
            let path = format!("C:\\dir\\{n}");
            assert!(matches!(w(&path), Err(PathError::Reserved(_))), "{n}");
        }
        for n in ["COM10", "console", "nulx", "com", "LPT", "x.con"] {
            assert!(w(&format!("C:\\dir\\{n}")).is_ok(), "{n}");
        }
    }

    #[test]
    fn windows_case_and_prefix_rules() {
        let a = w("C:\\People\\Sponge").unwrap();
        assert_eq!(a, w("c:/people/sponge").unwrap());
        assert!(w("c:\\PEOPLE\\sponge\\x").unwrap().starts_with(&a));
        assert!(!w("C:\\People\\Sponge2").unwrap().starts_with(&a));
        assert!(!w("D:\\Users\\Sponge").unwrap().starts_with(&a), "another drive is outside");
        assert!(!w("\\\\srv\\share\\Users\\Sponge").unwrap().starts_with(&a), "UNC is outside a drive");
        let unc = w("\\\\Srv\\Share\\a").unwrap();
        assert!(w("\\\\?\\UNC\\srv\\SHARE\\a\\b").unwrap().starts_with(&unc));
        assert!(!w("\\\\srv\\other\\a\\b").unwrap().starts_with(&unc));
        assert!(!w("\\\\srv2\\share\\a\\b").unwrap().starts_with(&unc));
        assert_eq!(w("C:\\a").unwrap().extended(), "\\\\?\\C:\\a");
        assert_eq!(w("\\\\srv\\sh\\a").unwrap().extended(), "\\\\?\\UNC\\srv\\sh\\a");
    }

    #[test]
    fn windows_resolve_relative() {
        let base = w("C:\\Work\\proj").unwrap();
        let r = |g: &str| resolve(&base, g, false).map(|p| p.display());
        assert_eq!(r("src\\a.rs").unwrap(), "C:\\Work\\proj\\src\\a.rs");
        assert_eq!(r("src/../b").unwrap(), "C:\\Work\\proj\\b");
        assert_eq!(r("..").unwrap_err(), PathError::Escapes);
        assert_eq!(r("a\\..\\..\\x").unwrap_err(), PathError::Escapes);
        assert_eq!(r("D:\\other").unwrap(), "D:\\other", "an absolute path stands alone; the roots decide");
        assert_eq!(r("D:other").unwrap_err(), PathError::Relative);
        assert_eq!(r("\\x").unwrap(), "C:\\x");
        assert_eq!(r("").unwrap(), "C:\\Work\\proj");
        assert_eq!(resolve(&base, "", true).unwrap_err(), PathError::Empty);
        assert_eq!(r("f.txt:s").unwrap_err(), PathError::AlternateStream);
        assert_eq!(r("NUL").unwrap_err(), PathError::Reserved("NUL".into()));
    }

    #[test]
    fn short_names() {
        for n in ["PROGRA~1", "LONGNA~2.TXT", "a~1", "abcdef~1.c"] {
            assert!(looks_like_short_name(n), "{n}");
        }
        for n in ["plain.txt", "~tmp", "a~b", "verylongname~1", "x~1.html"] {
            assert!(!looks_like_short_name(n), "{n}");
        }
    }

    #[test]
    fn posix_pure_table() {
        assert_eq!(p("/a/./b/../c").unwrap().display(), "/a/c");
        assert_eq!(p("/a//b/").unwrap().display(), "/a/b");
        assert_eq!(p("/").unwrap().display(), "/");
        assert_eq!(p("/..").unwrap_err(), PathError::Escapes);
        assert_eq!(p("/a/../..").unwrap_err(), PathError::Escapes);
        assert_eq!(p("a/b").unwrap_err(), PathError::Relative);
        assert_eq!(p("/a\0b").unwrap_err(), PathError::Nul);
        assert_eq!(p("").unwrap_err(), PathError::Empty);
        // a backslash is a real character on Linux and is not converted; case counts
        assert_eq!(p("/a/b\\c").unwrap().parts(), &["a".to_string(), "b\\c".to_string()]);
        assert_ne!(p("/A").unwrap(), p("/a").unwrap());
        assert!(p("/a:b/CON").is_ok(), "Windows rules do not apply on Linux");
        assert!(!p("/ab").unwrap().starts_with(&p("/a").unwrap()));
        let base = p("/srv/root").unwrap();
        assert_eq!(resolve(&base, "x/../y", false).unwrap().display(), "/srv/root/y");
        assert_eq!(resolve(&base, "../x", false).unwrap_err(), PathError::Escapes);
        assert_eq!(resolve(&base, "/etc/passwd", false).unwrap().display(), "/etc/passwd");
        assert_eq!(resolve(&base, "", false).unwrap(), base);
        assert_eq!(resolve(&base, "", true).unwrap_err(), PathError::Empty);
        assert_eq!(resolve(&base, "a\0", false).unwrap_err(), PathError::Nul);
    }

    #[cfg(unix)]
    mod fs {
        use super::super::*;
        use std::os::unix::fs::symlink;

        pub struct Tmp(pub PathBuf);
        impl Tmp {
            pub fn new(tag: &str) -> Tmp {
                let d = std::env::temp_dir().join(format!("pgm-machine-{}-{}-{:?}", tag, std::process::id(), std::thread::current().id()).replace(['(', ')'], ""));
                let _ = std::fs::remove_dir_all(&d);
                std::fs::create_dir_all(&d).unwrap();
                Tmp(std::fs::canonicalize(&d).unwrap())
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn dotdot_and_absolute_escape() {
            let t = Tmp::new("dd");
            let root = t.0.join("root");
            std::fs::create_dir_all(root.join("sub")).unwrap();
            std::fs::write(t.0.join("secret"), "s").unwrap();
            let roots = vec![root.clone()];
            assert!(canonicalize_in(&roots, "sub/../sub/f", true, false).is_ok());
            assert_eq!(canonicalize_in(&roots, "../secret", false, false).unwrap_err(), PathError::Escapes);
            assert_eq!(canonicalize_in(&roots, "sub/../../secret", false, false).unwrap_err(), PathError::Escapes);
            let abs = t.0.join("secret");
            assert_eq!(canonicalize_in(&roots, abs.to_str().unwrap(), false, false).unwrap_err(), PathError::Outside);
            assert_eq!(canonicalize_in(&roots, "a\0b", false, false).unwrap_err(), PathError::Nul);
            assert_eq!(canonicalize_in(&roots, "", true, false).unwrap_err(), PathError::Empty);
            assert_eq!(canonicalize_in(&roots, "", false, false).unwrap().real.display(), root.to_str().unwrap());
            // a backslash name is just a name
            let r = canonicalize_in(&roots, "a\\b", true, false).unwrap();
            assert_eq!(r.real.file_name(), Some("a\\b"));
            assert!(!r.exists);
        }

        #[test]
        fn symlink_escape_and_inside_links() {
            let t = Tmp::new("sl");
            let root = t.0.join("root");
            let out = t.0.join("outside");
            std::fs::create_dir_all(root.join("real")).unwrap();
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join("secret"), "s").unwrap();
            symlink(&out, root.join("escape")).unwrap();
            symlink(out.join("secret"), root.join("flink")).unwrap();
            symlink(root.join("real"), root.join("inside")).unwrap();
            symlink(root.join("nothing-here"), root.join("dangling")).unwrap();
            symlink(t.0.join("nowhere"), root.join("dangling-out")).unwrap();
            let roots = vec![root.clone()];
            let ck = |g: &str, w: bool| canonicalize_in(&roots, g, w, false);
            assert_eq!(ck("escape/secret", false).unwrap_err(), PathError::Outside);
            assert_eq!(ck("escape/new", true).unwrap_err(), PathError::Outside, "write below an escaping link");
            assert_eq!(ck("flink", false).unwrap_err(), PathError::Outside);
            assert_eq!(ck("dangling-out", true).unwrap_err(), PathError::DanglingLink);
            assert_eq!(ck("dangling", true).unwrap_err(), PathError::DanglingLink);
            let ok = ck("inside/f", true).unwrap();
            assert_eq!(ok.real.display(), root.join("real/f").to_str().unwrap(), "a link that stays inside resolves to its target");
            // followLinks: the lexical path decides
            assert!(canonicalize_in(&roots, "escape/secret", false, true).is_ok());
            // the root itself reached through a link
            let alias = t.0.join("alias");
            symlink(&root, &alias).unwrap();
            assert!(canonicalize_in(&[alias.clone()], "real", false, false).is_ok());
            // /proc/self/root style magic links resolve to the real thing
            let r = canonicalize(&root, "/proc/self/root/etc", false).unwrap();
            assert_eq!(r.real.display(), "/etc");
        }

        #[test]
        fn proc_magic_links_cannot_leave() {
            let t = Tmp::new("pm");
            let roots = vec![t.0.clone()];
            assert_eq!(canonicalize_in(&roots, "/proc/self/cwd/..", false, false).is_err(), true);
            assert_eq!(canonicalize_in(&roots, "/proc/self/root/etc/hostname", false, false).unwrap_err(), PathError::Outside);
        }

        #[test]
        fn open_checked_modes_and_swapped_links() {
            use std::io::{Read, Write};
            let t = Tmp::new("op");
            let root = t.0.join("root");
            let out = t.0.join("outside");
            std::fs::create_dir_all(root.join("d")).unwrap();
            std::fs::create_dir_all(&out).unwrap();
            std::fs::write(out.join("victim"), "keep").unwrap();
            let roots = vec![root.clone()];

            // plain create / append / read / truncate / create-new
            let r = canonicalize_in(&roots, "d/f.txt", true, false).unwrap();
            let mut f = open_checked(&r, &roots, OpenMode::WriteTruncate).unwrap();
            f.write_all(b"hello").unwrap();
            let r = canonicalize_in(&roots, "d/f.txt", true, false).unwrap();
            let mut f = open_checked(&r, &roots, OpenMode::Append).unwrap();
            f.write_all(b"!").unwrap();
            let mut s = String::new();
            open_checked(&r, &roots, OpenMode::Read).unwrap().read_to_string(&mut s).unwrap();
            assert_eq!(s, "hello!");
            assert!(open_checked(&r, &roots, OpenMode::CreateNew).is_err(), "create-new on an existing file");
            let _ = open_checked(&r, &roots, OpenMode::WriteTruncate).unwrap();
            assert_eq!(std::fs::read_to_string(root.join("d/f.txt")).unwrap(), "");

            // TOCTOU 1: the file is swapped for a link to outside after the check
            let r = canonicalize_in(&roots, "d/swap.txt", true, false).unwrap();
            symlink(out.join("victim"), root.join("d/swap.txt")).unwrap();
            let e = open_checked(&r, &roots, OpenMode::WriteTruncate).unwrap_err();
            assert_eq!(e, PathError::Swapped);
            assert_eq!(std::fs::read_to_string(out.join("victim")).unwrap(), "keep", "nothing was written or truncated");

            // TOCTOU 2: the parent directory is swapped for a link to outside after the check
            std::fs::create_dir_all(root.join("sub")).unwrap();
            let r = canonicalize_in(&roots, "sub/new.txt", true, false).unwrap();
            std::fs::remove_dir_all(root.join("sub")).unwrap();
            symlink(&out, root.join("sub")).unwrap();
            assert_eq!(open_checked(&r, &roots, OpenMode::CreateNew).unwrap_err(), PathError::Swapped);
            assert!(!out.join("new.txt").exists());

            // TOCTOU 3: an ancestor swapped for a link to outside
            std::fs::create_dir_all(root.join("a/b")).unwrap();
            let r = canonicalize_in(&roots, "a/b/n.txt", true, false).unwrap();
            std::fs::rename(root.join("a"), t.0.join("moved")).unwrap();
            symlink(&out, root.join("a")).unwrap();
            std::fs::create_dir_all(out.join("b")).unwrap();
            assert_eq!(open_checked(&r, &roots, OpenMode::CreateNew).unwrap_err(), PathError::Swapped);
            assert!(!out.join("b/n.txt").exists());

            // a FIFO swapped in does not hang the open
            let r = canonicalize_in(&roots, "d/fifo", true, false).unwrap();
            let c = std::ffi::CString::new(root.join("d/fifo").to_str().unwrap()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            let _ = open_checked(&r, &roots, OpenMode::Read);
        }
    }
}
