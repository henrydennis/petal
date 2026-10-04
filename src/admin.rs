//! Doing things as an administrator: reading folders that belong to macOS or other users,
//! and deleting Time Machine snapshots.
//!
//! Petal itself never runs as root. For each job it asks for an administrator's password
//! (through Authorization Services, so the dialog names Petal) and starts its own binary
//! again as `petal --admin <job file>`. That helper does the one job, writes the result to
//! the pipe it shares with the app, and exits. It reads metadata only, exactly as the scan
//! does; the only thing it ever deletes is a Time Machine snapshot.

use std::collections::HashSet;
use std::ffi::{CString, OsStr};
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::scan::{self, FoldersRead, Fresh, Kind, Raw, Stale, Unreadable};

/// Something to do as an administrator.
pub enum Job {
    /// Read these folders of the tree rooted at `root` again (see `scan::read_folders`).
    Read { root: PathBuf, stale: Vec<Stale>, hardlinks: HashSet<(u64, u64)> },
    /// Delete these Time Machine snapshots of the volume on `device` (e.g. "disk3s5").
    DeleteSnapshots { device: String, names: Vec<String> },
}

pub enum Outcome {
    Read(FoldersRead),
    /// The snapshots that couldn't be deleted, with why.
    Deleted { failed: Vec<(String, String)> },
}

#[derive(Debug)]
pub enum Error {
    /// The user cancelled the password dialog.
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Cancelled => write!(f, "cancelled"),
            Error::Failed(why) => write!(f, "{why}"),
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Failed(error.to_string())
    }
}

const JOB_MAGIC: &[u8; 4] = b"PTLJ";
const REPLY_MAGIC: &[u8; 4] = b"PTLR";
const END: &[u8; 4] = b"END!";

/// Do `job` as an administrator, asking for a password with `prompt`. Blocks until it's
/// done (or the dialog is cancelled), so call it off the UI thread.
pub fn run(job: &Job, prompt: &str) -> Result<Outcome, Error> {
    let exe = std::env::current_exe()?;
    let job_file = JobFile::write(job)?;
    let authorization = Authorization::new(prompt)?;
    let pipe = authorization.execute(&exe, &[OsStr::new("--admin"), job_file.path.as_os_str()])?;
    read_reply(pipe)
}

/// Run the helper as this user, for tests: the same protocol, without the password.
#[cfg(test)]
fn run_unprivileged(job: &Job) -> Result<Outcome, Error> {
    let exe = std::env::var_os("CARGO_BIN_EXE_petal").map(PathBuf::from).unwrap_or_else(|| {
        // Unit tests run from target/<profile>/deps; the binary sits one level up.
        let mut path = std::env::current_exe().unwrap();
        path.pop();
        path.pop();
        path.join("petal")
    });
    let job_file = JobFile::write(job)?;
    let mut child = std::process::Command::new(exe).arg("--admin").arg(&job_file.path).stdout(std::process::Stdio::piped()).spawn()?;
    let stdout = child.stdout.take().unwrap();
    // (`read_reply` reaps the child.)
    read_reply(fs::File::from(std::os::fd::OwnedFd::from(stdout)))
}

/// The job, written to a file in the user's private temporary folder for the helper to
/// read; removed when dropped.
struct JobFile {
    path: PathBuf,
}

impl JobFile {
    fn write(job: &Job) -> io::Result<Self> {
        let mut template = std::env::temp_dir().join("petal-admin-XXXXXX").into_os_string().into_vec();
        template.push(0);
        let fd = unsafe { libc::mkstemp(template.as_mut_ptr().cast()) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        template.pop();
        let path = PathBuf::from(OsStr::from_bytes(&template));
        let file = unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) };
        let this = JobFile { path };
        let mut out = Out(BufWriter::new(file));
        out.0.write_all(JOB_MAGIC)?;
        encode_job(&mut out, job)?;
        out.0.flush()?;
        Ok(this)
    }
}

