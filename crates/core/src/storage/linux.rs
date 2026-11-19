use super::*;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::io::{self, ErrorKind};
use std::ops::Bound::{Excluded, Unbounded};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Component;

#[derive(Clone, Copy)]
struct Stat {
    mount: u64,
    inode: (u32, u32, u64),
    blocks: u64,
    mode: libc::mode_t,
    links: u32,
}

impl Stat {
    fn read(fd: &OwnedFd, name: &CStr) -> io::Result<Self> {
        let mask = libc::STATX_TYPE
            | libc::STATX_INO
            | libc::STATX_BLOCKS
            | libc::STATX_NLINK
            | libc::STATX_MNT_ID;
        // SAFETY: statx initializes the supplied buffer on success.
        let mut result: libc::statx = unsafe { std::mem::zeroed() };
        let status = unsafe {
            libc::statx(
                fd.as_raw_fd(),
                name.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_EMPTY_PATH | libc::AT_NO_AUTOMOUNT,
                mask,
                &mut result,
            )
        };
        if status < 0 {
            return Err(io::Error::last_os_error());
        }
        if result.stx_mask & mask != mask {
            return Err(ErrorKind::Unsupported.into());
        }
        Ok(Self {
            mount: result.stx_mnt_id,
            inode: (result.stx_dev_major, result.stx_dev_minor, result.stx_ino),
            blocks: result.stx_blocks,
            mode: libc::mode_t::from(result.stx_mode) & libc::S_IFMT,
            links: result.stx_nlink,
        })
    }
}

fn c_name(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| ErrorKind::InvalidInput.into())
}

const DIR_FLAGS: i32 = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

fn open_dir(parent: &OwnedFd, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: parent is live, name is NUL-terminated and flags require a directory.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIR_FLAGS) };
    // SAFETY: on success, openat returns a new descriptor we own.
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn open_on_mount(parent: &OwnedFd, name: &CStr) -> io::Result<OwnedFd> {
    // SAFETY: zero initializes the mode and any reserved fields. All pointers passed
    // to the syscall are valid and the kernel returns a new descriptor on success.
    let fd = unsafe {
        let mut how: libc::open_how = std::mem::zeroed();
        how.flags = DIR_FLAGS as u64;
        how.resolve = libc::RESOLVE_NO_XDEV | libc::RESOLVE_NO_SYMLINKS;
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of_val(&how),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

// Reject parent components rather than changing the meaning of a path through a symlink.
fn absolute(path: &Path) -> Option<PathBuf> {
    (path.is_absolute() && !path.components().any(|part| part == Component::ParentDir))
        .then(|| path.components().collect())
}

struct Dir {
    fd: OwnedFd,
    stat: Stat,
}

impl Dir {
    fn root(path: &Path) -> io::Result<Self> {
        // SAFETY: the path is a constant directory and success grants ownership of the fd.
        let fd = unsafe { libc::open(c"/".as_ptr(), DIR_FLAGS) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fd = unsafe { OwnedFd::from_raw_fd(fd) };
        for part in path.components() {
            if let Component::Normal(name) = part {
                fd = open_dir(&fd, &c_name(name)?)?;
            }
        }
        let stat = Stat::read(&fd, c"")?;
        Ok(Self { fd, stat })
    }

    fn child(&self, name: &OsStr) -> io::Result<Self> {
        let fd = open_on_mount(&self.fd, &c_name(name)?)?;
        let stat = Stat::read(&fd, c"")?;
        if stat.mount != self.stat.mount {
            return Err(ErrorKind::CrossesDevices.into());
        }
        Ok(Self { fd, stat })
    }

    fn at(&self, relative: &Path) -> io::Result<Self> {
        let mut current = Self {
            fd: self.fd.try_clone()?,
            stat: self.stat,
        };
        for name in relative {
            current = current.child(name)?;
        }
        Ok(current)
    }

    fn entries(&self) -> io::Result<Entries> {
        // Open an independent directory stream: dup would share the enumeration offset.
        let fd = open_dir(&self.fd, c".")?;
        // SAFETY: fdopendir takes ownership only on success.
        let stream = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        if stream.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _ = fd.into_raw_fd();
        Ok(Entries(stream))
    }

    fn stat(&self, name: &OsStr) -> io::Result<Stat> {
        let stat = Stat::read(&self.fd, &c_name(name)?)?;
        if stat.mount != self.stat.mount {
            return Err(ErrorKind::CrossesDevices.into());
        }
        Ok(stat)
    }
}

struct Entries(*mut libc::DIR);

impl Entries {
    fn next(&mut self) -> io::Result<Option<OsString>> {
        loop {
            // SAFETY: this thread owns the stream and its errno; copy the name before
            // the next call can overwrite readdir's buffer.
            let name = unsafe {
                *libc::__errno_location() = 0;
                let entry = libc::readdir(self.0);
                if entry.is_null() {
                    let error = io::Error::last_os_error();
                    return if error.raw_os_error() == Some(0) {
                        Ok(None)
                    } else {
                        Err(error)
                    };
                }
                CStr::from_ptr((*entry).d_name.as_ptr()).to_bytes()
            };
            if name != b"." && name != b".." {
                return Ok(Some(OsString::from_vec(name.to_vec())));
            }
        }
    }
}

impl Drop for Entries {
    fn drop(&mut self) {
        // SAFETY: Entries owns this live stream.
        unsafe { libc::closedir(self.0) };
    }
}

type FolderIndex = Option<usize>;

#[derive(Clone, Copy)]
enum Bucket {
    Worktree { dependency: bool },
    Scratch,
    Other,
}

impl Bucket {
    fn usage(self, bytes: u64) -> Usage {
        match self {
            Self::Worktree { dependency } => Usage {
                worktree_bytes: bytes,
                dependency_bytes: if dependency { bytes } else { 0 },
                ..Usage::default()
            },
            Self::Scratch => Usage {
                scratch_bytes: bytes,
                ..Usage::default()
            },
            Self::Other => Usage {
                other_bytes: bytes,
                ..Usage::default()
            },
        }
    }

    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Worktree { dependency: a }, Self::Worktree { dependency: b }) => {
                Self::Worktree { dependency: a && b }
            }
            (Self::Worktree { .. }, _) | (_, Self::Worktree { .. }) => {
                Self::Worktree { dependency: false }
            }
            (Self::Scratch, _) | (_, Self::Scratch) => Self::Scratch,
            _ => Self::Other,
        }
    }
}

