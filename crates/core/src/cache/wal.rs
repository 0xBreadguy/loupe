//! The committed WAL page map for one verified Insights read transaction.
//! Layouts below come from bundled sqlite3.c (WalIndexHdr, walHashGet, walDecodeFrame).
use super::PinnedGeneration;
use rusqlite::Connection;
use std::collections::HashSet;

#[cfg(target_os = "linux")]
pub(super) use linux::WalView;

#[cfg(not(target_os = "linux"))]
pub(super) struct WalView<'db>(
    std::marker::PhantomData<&'db Connection>,
    std::convert::Infallible,
);

#[cfg(not(target_os = "linux"))]
impl<'db> WalView<'db> {
    pub(super) fn open(_db: &'db Connection, _generation: &PinnedGeneration) -> Option<Self> {
        None
    }

    pub(super) fn stamp(&self) -> Vec<u8> {
        match self.1 {}
    }

    pub(super) fn changes_since(&self, _prior: &[u8]) -> Option<HashSet<u32>> {
        match self.1 {}
    }

    pub(super) fn page_size(&self) -> u32 {
        match self.1 {}
    }

    pub(super) fn usable_size(&self) -> u32 {
        match self.1 {}
    }

    pub(super) fn page_count(&self) -> u32 {
        match self.1 {}
    }

    pub(super) fn read_part(&self, _page: u32, _offset: usize, _out: &mut [u8]) -> Option<()> {
        match self.1 {}
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Connection, HashSet, PinnedGeneration};
    use crate::cache::{FileStamp, Key, digest, handle_matches, wal_header};
    use rusqlite::ffi;
    use std::collections::HashMap;
    use std::ffi::{CStr, c_void};
    use std::mem::MaybeUninit;
    use std::path::Path;
    use std::ptr::{NonNull, null_mut};

    const SOURCE_ID: &CStr =
        c"2025-11-28 17:28:25 281fc0e9afc38674b9b0991943b9e9d1e64c6cbdb133d35f6f5c87ff6af38a88";
    const REGION_BYTES: usize = 32768;
    const INDEX_HEADER_BYTES: usize = 136;
    const FIRST_REGION_FRAMES: usize = 4096 - INDEX_HEADER_BYTES / 4;
    const REGION_FRAMES: usize = 4096;
    const WAL_HEADER_BYTES: u64 = 32;
    const FRAME_HEADER_BYTES: u64 = 24;
    const MAX_FRAMES: u32 = 1_000_000;
    const MAX_PAGE: u32 = 0xffff_fffe;
    const WAL_FORMAT: u32 = 3_007_000;