impl Drop for JobFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Read the helper's reply, then reap it.
fn read_reply(pipe: fs::File) -> Result<Outcome, Error> {
    let mut input = In(BufReader::with_capacity(1 << 20, pipe));
    let mut magic = [0u8; 4];
    input.0.read_exact(&mut magic).map_err(|_| Error::Failed("the administrator helper didn't start".into()))?;
    if &magic != REPLY_MAGIC {
        return Err(Error::Failed("the administrator helper sent something unexpected".into()));
    }
    let pid = input.u64()? as libc::pid_t;
    let parent = input.u64()? as libc::pid_t;
    let result = decode_reply(&mut input);
    // Authorization Services may start the helper directly, or through a trampoline that
    // starts it; either way, wait for the process that is ours.
    let ours = if parent == unsafe { libc::getpid() } { pid } else { parent };
    let mut status = 0;
    unsafe { libc::waitpid(ours, &mut status, 0) };
    result
}

fn decode_reply<R: Read>(input: &mut In<R>) -> Result<Outcome, Error> {
    let outcome = match input.u8()? {
        0 => {
            let message = input.string()?;
            return Err(Error::Failed(message));
        }
        1 => {
            let count = input.count()?;
            let mut fresh = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                let ix = input.u64()? as usize;
                fresh.push((ix, decode_fresh(input)?));
            }
            let errors = input.u64()?;
            let count = input.count()?;
            let mut unreadable = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                unreadable.push(decode_unreadable(input)?);
            }
            Outcome::Read(FoldersRead { fresh, errors, unreadable })
        }
        2 => {
            let count = input.count()?;
            let mut failed = Vec::new();
            for _ in 0..count {
                failed.push((input.string()?, input.string()?));
            }
            Outcome::Deleted { failed }
        }
        _ => return Err(Error::Failed("the administrator helper sent something unexpected".into())),
    };
    let mut end = [0u8; 4];
    input.0.read_exact(&mut end).map_err(|_| Error::Failed("the administrator helper stopped early".into()))?;
    if &end != END {
        return Err(Error::Failed("the administrator helper stopped early".into()));
    }
    Ok(outcome)
}

/// `petal --admin <job file>`: do the job and write the reply to stdout. Returns the exit code.
pub fn helper_main(job_file: &Path) -> i32 {
    // Authorization Services sets only the effective user; become root through and through.
    unsafe {
        if libc::geteuid() == 0 && libc::getuid() != 0 {
            libc::setuid(0);
        }
    }
    let stdout = io::stdout().lock();
    let mut out = Out(BufWriter::with_capacity(1 << 20, stdout));
    let result = (|| -> io::Result<()> {
        out.0.write_all(REPLY_MAGIC)?;
        out.u64(unsafe { libc::getpid() } as u64)?;
        out.u64(unsafe { libc::getppid() } as u64)?;
        match read_job(job_file) {
            Ok(job) => do_job(&mut out, job)?,
            Err(error) => {
                out.u8(0)?;
                out.str(&format!("couldn't read the job: {error}"))?;
            }
        }
        out.0.write_all(END)?;
        out.0.flush()
    })();
    if result.is_ok() { 0 } else { 1 }
}

fn read_job(path: &Path) -> io::Result<Job> {
    let mut input = In(BufReader::new(fs::File::open(path)?));
    let mut magic = [0u8; 4];
    input.0.read_exact(&mut magic)?;
    if &magic != JOB_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a Petal job"));
    }
    decode_job(&mut input)
}