struct Block {
    bytes: u64,
    bucket: Bucket,
    folder: FolderIndex,
}

struct Scan<F> {
    result: Inventory,
    cancelled: F,
    linked: HashMap<(u32, u32, u64), Block>,
    directories: HashSet<(u32, u32, u64)>,
}

impl<F: FnMut() -> bool> Scan<F> {
    fn check(&mut self) -> Result<(), Cancelled> {
        if (self.cancelled)() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    }

    fn add(&mut self, stat: Stat, bucket: Bucket, folder: FolderIndex) {
        let bytes = stat.blocks.saturating_mul(512);
        if stat.mode != libc::S_IFDIR && stat.links > 1 {
            self.linked
                .entry(stat.inode)
                .and_modify(|block| {
                    if block.folder != folder {
                        block.folder = None;
                    }
                    block.bucket = block.bucket.merge(bucket);
                })
                .or_insert(Block {
                    bytes,
                    bucket,
                    folder,
                });
        } else {
            self.credit(bucket.usage(bytes), folder);
        }
    }

    fn credit(&mut self, usage: Usage, folder: FolderIndex) {
        if let Some(index) = folder {
            self.result.folders[index].usage.add(usage);
        } else {
            self.result.other.add(usage);
        }
    }

    fn folder(&mut self, path: PathBuf, kind: FolderKind, claim: Claim) -> usize {
        let index = self.result.folders.len();
        self.result.folders.push(Folder {
            path,
            kind,
            owner: claim.owner,
            project_id: claim.project_id,
            coverage: Coverage::Unavailable,
            usage: Usage::default(),
        });
        index
    }