    /// The caller keeps the same transaction pinned and revalidates its source proof
    /// before accepting or persisting cache results.
    pub(in crate::cache) struct WalView<'db> {
        db: &'db Connection,
        layout: Layout,
        usable_size: u32,
        lineage: Key,
        checksum: [u8; 8],
        salts: [u8; 8],
        main_size: u64,
        wal_size: u64,
        frame_pages: Vec<u32>,
        latest: HashMap<u32, u32>,
    }

    struct Layout {
        page_size: u32,
        page_count: u32,
        frame_count: u32,
    }

    impl Layout {
        fn frame_offset(&self, frame: u32) -> Option<u64> {
            if frame == 0 || frame > self.frame_count {
                return None;
            }
            // frame_count and page_size are both capped at construction.
            Some(
                WAL_HEADER_BYTES
                    + u64::from(frame - 1) * (FRAME_HEADER_BYTES + u64::from(self.page_size)),
            )
        }
    }

    fn word_be(bytes: &[u8]) -> u32 {
        u32::from_be_bytes(bytes.try_into().unwrap())
    }

    fn word_native(bytes: &[u8]) -> u32 {
        u32::from_ne_bytes(bytes.try_into().unwrap())
    }

    fn page_size(encoded: u16) -> u32 {
        if encoded == 1 {
            65536
        } else {
            u32::from(encoded)
        }
    }

    fn read_transaction(db: &Connection) -> bool {
        // SAFETY: db owns the connection and is borrowed for the duration of the call.
        !db.is_autocommit()
            && unsafe {
                ffi::sqlite3_txn_state(db.handle(), c"main".as_ptr()) == ffi::SQLITE_TXN_READ
            }
    }

    fn builtin_unix(db: &Connection) -> bool {
        let mut vfs = null_mut::<ffi::sqlite3_vfs>();
        // SAFETY: The source ID is a static C string. VFS_POINTER writes the supplied
        // pointer slot. The expected VFS is the bundled Unix VFS registered by SQLite.
        unsafe {
            CStr::from_ptr(ffi::sqlite3_sourceid()) == SOURCE_ID
                && ffi::sqlite3_file_control(
                    db.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_VFS_POINTER,
                    (&mut vfs as *mut *mut ffi::sqlite3_vfs).cast(),
                ) == ffi::SQLITE_OK
                && !vfs.is_null()
                && vfs == ffi::sqlite3_vfs_find(c"unix".as_ptr())
        }
    }

    struct VfsFile<'db> {
        db: &'db Connection,
        pointer: NonNull<ffi::sqlite3_file>,
        methods: &'db ffi::sqlite3_io_methods,
    }

    impl<'db> VfsFile<'db> {
        fn get(db: &'db Connection, op: libc::c_int) -> Option<Self> {
            if !read_transaction(db) {
                return None;
            }
            let mut file = null_mut::<ffi::sqlite3_file>();
            // SAFETY: Only the two pointer-returning controls are accepted. The open
            // read transaction owns this file, and the borrowed connection is !Sync.
            if !matches!(
                op,
                ffi::SQLITE_FCNTL_FILE_POINTER | ffi::SQLITE_FCNTL_JOURNAL_POINTER
            ) || unsafe {
                ffi::sqlite3_file_control(
                    db.handle(),
                    c"main".as_ptr(),
                    op,
                    (&mut file as *mut *mut ffi::sqlite3_file).cast(),
                )
            } != ffi::SQLITE_OK
            {
                return None;
            }
            let pointer = NonNull::new(file)?;
            // SAFETY: A populated sqlite3_file has a pointer to static VFS methods.
            let methods = unsafe { pointer.as_ref().pMethods.as_ref()? };
            Some(Self {
                db,
                pointer,
                methods,
            })
        }

        fn read(&self, size: u64, offset: u64, out: &mut [u8]) -> Option<()> {
            let len = libc::c_int::try_from(out.len()).ok()?;
            if len <= 0 || offset.checked_add(len as u64)? > size {
                return None;
            }
            let offset = i64::try_from(offset).ok()?;
            let read = self.methods.xRead?;
            // SAFETY: The file is owned by this connection and out is writable for len.
            // SQLite returns SQLITE_IOERR_SHORT_READ for short reads, including zero fill.
            (unsafe { read(self.pointer.as_ptr(), out.as_mut_ptr().cast(), len, offset) }
                == ffi::SQLITE_OK)
                .then_some(())
        }

        fn region(&self, index: libc::c_int) -> Option<NonNull<c_void>> {
            if self.methods.iVersion < 2 {
                return None;
            }
            let map = self.methods.xShmMap?;
            let mut region = null_mut();
            // SAFETY: This is the main handle's existing shared-memory object. With
            // extend=0, the Unix VFS maps only regions already allocated by SQLite.
            if unsafe {
                map(
                    self.pointer.as_ptr(),
                    index,
                    REGION_BYTES as i32,
                    0,
                    &mut region,
                )
            } != ffi::SQLITE_OK
            {
                return None;
            }
            NonNull::new(region)
        }

        fn filestat(&self) -> Option<serde_json::Value> {
            let control = self.methods.xFileControl?;
            // SAFETY: FILESTAT appends diagnostic JSON to an initialized sqlite3_str.
            // finish destroys the builder and transfers the UTF-8 buffer to us.
            unsafe {
                let builder = NonNull::new(ffi::sqlite3_str_new(self.db.handle()))?;
                let result = control(
                    self.pointer.as_ptr(),
                    ffi::SQLITE_FCNTL_FILESTAT,
                    builder.as_ptr().cast(),
                );
                let data = NonNull::new(ffi::sqlite3_str_finish(builder.as_ptr()))?;
                let stat = if result == ffi::SQLITE_OK {
                    serde_json::from_slice(CStr::from_ptr(data.as_ptr()).to_bytes()).ok()
                } else {
                    None
                };
                ffi::sqlite3_free(data.as_ptr().cast());
                stat
            }
        }
    }

    fn copy_region(region: NonNull<c_void>, offset: usize, out: &mut [u8]) -> Option<()> {
        if offset.checked_add(out.len())? > REGION_BYTES {
            return None;
        }
        let destination = libc::iovec {
            iov_base: out.as_mut_ptr().cast(),
            iov_len: out.len(),
        };
        let source = libc::iovec {
            iov_base: region.as_ptr().wrapping_add(offset),
            iov_len: out.len(),
        };
        // SAFETY: The kernel copies into our destination. Rust never dereferences the
        // shared memory, which may be malformed or unmapped due to a replacement.
        let n = unsafe { libc::process_vm_readv(libc::getpid(), &destination, 1, &source, 1, 0) };
        (n == out.len() as isize).then_some(())
    }

    fn frames(main: &VfsFile<'_>, layout: &Layout) -> Option<(Vec<u32>, HashMap<u32, u32>)> {
        let count = layout.frame_count as usize;
        let mut frame_pages = Vec::with_capacity(count);
        let mut latest = HashMap::new();
        let mut mapped = [0; REGION_FRAMES * 4];
        let mut again = [0; REGION_FRAMES * 4];
        let lock_page = 0x4000_0000 / layout.page_size + 1;
        for index in 0.. {
            let (offset, capacity) = if index == 0 {
                (INDEX_HEADER_BYTES, FIRST_REGION_FRAMES)
            } else {
                (0, REGION_FRAMES)
            };
            let len = capacity.min(count - frame_pages.len()) * 4;
            let region = main.region(index)?;
            copy_region(region, offset, &mut mapped[..len])?;
            copy_region(region, offset, &mut again[..len])?;
            if mapped[..len] != again[..len] {
                return None;
            }
            for word in mapped[..len].chunks_exact(4) {
                let page = word_native(word);
                // Truncating commits can leave prior entries above nPage. They
                // still belong in the delta, but are invisible to the pager.
                if page == 0 || page > MAX_PAGE || page == lock_page {
                    return None;
                }
                frame_pages.push(page);
                if page <= layout.page_count {
                    latest.insert(page, frame_pages.len() as u32);
                }
            }
            if frame_pages.len() == count {
                return Some((frame_pages, latest));
            }
        }
        unreachable!()
    }

    fn stat_fd(file: &serde_json::Value) -> Option<libc::stat> {
        let fd = libc::c_int::try_from(file.get("h")?.as_i64()?).ok()?;
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: FILESTAT identifies SQLite's borrowed, live descriptor.
        // fstat neither opens nor closes the file and initializes stat on success.
        if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
            return None;
        }
        let stat = unsafe { stat.assume_init() };
        (stat.st_mode & libc::S_IFMT == libc::S_IFREG && stat.st_nlink == 1).then_some(stat)
    }

    fn id(stat: &libc::stat) -> (u64, u64) {
        (stat.st_dev as u64, stat.st_ino as u64)
    }

    fn matches_stamp(stat: &libc::stat, stamp: &FileStamp) -> bool {
        id(stat) == stamp.id
            && u64::try_from(stat.st_size).ok() == Some(stamp.size)
            && (stat.st_mtime as i64, stat.st_mtime_nsec as i64) == stamp.modified
            && (stat.st_ctime as i64, stat.st_ctime_nsec as i64) == stamp.changed
    }

    fn valid_wal_header(raw: &[u8; 32], header: &[u8; 48], size: u32) -> bool {
        let magic = word_be(&raw[..4]);
        if header[13] > 1
            || magic != (0x377f_0682 | u32::from(header[13]))
            || word_be(&raw[4..8]) != WAL_FORMAT
            || word_be(&raw[8..12]) != size
            || raw[16..24] != header[32..40]
        {
            return false;
        }
        // walChecksumBytes treats the input as words in the byte order
        // selected by the WAL magic. The stored checksum is big endian.
        let word = |bytes: &[u8]| {
            if header[13] == 1 {
                word_be(bytes)
            } else {
                u32::from_le_bytes(bytes.try_into().unwrap())
            }
        };
        let (mut a, mut b) = (0u32, 0u32);
        for pair in raw[..24].chunks_exact(8) {
            a = a.wrapping_add(word(&pair[..4])).wrapping_add(b);
            b = b.wrapping_add(word(&pair[4..])).wrapping_add(a);
        }
        a == word_be(&raw[24..28]) && b == word_be(&raw[28..32])
    }

    impl<'db> WalView<'db> {
        pub(in crate::cache) fn open(
            db: &'db Connection,
            generation: &PinnedGeneration,
        ) -> Option<Self> {
            let header = generation.commit?;
            let files = &generation.files;
            let wal_stamp = files.wal.as_ref()?;
            let shm_id = files.shm?;
            let path = Path::new(db.path()?);
            if !db.is_readonly("main").ok()?
                || !read_transaction(db)
                || !builtin_unix(db)
                || !handle_matches(db, path)
                || wal_header(db)? != header
            {
                return None;
            }
            let layout = Layout {
                page_size: page_size(u16::from_ne_bytes(header[14..16].try_into().unwrap())),
                frame_count: word_native(&header[16..20]),
                page_count: word_native(&header[20..24]),
            };
            if !(512..=65536).contains(&layout.page_size)
                || !layout.page_size.is_power_of_two()
                || !(1..=MAX_FRAMES).contains(&layout.frame_count)
                || !(1..=MAX_PAGE).contains(&layout.page_count)
                || layout.frame_offset(layout.frame_count)?
                    + FRAME_HEADER_BYTES
                    + u64::from(layout.page_size)
                    > wal_stamp.size
            {
                return None;
            }
            let main = VfsFile::get(db, ffi::SQLITE_FCNTL_FILE_POINTER)?;
            let wal = VfsFile::get(db, ffi::SQLITE_FCNTL_JOURNAL_POINTER)?;
            let main_stat = main.filestat()?;
            let wal_stat = wal.filestat()?;
            let shm = main_stat.get("shm")?;
            let lock = shm.get("sharedMask")?.as_u64()?;
            // The Unix VFS sharedMask identifies the connection's held locks.
            // walTryBeginRead may choose read lock 0 on a fully backfilled WAL.
            // That reader ignores the WAL, and a writer can reset beneath it.
            if !(0x10..=0x80).contains(&lock)
                || !lock.is_power_of_two()
                || shm.get("exclMask")?.as_u64()? != 0
                || main_stat.get("vfs")?.as_str()? != "unix"
                || wal_stat.get("vfs")?.as_str()? != "unix"
                // The default reader uses pread. An active database mapping
                // could fault if its backing file is truncated out of band.
                || main_stat.get("mmapSize").is_some()
                || wal_stat.get("mmapSize").is_some()
                || !matches_stamp(&stat_fd(&main_stat)?, &files.main)
                || !matches_stamp(&stat_fd(&wal_stat)?, wal_stamp)
                || id(&stat_fd(shm)?) != shm_id
            {
                return None;
            }
            let mut epoch = [0; WAL_HEADER_BYTES as usize];
            wal.read(wal_stamp.size, 0, &mut epoch)?;
            if !valid_wal_header(&epoch, &header, layout.page_size) {
                return None;
            }

            let (frame_pages, latest) = frames(&main, &layout)?;
            let mut checksum = [0; 8];
            for (out, word) in checksum
                .chunks_exact_mut(4)
                .zip(header[24..32].chunks_exact(4))
            {
                out.copy_from_slice(&word_native(word).to_be_bytes());
            }
            let mut view = Self {
                db,
                layout,
                usable_size: 0,
                lineage: [0; 16],
                checksum,
                salts: epoch[16..24].try_into().unwrap(),
                main_size: files.main.size,
                wal_size: wal_stamp.size,
                frame_pages,
                latest,
            };
            let last = view.frame_header(&wal, view.layout.frame_count)?;
            if word_be(&last[4..8]) != view.layout.page_count || last[16..24] != view.checksum {
                return None;
            }
            let mut database = [0; 21];
            view.read_page_bytes(1, 0, &mut database)?;
            if database[..16] != *b"SQLite format 3\0"
                || database[18..20] != [2, 2]
                || page_size(u16::from_be_bytes(database[16..18].try_into().unwrap()))
                    != view.layout.page_size
            {
                return None;
            }
            view.usable_size = view.layout.page_size.checked_sub(u32::from(database[20]))?;
            if view.usable_size < 480 {
                return None;
            }
            let schema_version: i32 = db
                .query_row("PRAGMA main.schema_version", [], |row| row.get(0))
                .ok()?;
            let root_page: u32 = db
                .query_row(
                    "SELECT rootpage FROM main.sqlite_schema WHERE type='table'
                 AND name='orchestration_v2_projection_turn_items'",
                    [],
                    |row| row.get(0),
                )
                .ok()?;
            if !(1..=view.layout.page_count).contains(&root_page)
                || PinnedGeneration::capture(db, path)?.ne(generation)
                || !handle_matches(db, path)
            {
                return None;
            }
            view.lineage = digest(
                "WAL lineage v1",
                &(
                    &files.main,
                    wal_stamp.id,
                    shm_id,
                    schema_version,
                    root_page,
                    epoch,
                    view.usable_size,
                ),
            );
            Some(view)
        }

        pub(in crate::cache) fn stamp(&self) -> Vec<u8> {
            let mut stamp = Vec::with_capacity(29);
            stamp.push(1);
            stamp.extend_from_slice(&self.lineage);
            stamp.extend_from_slice(&self.layout.frame_count.to_be_bytes());
            stamp.extend_from_slice(&self.checksum);
            stamp
        }

        pub(in crate::cache) fn changes_since(&self, prior: &[u8]) -> Option<HashSet<u32>> {
            if prior.len() != 29 || prior[0] != 1 || prior[1..17] != self.lineage {
                return None;
            }
            let frame = word_be(&prior[17..21]);
            let wal = VfsFile::get(self.db, ffi::SQLITE_FCNTL_JOURNAL_POINTER)?;
            let old = self.frame_header(&wal, frame)?;
            // A restored or restarted WAL may have a different history. Check the
            // stored identity of the old commit, not just the salts or frame count.
            if !(1..=MAX_PAGE).contains(&word_be(&old[4..8])) || old[16..24] != prior[21..29] {
                return None;
            }
            Some(self.frame_pages[frame as usize..].iter().copied().collect())
        }

        pub(in crate::cache) fn page_size(&self) -> u32 {
            self.layout.page_size
        }

        pub(in crate::cache) fn usable_size(&self) -> u32 {
            self.usable_size
        }

        pub(in crate::cache) fn page_count(&self) -> u32 {
            self.layout.page_count
        }

        /// Read only the table leaf structure or an overflow page's next link.
        pub(in crate::cache) fn read_part(
            &self,
            page: u32,
            offset: usize,
            out: &mut [u8],
        ) -> Option<()> {
            if offset.checked_add(out.len())? > self.usable_size as usize {
                return None;
            }
            self.read_page_bytes(page, offset as u64, out)
        }

        fn frame_header(&self, wal: &VfsFile<'_>, frame: u32) -> Option<[u8; 24]> {
            let at = self.layout.frame_offset(frame)?;
            let mut raw = [0; FRAME_HEADER_BYTES as usize];
            wal.read(self.wal_size, at, &mut raw)?;
            let expected = self.frame_pages.get((frame - 1) as usize)?;
            (word_be(&raw[..4]) == *expected && raw[8..16] == self.salts).then_some(raw)
        }

        fn read_page_bytes(&self, page: u32, offset: u64, out: &mut [u8]) -> Option<()> {
            if page == 0
                || page > self.layout.page_count
                || offset.checked_add(out.len() as u64)? > u64::from(self.layout.page_size)
            {
                return None;
            }
            if let Some(&frame) = self.latest.get(&page) {
                let wal = VfsFile::get(self.db, ffi::SQLITE_FCNTL_JOURNAL_POINTER)?;
                self.frame_header(&wal, frame)?;
                let at = self.layout.frame_offset(frame)? + FRAME_HEADER_BYTES + offset;
                wal.read(self.wal_size, at, out)
            } else {
                let main = VfsFile::get(self.db, ffi::SQLITE_FCNTL_FILE_POINTER)?;
                let at = u64::from(page - 1) * u64::from(self.layout.page_size) + offset;
                main.read(self.main_size, at, out)
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::cache::Generation;
    use rusqlite::OpenFlags;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join("t3-agents-wal-XXXXXX");
            let mut path = std::ffi::CString::new(path.as_os_str().as_bytes())
                .unwrap()
                .into_bytes_with_nul();
            // SAFETY: mkdtemp mutates a writable NUL-terminated pathname.
            assert!(!unsafe { libc::mkdtemp(path.as_mut_ptr().cast()) }.is_null());
            path.pop();
            Self(PathBuf::from(std::ffi::OsStr::from_bytes(&path)))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn writer(path: &Path, page_size: u32) -> Connection {
        let writer = Connection::open(path).unwrap();
        writer.pragma_update(None, "page_size", page_size).unwrap();
        writer.execute_batch(
            "CREATE TABLE orchestration_v2_projection_turn_items (run_id TEXT, ordinal INTEGER);
             CREATE TABLE other (data BLOB);
             PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             INSERT INTO orchestration_v2_projection_turn_items VALUES ('test', 1);",
        ).unwrap();
        writer
    }

    fn stable(path: &Path) {
        for _ in 0..200 {
            if Generation::capture(path).is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("synthetic WAL metadata failed to settle");
    }

    fn pinned(path: &Path) -> Connection {
        let reader = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        reader
            .query_row("PRAGMA main.schema_version", [], |_| Ok(()))
            .unwrap();
        reader
    }

    fn view<'db>(path: &Path, reader: &'db Connection) -> WalView<'db> {
        stable(path);
        let generation = PinnedGeneration::capture(reader, path).unwrap();
        WalView::open(reader, &generation).expect("a supported synthetic WAL transaction")
    }

    #[test]
    fn wal_delta_crosses_both_index_region_layouts() {
        let dir = Scratch::new();
        let path = dir.0.join("source.db");
        let writer = writer(&path, 512);
        let stamp = {
            let reader = pinned(&path);
            let current = view(&path, &reader);
            assert_eq!(
                current.changes_since(&current.stamp()),
                Some(HashSet::new())
            );
            current.stamp()
        };

        writer
            .execute_batch(
                "WITH RECURSIVE seq(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM seq WHERE x<4300)
             INSERT INTO other SELECT zeroblob(498) FROM seq;",
            )
            .unwrap();
        let reader = pinned(&path);
        let current = view(&path, &reader);
        let changes = current.changes_since(&stamp).unwrap();
        assert!(changes.contains(&3), "the other table's root changed");
        assert!(
            !changes.contains(&2),
            "the item table's root did not change"
        );
        assert!(
            changes.len() >= 4300,
            "the delta crossed into later index regions"
        );
        assert!(
            changes.contains(&current.page_count()),
            "the last newly allocated page is in the delta"
        );

        for i in [0, 1, 17, 28] {
            let mut corrupt = stamp.clone();
            corrupt[i] ^= 0xff;
            assert!(
                current.changes_since(&corrupt).is_none(),
                "invalid field at byte {i}"
            );
        }
        assert!(current.changes_since(&stamp[..28]).is_none());
    }

    #[test]
    fn wal_view_reads_structural_bytes_in_a_64k_page() {
        let dir = Scratch::new();
        let path = dir.0.join("source.db");
        let _writer = writer(&path, 65536);
        let reader = pinned(&path);
        let current = view(&path, &reader);
        assert_eq!(current.page_size(), 65536);
        assert_eq!(current.usable_size(), 65536);
        let mut leaf_type = [0; 1];
        assert_eq!(current.read_part(2, 0, &mut leaf_type), Some(()));
        assert_eq!(leaf_type, [0x0d]);
        assert_eq!(current.read_part(3, 0, &mut leaf_type), Some(()));
        assert_eq!(leaf_type, [0x0d]);
        assert!(current.read_part(4, 0, &mut leaf_type).is_none());
        assert!(current.read_part(2, 65536, &mut leaf_type).is_none());
        assert!(current.read_part(2, usize::MAX, &mut leaf_type).is_none());
    }

    #[test]
    fn checkpoint_and_reset_break_the_prior_lineage() {
        let dir = Scratch::new();
        let path = dir.0.join("source.db");
        let writer = writer(&path, 4096);
        let initial = pinned(&path);
        let first = view(&path, &initial).stamp();
        let (log, backfill): (u32, u32) = writer
            .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |r| {
                Ok((r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert!(log > 0 && log == backfill);
        {
            let reader = pinned(&path);
            stable(&path);
            let generation = PinnedGeneration::capture(&reader, &path).unwrap();
            assert!(
                WalView::open(&reader, &generation).is_none(),
                "read mark 0 uses the main database"
            );
        }

        writer
            .execute_batch("INSERT INTO other VALUES (zeroblob(16))")
            .unwrap();
        let after_checkpoint = {
            // The first reader pins the epoch, so this commit cannot reset it.
            let reader = pinned(&path);
            let current = view(&path, &reader);
            assert!(
                current.changes_since(&first).is_none(),
                "the checkpoint changed main"
            );
            current.stamp()
        };
        drop(initial);
        writer
            .execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO other VALUES (zeroblob(16))",
            )
            .unwrap();
        let reader = pinned(&path);
        let current = view(&path, &reader);
        assert!(
            current.changes_since(&after_checkpoint).is_none(),
            "the WAL restarted"
        );
    }

    #[test]
    fn stale_open_handles_cannot_borrow_a_replacement_sidecar_identity() {
        for suffix in ["-wal", "-shm"] {
            let dir = Scratch::new();
            let path = dir.0.join("source.db");
            let _writer = writer(&path, 4096);
            let reader = pinned(&path);
            view(&path, &reader);

            let sidecar = dir.0.join(format!("source.db{suffix}"));
            let old = dir.0.join("original");
            let len = fs::metadata(&sidecar).unwrap().len();
            fs::rename(&sidecar, &old).unwrap();
            let substitute = fs::File::create(&sidecar).unwrap();
            substitute.set_len(len).unwrap();
            drop(substitute);
            stable(&path);
            let replacement = PinnedGeneration::capture(&reader, &path).unwrap();
            assert!(
                WalView::open(&reader, &replacement).is_none(),
                "{suffix} was replaced"
            );
            fs::rename(&old, &sidecar).unwrap();
        }
    }
}