fn do_job<W: Write>(out: &mut Out<W>, job: Job) -> io::Result<()> {
    match job {
        Job::Read { root, stale, hardlinks } => {
            let asked: Vec<(usize, PathBuf, bool)> = stale.iter().map(|s| (s.ix, s.path.clone(), s.recursive)).collect();
            let mut read = scan::read_folders(&root, stale, hardlinks);
            // "Gone" means gone. A folder that's still there but couldn't be read even now
            // (macOS protects some from administrators too) stays as it was, still unreadable.
            read.fresh.retain(|(ix, fresh)| {
                let Fresh::Gone = fresh else { return true };
                let Some((_, path, whole)) = asked.iter().find(|a| a.0 == *ix) else { return true };
                if fs::symlink_metadata(path).is_err() {
                    return true;
                }
                read.errors += 1;
                read.unreadable.push(Unreadable { path: path.clone(), whole: *whole, errors: 1 });
                false
            });
            out.u8(1)?;
            out.u64(read.fresh.len() as u64)?;
            for (ix, fresh) in &read.fresh {
                out.u64(*ix as u64)?;
                encode_fresh(out, fresh)?;
            }
            out.u64(read.errors)?;
            out.u64(read.unreadable.len() as u64)?;
            for unreadable in &read.unreadable {
                encode_unreadable(out, unreadable)?;
            }
        }
        Job::DeleteSnapshots { device, names } => {
            let mut failed = Vec::new();
            for name in names {
                if let Err(why) = delete_snapshot(&device, &name) {
                    failed.push((name, why));
                }
            }
            out.u8(2)?;
            out.u64(failed.len() as u64)?;
            for (name, why) in failed {
                out.str(&name)?;
                out.str(&why)?;
            }
        }
    }
    Ok(())
}

/// Delete one Time Machine snapshot with `diskutil`, which holds the entitlement APFS
/// requires for it (root alone isn't enough).
fn delete_snapshot(device: &str, name: &str) -> Result<(), String> {
    // The job file is the user's to write, so check it asks for nothing else.
    let is_volume = device.strip_prefix("disk").is_some_and(|rest| {
        let mut parts = rest.split('s');
        let whole = parts.next().is_some_and(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
        let slices: Vec<&str> = parts.collect();
        whole && !slices.is_empty() && slices.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    });
    if !is_volume {
        return Err(format!("“{device}” isn't a volume"));
    }
    let snapshot = crate::disk::Snapshot { name: name.to_string(), created: 0 };
    if !snapshot.is_time_machine() || name.contains('/') {
        return Err("only Time Machine snapshots are deleted".into());
    }
    let output = std::process::Command::new("/usr/sbin/diskutil")
        .args(["apfs", "deleteSnapshot", device, "-name", name])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let text = if text.is_empty() { String::from_utf8_lossy(&output.stdout).trim().to_string() } else { text };
        Err(text.lines().last().unwrap_or("diskutil failed").to_string())
    }
}

// ---- Authorization Services ----

#[repr(C)]
struct AuthorizationItem {
    name: *const libc::c_char,
    value_length: usize,
    value: *mut libc::c_void,
    flags: u32,
}

#[repr(C)]
struct AuthorizationItemSet {
    count: u32,
    items: *mut AuthorizationItem,
}

type AuthorizationRef = *mut libc::c_void;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn AuthorizationCreate(rights: *const AuthorizationItemSet, environment: *const AuthorizationItemSet, flags: u32, authorization: *mut AuthorizationRef) -> i32;
    fn AuthorizationFree(authorization: AuthorizationRef, flags: u32) -> i32;
    // Deprecated, but the only way to run a tool as root without installing a privileged
    // helper, which needs a notarized app.
    fn AuthorizationExecuteWithPrivileges(
        authorization: AuthorizationRef,
        path_to_tool: *const libc::c_char,
        options: u32,
        arguments: *const *const libc::c_char,
        communications_pipe: *mut *mut libc::FILE,
    ) -> i32;
}

const FLAG_INTERACTION_ALLOWED: u32 = 1 << 0;
const FLAG_EXTEND_RIGHTS: u32 = 1 << 1;
const FLAG_DESTROY_RIGHTS: u32 = 1 << 3;
const FLAG_PREAUTHORIZE: u32 = 1 << 4;
const ERR_AUTHORIZATION_CANCELED: i32 = -60006;