    // Iteration is bounded by the open directory depth. If resources run out, the
    // branch is incomplete and sibling branches can still contribute.
    fn walk(
        &mut self,
        root: Dir,
        root_path: &Path,
        bucket: Bucket,
        folder: FolderIndex,
        skip: &BTreeMap<PathBuf, Claim>,
    ) -> Result<Coverage, Cancelled> {
        self.check()?;
        // Separately opened roots can expose the same directory through bind mounts.
        if !self.directories.insert(root.stat.inode) {
            return Ok(Coverage::Partial);
        }
        self.add(root.stat, bucket, folder);
        let Ok(entries) = root.entries() else {
            return Ok(Coverage::Partial);
        };
        let mut stack = vec![(root, entries, root_path.to_owned(), bucket)];
        let mut coverage = Coverage::Complete;
        while let Some((dir, entries, path, bucket)) = stack.last_mut() {
            self.check()?;
            let name = match entries.next() {
                Ok(Some(name)) => name,
                result => {
                    if result.is_err() {
                        coverage = Coverage::Partial;
                    }
                    stack.pop();
                    continue;
                }
            };
            let path = path.join(&name);
            if skip.contains_key(&path) {
                continue;
            }
            let Ok(stat) = dir.stat(&name) else {
                coverage = Coverage::Partial;
                continue;
            };
            let mut bucket = *bucket;
            if stat.mode != libc::S_IFDIR {
                self.add(stat, bucket, folder);
                continue;
            }
            if matches!(bucket, Bucket::Worktree { .. })
                && (name == "target" || name == "node_modules")
            {
                bucket = Bucket::Worktree { dependency: true };
            }
            match dir
                .child(&name)
                .and_then(|child| Ok((child.entries()?, child)))
            {
                Ok((entries, child)) if child.stat.inode == stat.inode => {
                    if self.directories.insert(child.stat.inode) {
                        self.add(child.stat, bucket, folder);
                        stack.push((child, entries, path, bucket));
                    } else {
                        coverage = Coverage::Partial;
                    }
                }
                _ => coverage = Coverage::Partial,
            }
        }
        Ok(coverage)
    }

    fn finish(mut self) -> Result<Inventory, Cancelled> {
        for (_, block) in std::mem::take(&mut self.linked) {
            self.check()?;
            self.credit(block.bucket.usage(block.bytes), block.folder);
        }
        for row in &self.result.folders {
            if (self.cancelled)() {
                return Err(Cancelled);
            }
            if let Some(project) = &row.project_id {
                self.result
                    .projects
                    .entry(project.clone())
                    .or_default()
                    .add(row.usage);
                if let Owner::Thread { id, .. } = &row.owner {
                    self.result
                        .threads
                        .entry(id.clone())
                        .or_default()
                        .add(row.usage);
                }
            } else {
                self.result.other.add(row.usage);
            }
        }
        self.result.total = self.result.other;
        for usage in self.result.projects.values() {
            self.result.total.add(*usage);
        }
        self.result
            .folders
            .sort_unstable_by(|a, b| a.path.cmp(&b.path));
        self.check()?;
        Ok(self.result)
    }
}

#[derive(Clone)]
struct Claim {
    owner: Owner,
    project_id: Option<String>,
}

impl Claim {
    fn missing(project_id: Option<String>) -> Self {
        Self {
            owner: Owner::Missing,
            project_id,
        }
    }

    fn shared() -> Self {
        Self {
            owner: Owner::Shared,
            project_id: None,
        }
    }
}

type ProjectRoots<'a> = Vec<(&'a str, PathBuf)>;

fn inside(path: &Path, parent: &Path) -> bool {
    path != parent && path.starts_with(parent)
}

fn intersects(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

fn project_space(path: &Path, managed: &Path, projects: &ProjectRoots<'_>) -> bool {
    projects.iter().any(|(_, project)| {
        // A managed root explicitly placed inside a project can still be inventoried.
        !inside(managed, project) && intersects(path, project)
    })
}

fn matching_projects<'a>(container: &Path, projects: &ProjectRoots<'a>) -> BTreeSet<&'a str> {
    projects
        .iter()
        .filter(|(_, root)| root.file_name() == container.file_name())
        .map(|(id, _)| *id)
        .collect()
}

// GitVcsDriverCore.ts uses <root>/<repo basename>/<branch prefix>-<16 hex>[-N].
fn entries(
    dir: &Dir,
    coverage: &mut Coverage,
    mut visit: impl FnMut(&OsStr, &mut Coverage) -> Result<(), Cancelled>,
) -> Result<(), Cancelled> {
    let Ok(mut iter) = dir.entries() else {
        *coverage = Coverage::Partial;
        return Ok(());
    };
    loop {
        match iter.next() {
            Ok(Some(name)) => visit(&name, coverage)?,
            Ok(None) => return Ok(()),
            Err(_) => {
                *coverage = Coverage::Partial;
                return Ok(());
            }
        }
    }
}