struct Authorization(AuthorizationRef);

impl Authorization {
    /// Ask for an administrator's password now (the dialog shows `prompt`).
    fn new(prompt: &str) -> Result<Self, Error> {
        let mut right = AuthorizationItem { name: c"system.privilege.admin".as_ptr(), value_length: 0, value: std::ptr::null_mut(), flags: 0 };
        let rights = AuthorizationItemSet { count: 1, items: &mut right };
        let prompt = CString::new(prompt).unwrap_or_default();
        let mut prompt_item = AuthorizationItem {
            name: c"prompt".as_ptr(),
            value_length: prompt.as_bytes().len(),
            value: prompt.as_ptr() as *mut libc::c_void,
            flags: 0,
        };
        let environment = AuthorizationItemSet { count: 1, items: &mut prompt_item };
        let mut authorization: AuthorizationRef = std::ptr::null_mut();
        let flags = FLAG_INTERACTION_ALLOWED | FLAG_EXTEND_RIGHTS | FLAG_PREAUTHORIZE;
        let status = unsafe { AuthorizationCreate(&rights, &environment, flags, &mut authorization) };
        match status {
            0 => Ok(Authorization(authorization)),
            ERR_AUTHORIZATION_CANCELED => Err(Error::Cancelled),
            status => Err(Error::Failed(format!("macOS didn't grant administrator rights (error {status})"))),
        }
    }

    /// Start `tool` as root; its stdout comes back through the returned pipe.
    fn execute(&self, tool: &Path, arguments: &[&OsStr]) -> Result<fs::File, Error> {
        let tool = CString::new(tool.as_os_str().as_bytes()).map_err(|e| Error::Failed(e.to_string()))?;
        let arguments: Vec<CString> = arguments.iter().map(|a| CString::new(a.as_bytes())).collect::<Result<_, _>>().map_err(|e| Error::Failed(e.to_string()))?;
        let mut argv: Vec<*const libc::c_char> = arguments.iter().map(|a| a.as_ptr()).collect();
        argv.push(std::ptr::null());
        let mut pipe: *mut libc::FILE = std::ptr::null_mut();
        let status = unsafe { AuthorizationExecuteWithPrivileges(self.0, tool.as_ptr(), 0, argv.as_ptr(), &mut pipe) };
        match status {
            0 if !pipe.is_null() => {
                // Read through a plain file descriptor rather than stdio.
                let fd = unsafe { libc::dup(libc::fileno(pipe)) };
                unsafe { libc::fclose(pipe) };
                if fd < 0 {
                    return Err(io::Error::last_os_error().into());
                }
                Ok(unsafe { <fs::File as std::os::fd::FromRawFd>::from_raw_fd(fd) })
            }
            ERR_AUTHORIZATION_CANCELED => Err(Error::Cancelled),
            status => Err(Error::Failed(format!("couldn't start the administrator helper (error {status})"))),
        }
    }
}

impl Drop for Authorization {
    fn drop(&mut self) {
        unsafe { AuthorizationFree(self.0, FLAG_DESTROY_RIGHTS) };
    }
}

// ---- Encoding ----

struct Out<W: Write>(W);

impl<W: Write> Out<W> {
    fn u8(&mut self, value: u8) -> io::Result<()> {
        self.0.write_all(&[value])
    }
    fn u64(&mut self, value: u64) -> io::Result<()> {
        self.0.write_all(&value.to_le_bytes())
    }
    fn bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.u64(bytes.len() as u64)?;
        self.0.write_all(bytes)
    }
    fn str(&mut self, text: &str) -> io::Result<()> {
        self.bytes(text.as_bytes())
    }
    fn path(&mut self, path: &Path) -> io::Result<()> {
        self.bytes(path.as_os_str().as_bytes())
    }
}

struct In<R: Read>(R);

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

impl<R: Read> In<R> {
    fn u8(&mut self) -> io::Result<u8> {
        let mut buf = [0u8; 1];
        self.0.read_exact(&mut buf)?;
        Ok(buf[0])
    }
    fn u64(&mut self) -> io::Result<u64> {
        let mut buf = [0u8; 8];
        self.0.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }
    fn count(&mut self) -> io::Result<usize> {
        let count = self.u64()?;
        usize::try_from(count).ok().filter(|&c| c <= u32::MAX as usize).ok_or_else(|| invalid("count out of range"))
    }
    fn bytes(&mut self) -> io::Result<Vec<u8>> {
        let length = self.u64()?;
        // Names and paths; nothing near this long is real.
        if length > 1 << 20 {
            return Err(invalid("string too long"));
        }
        let mut buf = vec![0u8; length as usize];
        self.0.read_exact(&mut buf)?;
        Ok(buf)
    }
    fn string(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?).map_err(|_| invalid("not UTF-8"))
    }
    fn path(&mut self) -> io::Result<PathBuf> {
        Ok(PathBuf::from(OsStr::from_bytes(&self.bytes()?)))
    }
}

fn encode_job<W: Write>(out: &mut Out<W>, job: &Job) -> io::Result<()> {
    match job {
        Job::Read { root, stale, hardlinks } => {
            out.u8(1)?;
            out.path(root)?;
            out.u64(stale.len() as u64)?;
            for s in stale {
                out.u64(s.ix as u64)?;
                out.path(&s.path)?;
                out.u8(s.recursive as u8)?;
                out.u64(s.known_dirs.len() as u64)?;
                for name in &s.known_dirs {
                    out.str(name)?;
                }
            }
            out.u64(hardlinks.len() as u64)?;
            for &(dev, ino) in hardlinks {
                out.u64(dev)?;
                out.u64(ino)?;
            }
        }
        Job::DeleteSnapshots { device, names } => {
            out.u8(2)?;
            out.str(device)?;
            out.u64(names.len() as u64)?;
            for name in names {
                out.str(name)?;
            }
        }
    }
    Ok(())
}

fn decode_job<R: Read>(input: &mut In<R>) -> io::Result<Job> {
    match input.u8()? {
        1 => {
            let root = input.path()?;
            let count = input.count()?;
            let mut stale = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                let ix = input.u64()? as usize;
                let path = input.path()?;
                let recursive = input.u8()? != 0;
                let dirs = input.count()?;
                let mut known_dirs = HashSet::with_capacity(dirs.min(1 << 16));
                for _ in 0..dirs {
                    known_dirs.insert(input.string()?);
                }
                stale.push(Stale { ix, path, recursive, known_dirs });
            }
            let count = input.count()?;
            let mut hardlinks = HashSet::with_capacity(count.min(1 << 20));
            for _ in 0..count {
                hardlinks.insert((input.u64()?, input.u64()?));
            }
            Ok(Job::Read { root, stale, hardlinks })
        }
        2 => {
            let device = input.string()?;
            let count = input.count()?;
            let mut names = Vec::with_capacity(count.min(1 << 10));
            for _ in 0..count {
                names.push(input.string()?);
            }
            Ok(Job::DeleteSnapshots { device, names })
        }
        _ => Err(invalid("unknown job")),
    }
}

fn encode_kind(kind: Kind) -> u8 {
    match kind {
        Kind::Dir => 0,
        Kind::File => 1,
        Kind::Other => 2,
    }
}

fn decode_kind(byte: u8) -> io::Result<Kind> {
    match byte {
        0 => Ok(Kind::Dir),
        1 => Ok(Kind::File),
        2 => Ok(Kind::Other),
        _ => Err(invalid("unknown kind")),
    }
}

fn encode_raw<W: Write>(out: &mut Out<W>, raw: &Raw) -> io::Result<()> {
    out.str(&raw.name)?;
    out.u64(raw.size)?;
    out.u8(encode_kind(raw.kind))?;
    out.u64(raw.items)?;
    out.u64(raw.children.len() as u64)?;
    for child in &raw.children {
        encode_raw(out, child)?;
    }
    Ok(())
}