fn directory(dir: &Dir, name: &OsStr, coverage: &mut Coverage) -> Option<Dir> {
    match dir.stat(name) {
        Ok(stat) if stat.mode != libc::S_IFDIR => None,
        Ok(stat) => match dir.child(name) {
            Ok(child) if child.stat.inode == stat.inode => Some(child),
            _ => {
                *coverage = Coverage::Partial;
                None
            }
        },
        Err(_) => {
            *coverage = Coverage::Partial;
            None
        }
    }
}

enum GitMarker {
    Linked,
    Other,
    Absent,
}

fn git_marker(dir: &Dir, coverage: &mut Coverage) -> GitMarker {
    match dir.stat(OsStr::new(".git")) {
        Ok(stat) if stat.mode == libc::S_IFREG => GitMarker::Linked,
        Ok(_) => GitMarker::Other,
        Err(err) if err.kind() == ErrorKind::NotFound => GitMarker::Absent,
        Err(_) => {
            *coverage = Coverage::Partial;
            GitMarker::Other
        }
    }
}

fn discover(
    scan: &mut Scan<impl FnMut() -> bool>,
    dir: &Dir,
    root: &Path,
    projects: &ProjectRoots<'_>,
    contains_protected_root: impl Fn(&Path) -> bool,
    candidates: &mut BTreeMap<PathBuf, Claim>,
    coverage: &mut Coverage,
) -> Result<(), Cancelled> {
    // storageCleanup.ts recognizes explicit <root>/<name> and <root>/<repository>/<worktree>.
    // A flat orphan needs a linked-worktree marker. At canonical depth, a generated
    // checkout name under a known repository also identifies a leftover without its marker.
    entries(dir, coverage, |name, coverage| {
        scan.check()?;
        let path = root.join(name);
        if candidates.contains_key(&path)
            || contains_protected_root(&path)
            || projects
                .iter()
                .any(|(_, project)| !inside(root, project) && path.starts_with(project))
        {
            return Ok(());
        }
        let Some(child) = directory(dir, name, coverage) else {
            return Ok(());
        };
        match git_marker(&child, coverage) {
            GitMarker::Linked if !project_space(&path, root, projects) => {
                candidates.insert(path, Claim::missing(None));
            }
            GitMarker::Absent => {
                let matches = matching_projects(&path, projects);
                let project = (matches.len() == 1).then(|| matches.first().unwrap().to_string());
                entries(&child, coverage, |name, coverage| {
                    scan.check()?;
                    let path = path.join(name);
                    if candidates.contains_key(&path)
                        || contains_protected_root(&path)
                        || project_space(&path, root, projects)
                        || candidates
                            .range::<Path, _>((Excluded(path.as_path()), Unbounded))
                            .next()
                            .is_some_and(|(candidate, _)| inside(candidate, &path))
                    {
                        return Ok(());
                    }
                    if let Some(child) = directory(&child, name, coverage) {
                        match git_marker(&child, coverage) {
                            GitMarker::Linked => {
                                candidates.insert(path, Claim::missing(project.clone()));
                            }
                            GitMarker::Absent if !matches.is_empty() && generated_name(name) => {
                                candidates.insert(path, Claim::missing(project.clone()));
                            }
                            _ => {}
                        }
                    }
                    Ok(())
                })?;
            }
            _ => {}
        }
        Ok(())
    })
}