fn decode_raw<R: Read>(input: &mut In<R>) -> io::Result<Raw> {
    let name = input.string()?;
    let size = input.u64()?;
    let kind = decode_kind(input.u8()?)?;
    let items = input.u64()?;
    let count = input.count()?;
    let mut children = Vec::with_capacity(count.min(1 << 16));
    for _ in 0..count {
        children.push(decode_raw(input)?);
    }
    Ok(Raw { name, size, kind, items, children })
}

fn encode_fresh<W: Write>(out: &mut Out<W>, fresh: &Fresh) -> io::Result<()> {
    match fresh {
        Fresh::Gone => out.u8(0),
        Fresh::Whole(raw) => {
            out.u8(1)?;
            encode_raw(out, raw)
        }
        Fresh::Listed { own, files, known_dirs, new_dirs } => {
            out.u8(2)?;
            out.u64(*own)?;
            out.u64(files.len() as u64)?;
            for (name, size, nlink) in files {
                out.str(name)?;
                out.u64(*size)?;
                out.u64(*nlink)?;
            }
            out.u64(known_dirs.len() as u64)?;
            for name in known_dirs {
                out.str(name)?;
            }
            out.u64(new_dirs.len() as u64)?;
            for raw in new_dirs {
                encode_raw(out, raw)?;
            }
            Ok(())
        }
    }
}

fn decode_fresh<R: Read>(input: &mut In<R>) -> io::Result<Fresh> {
    match input.u8()? {
        0 => Ok(Fresh::Gone),
        1 => Ok(Fresh::Whole(decode_raw(input)?)),
        2 => {
            let own = input.u64()?;
            let count = input.count()?;
            let mut files = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                files.push((input.string()?, input.u64()?, input.u64()?));
            }
            let count = input.count()?;
            let mut known_dirs = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                known_dirs.push(input.string()?);
            }
            let count = input.count()?;
            let mut new_dirs = Vec::with_capacity(count.min(1 << 16));
            for _ in 0..count {
                new_dirs.push(decode_raw(input)?);
            }
            Ok(Fresh::Listed { own, files, known_dirs, new_dirs })
        }
        _ => Err(invalid("unknown result")),
    }
}

fn encode_unreadable<W: Write>(out: &mut Out<W>, unreadable: &Unreadable) -> io::Result<()> {
    out.path(&unreadable.path)?;
    out.u8(unreadable.whole as u8)?;
    out.u64(unreadable.errors)
}