fn scan_worktrees(
    scan: &mut Scan<impl FnMut() -> bool>,
    root: Option<&Path>,
    temp: Option<&Path>,
    scratch_root: Option<&Path>,
    projects: &ProjectRoots<'_>,
    threads: &[Thread<'_>],
) -> Result<BTreeMap<PathBuf, Claim>, Cancelled> {
    let home = crate::platform::home_dir().and_then(|home| absolute(Path::new(&home)));
    let protected_roots = [root, home.as_deref(), temp, Some(Path::new("/tmp"))];
    let contains_protected_root = |path: &Path| {
        protected_roots
            .iter()
            .flatten()
            .any(|root| root.starts_with(path))
    };
    let mut recorded = BTreeMap::new();
    for thread in threads {
        scan.check()?;
        if thread.cwd.as_os_str().is_empty() {
            continue;
        }
        let claim = Claim {
            owner: Owner::Thread {
                id: thread.id.into(),
                settled: thread.settled,
            },
            project_id: scan
                .result
                .projects
                .contains_key(thread.project_id)
                .then(|| thread.project_id.into()),
        };
        recorded
            .entry(absolute(thread.cwd).unwrap_or_else(|| thread.cwd.to_owned()))
            .and_modify(|existing: &mut Claim| {
                if existing.owner != claim.owner || existing.project_id != claim.project_id {
                    *existing = Claim::shared();
                }
            })
            .or_insert(claim);
    }

    let directory = match root {
        Some(root) if !projects.iter().any(|(_, project)| root == project) => {
            Dir::root(root).map_err(|err| unmeasured(&err))
        }
        _ => Err(Coverage::Unavailable),
    };
    let mut candidates = BTreeMap::new();
    let mut incomplete = false;
    // Admit only evidenced external roots to the skip set, so an invalid nested
    // reference cannot carve a hole in an otherwise measurable worktree.
    for (path, claim) in recorded {
        scan.check()?;
        let allowed = absolute(&path).is_some()
            && path.parent().is_some()
            && !contains_protected_root(&path)
            && !scratch_root.is_some_and(|root| intersects(&path, root))
            && !projects
                .iter()
                .any(|(_, project)| project.starts_with(&path));
        let coverage = if !allowed {
            Err(Coverage::Unavailable)
        } else if let Some(root) = root.filter(|root| inside(&path, root)) {
            if project_space(&path, root, projects) {
                Err(Coverage::Unavailable)
            } else {
                Ok(())
            }
        } else {
            external_worktree(&path).map(|_| ())
        };
        match coverage {
            Ok(()) => {
                candidates.insert(path, claim);
            }
            Err(coverage) => {
                incomplete |= coverage != Coverage::Missing;
                let index = scan.folder(path, FolderKind::Worktree, claim);
                scan.result.folders[index].coverage = coverage;
            }
        }
    }

    let mut coverage = if matches!(directory, Err(Coverage::Unavailable)) {
        Coverage::Unavailable
    } else {
        Coverage::Complete
    };
    if let (Some(root), Ok(dir)) = (root, &directory) {
        scan.result.worktree_root = Some(root.to_owned());
        discover(
            scan,
            dir,
            root,
            projects,
            contains_protected_root,
            &mut candidates,
            &mut coverage,
        )?;
    }
    incomplete |= coverage != Coverage::Complete;
    let mut has_results = coverage != Coverage::Unavailable;
    // Nested claims own overlapping paths. Charge the inner folder once to Other,
    // and exclude it from the outer folder's traversal.
    for (path, claim) in &candidates {
        scan.check()?;
        let claim = if path
            .ancestors()
            .skip(1)
            .any(|ancestor| candidates.contains_key(ancestor))
        {
            Claim::shared()
        } else {
            claim.clone()
        };
        let index = scan.folder(path.clone(), FolderKind::Worktree, claim);
        let dir = if let Some(relative) = root.and_then(|root| path.strip_prefix(root).ok()) {
            match &directory {
                Ok(dir) => dir.at(relative).map_err(|err| unmeasured(&err)),
                Err(coverage) => Err(*coverage),
            }
        } else {
            external_worktree(path)
        };
        let folder_coverage = match dir {
            Ok(child) => scan.walk(
                child,
                path,
                Bucket::Worktree { dependency: false },
                Some(index),
                &candidates,
            )?,
            Err(coverage) => coverage,
        };
        // ENOENT establishes that this path contributes no blocks. Its row can
        // still show Missing without making other measured sizes lower bounds.
        incomplete |= matches!(folder_coverage, Coverage::Partial | Coverage::Unavailable);
        has_results |= matches!(folder_coverage, Coverage::Complete | Coverage::Partial);
        scan.result.folders[index].coverage = folder_coverage;
    }
    scan.result.worktrees = match (has_results, incomplete) {
        (false, _) => Coverage::Unavailable,
        (true, true) => Coverage::Partial,
        (true, false) => Coverage::Complete,
    };
    Ok(candidates)
}

fn unmeasured(error: &io::Error) -> Coverage {
    if error.kind() == ErrorKind::NotFound {
        Coverage::Missing
    } else {
        Coverage::Unavailable
    }
}

fn external_worktree(path: &Path) -> Result<Dir, Coverage> {
    // Establish the mount at the exact recorded root. Below it, walk only
    // same-mount entries. The Git marker itself is inspected by statx alone.
    let dir = Dir::root(path).map_err(|err| unmeasured(&err))?;
    match dir.stat(OsStr::new(".git")) {
        Ok(stat) if stat.mode == libc::S_IFREG => Ok(dir),
        _ => Err(Coverage::Unavailable),
    }
}

struct ScratchClaim {
    cwd: PathBuf,
    claim: Claim,
}

fn scratch_claims(
    scan: &mut Scan<impl FnMut() -> bool>,
    projects: &ProjectRoots<'_>,
    threads: &[Thread<'_>],
    worktrees: &BTreeMap<PathBuf, Claim>,
) -> Result<BTreeMap<String, ScratchClaim>, Cancelled> {
    let mut claims = BTreeMap::new();
    // Scratch on the project checkout is only attributable to the project by cwd.
    for (id, path) in projects {
        scan.check()?;
        add_scratch_claim(
            &mut claims,
            path.clone(),
            Claim::missing(Some((*id).into())),
        );
    }
    for folder in &scan.result.folders {
        if (scan.cancelled)() {
            return Err(Cancelled);
        }
        if worktrees.contains_key(&folder.path)
            && (folder.coverage != Coverage::Missing
                || matches!(folder.owner, Owner::Thread { .. }))
        {
            add_scratch_claim(
                &mut claims,
                folder.path.clone(),
                Claim {
                    owner: folder.owner.clone(),
                    project_id: folder.project_id.clone(),
                },
            );
        }
    }
    let project_cwds: BTreeSet<_> = projects.iter().map(|(_, path)| path).collect();
    let mut cwd_threads = BTreeMap::new();
    for thread in threads {
        if (scan.cancelled)() {
            return Err(Cancelled);
        }
        if let Some(cwd) = absolute(thread.cwd) {
            cwd_threads
                .entry(cwd)
                .or_insert_with(BTreeMap::new)
                .insert(thread.id, thread.project_id);
        }
    }
    for (cwd, owners) in cwd_threads {
        if (scan.cancelled)() {
            return Err(Cancelled);
        }
        if worktrees.contains_key(&cwd) {
            continue;
        }
        // An unverified reference can identify a Claude namespace,
        // but cwd alone cannot distinguish a T3 session from other Claude use.
        if owners.len() > 1 {
            add_scratch_claim(&mut claims, cwd, Claim::shared());
        } else if !project_cwds.contains(&cwd) {
            let project_id = *owners.values().next().expect("at least one thread");
            let project = scan
                .result
                .projects
                .contains_key(project_id)
                .then(|| project_id.into());
            add_scratch_claim(&mut claims, cwd, Claim::missing(project));
        }
    }
    Ok(claims)
}

fn add_scratch_claim(claims: &mut BTreeMap<String, ScratchClaim>, cwd: PathBuf, claim: Claim) {
    if let Some(key) = encoded_cwd(&cwd) {
        claims
            .entry(key)
            .and_modify(|existing| {
                if existing.cwd != cwd
                    || existing.claim.owner != claim.owner
                    || existing.claim.project_id != claim.project_id
                {
                    existing.claim = Claim::shared();
                }
            })
            .or_insert(ScratchClaim { cwd, claim });
    }
}

fn uuid(name: &OsStr) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                *byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn scratch_marker(dir: &Dir, coverage: &mut Coverage) -> bool {
    ["tasks", "scratchpad"]
        .iter()
        .any(|name| match dir.stat(OsStr::new(name)) {
            Ok(stat) => stat.mode == libc::S_IFDIR,
            Err(err) if err.kind() == ErrorKind::NotFound => false,
            Err(_) => {
                *coverage = Coverage::Partial;
                false
            }
        })
}

fn scan_scratch(
    scan: &mut Scan<impl FnMut() -> bool>,
    temp: &Path,
    uid_dir: &OsStr,
    claims: &BTreeMap<String, ScratchClaim>,
) -> Result<(), Cancelled> {
    let Ok(temp_dir) = Dir::root(temp) else {
        return Ok(());
    };
    let root = match temp_dir.child(uid_dir) {
        Ok(dir) => dir,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            scan.result.claude = Coverage::Complete;
            return Ok(());
        }
        Err(_) => return Ok(()),
    };
    let path = temp.join(uid_dir);
    let skip = BTreeMap::new();
    let mut coverage = Coverage::Complete;
    if !scan.directories.insert(root.stat.inode) {
        scan.result.claude = Coverage::Partial;
        return Ok(());
    }
    scan.add(root.stat, Bucket::Other, None);
    entries(&root, &mut coverage, |name, coverage| {
        scan.check()?;
        let Some(namespace) = scratch_directory(scan, &root, name, coverage) else {
            return Ok(());
        };
        if !scan.directories.insert(namespace.stat.inode) {
            *coverage = Coverage::Partial;
            return Ok(());
        }
        let project_path = path.join(name);
        let claim = name.to_str().and_then(|encoded| claims.get(encoded));
        scan.add(namespace.stat, Bucket::Other, None);
        entries(&namespace, coverage, |session, coverage| {
            scan.check()?;
            let Some(dir) = scratch_directory(scan, &namespace, session, coverage) else {
                return Ok(());
            };
            let session_path = project_path.join(session);
            // A native UUID identifies a session under a known cwd. A directory
            // with Claude's task/scratchpad structure also supplies evidence;
            // unknown namespaces need both the native shape and marker.
            let marker = scratch_marker(&dir, coverage);
            let session_evidence = match claim {
                Some(_) => uuid(session) || marker,
                None => name.as_bytes().starts_with(b"-") && uuid(session) && marker,
            };
            let (bucket, row) = if session_evidence {
                let claim = claim.map_or_else(|| Claim::missing(None), |entry| entry.claim.clone());
                (
                    Bucket::Scratch,
                    Some(scan.folder(session_path.clone(), FolderKind::Scratch, claim)),
                )
            } else {
                (Bucket::Other, None)
            };
            let measured = scan.walk(dir, &session_path, bucket, row, &skip)?;
            if let Some(index) = row {
                scan.result.folders[index].coverage = measured;
            }
            if measured == Coverage::Partial {
                *coverage = Coverage::Partial;
            }
            Ok(())
        })
    })?;
    scan.result.claude = coverage;
    Ok(())
}

fn scratch_directory(
    scan: &mut Scan<impl FnMut() -> bool>,
    parent: &Dir,
    name: &OsStr,
    coverage: &mut Coverage,
) -> Option<Dir> {
    match parent.stat(name) {
        Ok(stat) if stat.mode != libc::S_IFDIR => {
            scan.add(stat, Bucket::Other, None);
            None
        }
        Ok(stat) => match parent.child(name) {
            Ok(dir) if dir.stat.inode == stat.inode => Some(dir),
            _ => {
                *coverage = Coverage::Partial;
                None
            }
        },
        Err(_) => {
            *coverage = Coverage::Partial;
            None
        }
    }
}

pub(super) fn scan(
    input: &InventoryInput<'_>,
    cancelled: impl FnMut() -> bool,
) -> Result<Inventory, Cancelled> {
    let mut scan = Scan {
        result: Inventory::default(),
        cancelled,
        linked: HashMap::new(),
        directories: HashSet::new(),
    };
    scan.check()?;
    let mut projects = Vec::new();
    for project in input.projects {
        scan.check()?;
        scan.result.projects.entry(project.id.into()).or_default();
        if let Some(path) = absolute(project.root) {
            projects.push((project.id, path));
        }
    }
    for thread in input.threads {
        scan.check()?;
        scan.result.threads.entry(thread.id.into()).or_default();
    }
    let root = absolute(input.worktree_root).filter(|path| path.parent().is_some());
    let temp = absolute(input.temp_dir);
    let uid_dir = format!("claude-{}", input.uid);
    let scratch_root = temp.as_ref().map(|temp| temp.join(&uid_dir));
    if let (Some(root), Some(scratch_root)) = (&root, &scratch_root) {
        // A source cannot safely act as another source's scan area.
        if intersects(root, scratch_root) {
            return scan.finish();
        }
    }
    let worktrees = scan_worktrees(
        &mut scan,
        root.as_deref(),
        temp.as_deref(),
        scratch_root.as_deref(),
        &projects,
        input.threads,
    )?;
    if let Some(temp) = temp {
        let claims = scratch_claims(&mut scan, &projects, input.threads, &worktrees)?;
        scan_scratch(&mut scan, &temp, OsStr::new(&uid_dir), &claims)?;
    }
    scan.finish()
}

#[cfg(test)]
mod tests;