fn decode_unreadable<R: Read>(input: &mut In<R>) -> io::Result<Unreadable> {
    Ok(Unreadable { path: input.path()?, whole: input.u8()? != 0, errors: input.u64()? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    /// A job survives the trip through its file, and a reply through the pipe.
    #[test]
    fn job_and_reply_round_trip() {
        let stale = vec![Stale { ix: 7, path: "/a/b".into(), recursive: true, known_dirs: ["x".to_string()].into_iter().collect() }];
        let job = Job::Read { root: "/a".into(), stale, hardlinks: [(1, 2), (3, 4)].into_iter().collect() };
        let file = JobFile::write(&job).unwrap();
        let Job::Read { root, stale, hardlinks } = read_job(&file.path).unwrap() else { panic!("wrong job") };
        assert_eq!(root, Path::new("/a"));
        assert_eq!((stale[0].ix, stale[0].path.as_path(), stale[0].recursive), (7, Path::new("/a/b"), true));
        assert!(stale[0].known_dirs.contains("x"));
        assert_eq!(hardlinks.len(), 2);

        let raw = Raw {
            name: "b".into(),
            size: 30,
            kind: Kind::Dir,
            items: 1,
            children: vec![Raw { name: "f".into(), size: 26, kind: Kind::File, items: 1, children: Vec::new() }],
        };
        let mut buf = Vec::new();
        let mut out = Out(&mut buf);
        out.u8(1).unwrap();
        out.u64(1).unwrap();
        out.u64(7).unwrap();
        encode_fresh(&mut out, &Fresh::Whole(raw)).unwrap();
        out.u64(2).unwrap();
        out.u64(1).unwrap();
        encode_unreadable(&mut out, &Unreadable { path: "/a/c".into(), whole: false, errors: 2 }).unwrap();
        out.0.write_all(END).unwrap();
        let Outcome::Read(read) = decode_reply(&mut In(buf.as_slice())).unwrap() else { panic!("wrong outcome") };
        assert_eq!(read.errors, 2);
        assert_eq!(read.unreadable, vec![Unreadable { path: "/a/c".into(), whole: false, errors: 2 }]);
        let Fresh::Whole(raw) = &read.fresh[0].1 else { panic!("wrong fresh") };
        assert_eq!((raw.size, raw.children[0].name.as_str()), (30, "f"));

        // A reply cut short is an error, not a partial result.
        assert!(decode_reply(&mut In(&buf[..buf.len() - 2])).is_err());
    }

    /// The helper refuses to delete anything but a Time Machine snapshot.
    #[test]
    fn deletes_only_time_machine_snapshots() {
        assert!(delete_snapshot("disk3s5", "com.apple.os.update-ABC").is_err());
        assert!(delete_snapshot("/dev/disk3s5", "com.apple.TimeMachine.2026-10-04-101010.local").is_err());
        assert!(delete_snapshot("disk3s5; rm", "com.apple.TimeMachine.2026-10-04-101010.local").is_err());
    }

    /// Folders the scan couldn't read are read by the helper and spliced into the tree, which
    /// then matches a scan that could read everything. (Run unprivileged: the folders are
    /// made unreadable for the scan, then readable again before the helper runs.)
    #[test]
    fn helper_reads_what_the_scan_could_not() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = std::env::temp_dir().join(format!("petal-admin-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir.join("open/a.bin"), 10_000);
        write(&dir.join("locked/inner/b.bin"), 50_000);
        write(&dir.join("locked/c.bin"), 7_000);
        fs::hard_link(dir.join("open/a.bin"), dir.join("locked/a-link.bin")).unwrap();
        let locked = dir.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let progress = scan::Progress::default();
        let mut tree = scan::scan(&dir, &progress);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(tree.unreadable.len(), 1);
        assert!(tree.unreadable[0].whole);
        let errors_before = tree.errors;

        let stale: Vec<Stale> = tree
            .unreadable
            .iter()
            .map(|u| {
                let ix = tree.find(&u.path).unwrap();
                Stale { ix, path: u.path.clone(), recursive: u.whole, known_dirs: HashSet::new() }
            })
            .collect();
        let job = Job::Read { root: tree.root_path.clone(), stale, hardlinks: std::mem::take(&mut tree.hardlinks) };
        let Outcome::Read(read) = run_unprivileged(&job).unwrap() else { panic!("wrong outcome") };
        assert_eq!(read.errors, 0);
        scan::apply_changes(&mut tree, read.fresh);
        tree.errors = errors_before - 1 + read.errors;

        let full = scan::scan(&dir, &scan::Progress::default());
        assert_eq!(tree.errors, 0);
        assert_eq!(tree.nodes[scan::Tree::ROOT].size, full.nodes[scan::Tree::ROOT].size);
        assert_eq!(tree.nodes[scan::Tree::ROOT].items, full.nodes[scan::Tree::ROOT].items);
        // The file hard-linked into the locked folder is counted once across both reads.
        let size = |name: &str| tree.nodes[tree.find(&dir.join(name)).unwrap()].size;
        assert_eq!(size("open/a.bin") + size("locked/a-link.bin"), size("open/a.bin").max(size("locked/a-link.bin")));
        assert!(size("open/a.bin") > 0);
        fs::remove_dir_all(&dir).unwrap();
    }
}
