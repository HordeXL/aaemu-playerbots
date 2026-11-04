//! Minimal read-only reader for ArcheAge `game_pak` archives.
//!
//! Vendored (read path only) from the AAEmu Rust server's `aaemu-commons::aapak`
//! (GPL-3.0-or-later), itself a port of the AAEmu-Packer `AAPak` class.
//!
//! Layout (little endian): `[file data ...][file table: (files + extra) entries of 0x150 bytes,
//! each AES-128-CBC encrypted on its own with a zero IV][zero padding to 0x200][header: 0x200
//! bytes, only the first 0x20 bytes encrypted]`. File data is stored raw. The pak is tens of GB:
//! only the file table is loaded, file data is read with `seek` + `read` of the needed range.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use anyhow::{bail, ensure, Context};

/// `AAPakFileHeader.XLGamesKey`: default AES-128 key used by XLGames for the header and file table.
pub const XL_GAMES_KEY: [u8; 16] = [
    0x32, 0x1F, 0x2A, 0xEE, 0xAA, 0x58, 0x4A, 0xB4, 0x9A, 0x6C, 0x9E, 0x09, 0xD5, 0x9E, 0x9C, 0x6F,
];
const HEADER_SIZE: usize = 0x200;
const FILE_INFO_SIZE: usize = 0x150;
const NAME_SIZE: usize = 0x108;
/// File table entries decoded per read (keeps the temporary buffer at ~1.4 MB).
const FAT_CHUNK_ENTRIES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PakType {
    /// Header starts with `WIBO`; files first, extra (deleted) entries last.
    A,
    /// Header has `IDEJ` at offset 8; extra entries first, files last.
    B,
    /// Header starts with `ZERO`; same ordering as A, different entry layout.
    F,
}

/// Location of one file's data inside the pak.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PakEntry {
    pub offset: u64,
    pub size: u64,
}

/// An opened `game_pak` (file table in memory, data on disk).
pub struct Pak {
    path: PathBuf,
    file: File,
    /// Length of the pak file: no entry may reach past it.
    len: u64,
    /// Lower-case name (forward slashes) -> location.
    entries: HashMap<String, PakEntry>,
}

/// CBC-decrypts `data` in place (zero IV, no padding). `data.len()` must be a multiple of 16.
fn cbc_decrypt(cipher: &aes::Aes128, data: &mut [u8]) {
    let mut prev = [0u8; 16];
    for start in (0..data.len() / 16 * 16).step_by(16) {
        let block = &mut data[start..start + 16];
        let mut cipher_text = [0u8; 16];
        cipher_text.copy_from_slice(block);
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
        for (b, p) in block.iter_mut().zip(prev) {
            *b ^= p;
        }
        prev = cipher_text;
    }
}

fn u32_at(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}

fn i64_at(d: &[u8], o: usize) -> i64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&d[o..o + 8]);
    i64::from_le_bytes(b)
}

/// Zero-terminated name, each byte mapped to the char with the same code point.
fn read_name(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| char::from(b))
        .collect()
}

/// `PakFileInfo::from_decrypted`: (name, offset, size) of a decrypted 0x150-byte entry.
fn parse_entry(d: &[u8], ty: PakType) -> (String, i64, i64) {
    match ty {
        PakType::A => (
            read_name(&d[..NAME_SIZE]),
            i64_at(d, 0x108),
            i64_at(d, 0x110),
        ),
        PakType::B => (
            read_name(&d[0x20..0x20 + NAME_SIZE]),
            i64_at(d, 0x130),
            i64_at(d, 0x18),
        ),
        PakType::F => (
            read_name(&d[8..8 + NAME_SIZE]),
            i64_at(d, 0x110),
            i64_at(d, 0x118),
        ),
    }
}

/// Normalises a lookup name: forward slashes, lower case.
fn norm(name: &str) -> String {
    name.replace('\\', "/").to_lowercase()
}

/// A read-only source of the client's files: a `game_pak` archive or a folder of
/// extracted client files.
///
/// The bake only needs to look a file up by its client path (`game/worlds/...`), read it and
/// list the names under a prefix. A folder of extracted files serves the same purpose, so the
/// bake can run on a client that was unpacked instead of packaged (`ArcheRipper` and friends).
pub trait ClientFiles {
    /// Number of files the source knows.
    fn len(&self) -> usize;

    /// Whether the source has no files.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the source has the file.
    fn contains(&self, name: &str) -> bool;

    /// Where the file's data starts, for a source that reads sequentially (`None` for a
    /// folder, which has no table of offsets).
    fn entry(&self, name: &str) -> Option<PakEntry>;

    /// The names (lower case, forward slashes) starting with `prefix`, sorted.
    fn names_with_prefix(&self, prefix: &str) -> Vec<&str>;

    /// Reads a file's raw bytes; `None` if the source does not have it.
    fn read(&mut self, name: &str) -> anyhow::Result<Option<Vec<u8>>>;

    /// [`ClientFiles::read`] as text, honouring a UTF-8/UTF-16 byte order mark.
    fn read_text(&mut self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.read(name)?.map(|b| decode_text(&b)))
    }

    /// What the source is, for error messages.
    fn path(&self) -> &Path;
}

/// A folder of extracted client files (the layout of an unpacked `game_pak`).
///
/// The file list is built once by walking the tree; the names are the client paths, lower case
/// with forward slashes (`game/worlds/main_world/world.xml`).
pub struct DirFiles {
    root: PathBuf,
    /// Lower-case client path -> the file's path on disk.
    entries: HashMap<String, PathBuf>,
}

impl DirFiles {
    /// Walks `root` and collects every file under it.
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        anyhow::ensure!(root.is_dir(), "{}: not a directory", root.display());
        let mut entries = HashMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let read = std::fs::read_dir(&dir)
                .with_context(|| format!("read directory {}", dir.display()))?;
            for entry in read {
                let entry = entry.with_context(|| format!("read {}", dir.display()))?;
                let path = entry.path();
                let file_type = entry.file_type().context("file type")?;
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                let Ok(rel) = path.strip_prefix(root) else {
                    continue;
                };
                let name = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/")
                    .to_lowercase();
                entries.insert(name, path);
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            entries,
        })
    }
}

impl ClientFiles for DirFiles {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&norm(name))
    }

    fn entry(&self, _name: &str) -> Option<PakEntry> {
        // A folder has no table of offsets: the bake only uses the offset to sort the cells by
        // their position in the file, which is meaningless here.
        None
    }

    fn names_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let p = norm(prefix);
        let mut v: Vec<&str> = self
            .entries
            .keys()
            .filter(|k| k.starts_with(&p))
            .map(String::as_str)
            .collect();
        v.sort_unstable();
        v
    }

    fn read(&mut self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(path) = self.entries.get(&norm(name)).cloned() else {
            return Ok(None);
        };
        let bytes = std::fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        Ok(Some(bytes))
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Pak {
    /// Opens `path` with the default XLGames key, keeping every file table entry.
    pub fn open(path: &Path) -> anyhow::Result<Pak> {
        Self::open_filtered(path, |_| true)
    }

    /// Opens `path`, keeping only the entries whose lower-case name satisfies `keep` (the file
    /// table of the full game_pak has hundreds of thousands of entries).
    pub fn open_filtered(path: &Path, keep: impl Fn(&str) -> bool) -> anyhow::Result<Pak> {
        let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        let cipher = aes::Aes128::new_from_slice(&XL_GAMES_KEY).expect("16-byte key");
        let len = file.seek(SeekFrom::End(0))?;
        ensure!(
            len >= HEADER_SIZE as u64,
            "{}: too small for a pak",
            path.display()
        );

        // header: only the first 0x20 bytes are encrypted (CBC blocks are independent of later ones)
        file.seek(SeekFrom::End(-(HEADER_SIZE as i64)))?;
        let mut head = [0u8; 0x20];
        file.read_exact(&mut head)?;
        cbc_decrypt(&cipher, &mut head);
        let (ty, file_count, extra_count) = if &head[0..4] == b"WIBO" {
            (PakType::A, u32_at(&head, 8), u32_at(&head, 12))
        } else if &head[8..12] == b"IDEJ" {
            (PakType::B, u32_at(&head, 12), u32_at(&head, 0))
        } else if &head[0..4] == b"ZERO" {
            (PakType::F, u32_at(&head, 8), u32_at(&head, 12))
        } else {
            bail!(
                "{}: not a valid pak (unknown header or wrong key)",
                path.display()
            );
        };

        // file table position (LoadRawFAT)
        let total = u64::from(file_count) + u64::from(extra_count);
        let info_bytes = total * FILE_INFO_SIZE as u64;
        let mut first = len as i64 - HEADER_SIZE as i64 - info_bytes as i64;
        first -= first % 0x200;
        ensure!(
            first >= 0,
            "{}: invalid file table offset {first}",
            path.display()
        );
        file.seek(SeekFrom::Start(first as u64))?;

        // Files come first for A/F, extra entries first for B.
        let skip = match ty {
            PakType::A | PakType::F => 0u64,
            PakType::B => u64::from(extra_count),
        };
        let take = u64::from(file_count);
        let mut entries = HashMap::new();
        let mut buf = vec![0u8; FAT_CHUNK_ENTRIES * FILE_INFO_SIZE];
        let mut index = 0u64;
        while index < total {
            let n = ((total - index) as usize).min(FAT_CHUNK_ENTRIES);
            let bytes = &mut buf[..n * FILE_INFO_SIZE];
            file.read_exact(bytes)?;
            for i in 0..n {
                let raw = &mut bytes[i * FILE_INFO_SIZE..(i + 1) * FILE_INFO_SIZE];
                let global = index + i as u64;
                if global < skip || global >= skip + take {
                    continue;
                }
                cbc_decrypt(&cipher, raw);
                let (name, offset, size) = parse_entry(raw, ty);
                if name.is_empty() || offset < 0 || size < 0 {
                    continue;
                }
                let name = norm(&name);
                if keep(&name) {
                    entries.insert(
                        name,
                        PakEntry {
                            offset: offset as u64,
                            size: size as u64,
                        },
                    );
                }
            }
            index += n as u64;
        }
        Ok(Pak {
            path: path.to_path_buf(),
            file,
            len,
            entries,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Number of kept entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&norm(name))
    }

    pub fn entry(&self, name: &str) -> Option<PakEntry> {
        self.entries.get(&norm(name)).copied()
    }

    /// Names (lower case) starting with `prefix` (case-insensitive), sorted.
    pub fn names_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let p = norm(prefix);
        let mut v: Vec<&str> = self
            .entries
            .keys()
            .filter(|k| k.starts_with(&p))
            .map(String::as_str)
            .collect();
        v.sort_unstable();
        v
    }

    /// Reads a file's raw bytes (seek + read of just its range); `None` if not in the pak.
    pub fn read(&mut self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(e) = self.entry(name) else {
            return Ok(None);
        };
        // A damaged or hostile file table could name any size: check the range against the pak
        // before a buffer of that size is allocated.
        let end = e.offset.checked_add(e.size);
        ensure!(
            end.is_some_and(|end| end <= self.len),
            "{name}: its entry ({} bytes at {}) reaches past the end of {} ({} bytes)",
            e.size,
            e.offset,
            self.path.display(),
            self.len
        );
        let size = usize::try_from(e.size).context("file too large")?;
        self.file.seek(SeekFrom::Start(e.offset))?;
        let mut out = vec![0u8; size];
        self.file
            .read_exact(&mut out)
            .with_context(|| format!("read {name} ({size} bytes at {})", e.offset))?;
        Ok(Some(out))
    }

    /// [`Pak::read`] as text, honouring a UTF-8/UTF-16 byte order mark.
    pub fn read_text(&mut self, name: &str) -> anyhow::Result<Option<String>> {
        Ok(self.read(name)?.map(|b| decode_text(&b)))
    }
}

impl ClientFiles for Pak {
    fn len(&self) -> usize {
        self.entries.len()
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn contains(&self, name: &str) -> bool {
        self.entries.contains_key(&norm(name))
    }

    fn entry(&self, name: &str) -> Option<PakEntry> {
        self.entries.get(&norm(name)).copied()
    }

    fn names_with_prefix(&self, prefix: &str) -> Vec<&str> {
        let p = norm(prefix);
        let mut v: Vec<&str> = self
            .entries
            .keys()
            .filter(|k| k.starts_with(&p))
            .map(String::as_str)
            .collect();
        v.sort_unstable();
        v
    }

    fn read(&mut self, name: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Pak::read(self, name)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

/// Decodes client text files (UTF-8, optionally with a BOM; UTF-16 LE/BE with a BOM).
pub fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&b[3..]).into_owned();
    }
    if b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> = (0..(b.len() - 2) / 2)
            .map(|i| {
                let c = [b[2 + 2 * i], b[3 + 2 * i]];
                if le {
                    u16::from_le_bytes(c)
                } else {
                    u16::from_be_bytes(c)
                }
            })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    String::from_utf8_lossy(b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncrypt;

    fn cbc_encrypt(cipher: &aes::Aes128, data: &mut [u8]) {
        let mut prev = [0u8; 16];
        for start in (0..data.len() / 16 * 16).step_by(16) {
            let block = &mut data[start..start + 16];
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            cipher.encrypt_block(GenericArray::from_mut_slice(block));
            prev.copy_from_slice(block);
        }
    }

    /// Builds a TypeA pak holding `files` (name, data) in memory.
    fn build_pak(files: &[(&str, &[u8])]) -> Vec<u8> {
        let cipher = aes::Aes128::new_from_slice(&XL_GAMES_KEY).unwrap();
        let mut out = Vec::new();
        let mut infos = Vec::new();
        for (name, data) in files {
            let offset = out.len() as i64;
            out.extend_from_slice(data);
            let mut e = vec![0u8; FILE_INFO_SIZE];
            e[..name.len()].copy_from_slice(name.as_bytes());
            e[0x108..0x110].copy_from_slice(&offset.to_le_bytes());
            e[0x110..0x118].copy_from_slice(&(data.len() as i64).to_le_bytes());
            e[0x118..0x120].copy_from_slice(&(data.len() as i64).to_le_bytes());
            cbc_encrypt(&cipher, &mut e);
            infos.push(e);
        }
        // pad file data so the table starts on a 0x200 boundary
        while out.len() % 0x200 != 0 {
            out.push(0);
        }
        for e in &infos {
            out.extend_from_slice(e);
        }
        while out.len() % 0x200 != 0 {
            out.push(0);
        }
        let mut head = [0u8; 0x20];
        head[0..4].copy_from_slice(b"WIBO");
        head[8..12].copy_from_slice(&(files.len() as u32).to_le_bytes());
        cbc_encrypt(&cipher, &mut head);
        let mut block = vec![0u8; HEADER_SIZE];
        block[..0x20].copy_from_slice(&head);
        out.extend_from_slice(&block);
        out
    }

    #[test]
    fn reads_files_from_a_synthetic_pak() {
        let dir = crate::test_dir("pak");
        let path = dir.join("synthetic.pak");
        let big = vec![7u8; 1500];
        std::fs::write(
            &path,
            build_pak(&[
                ("game/a.txt", b"hello"),
                ("Game/Sub/B.xml", &big),
                ("game/c.bin", b""),
            ]),
        )
        .unwrap();
        let mut pak = Pak::open(&path).unwrap();
        assert_eq!(pak.len(), 3);
        assert_eq!(pak.read("game/a.txt").unwrap().unwrap(), b"hello");
        assert_eq!(pak.read("GAME\\SUB\\b.xml").unwrap().unwrap(), big);
        assert_eq!(pak.read("game/c.bin").unwrap().unwrap(), Vec::<u8>::new());
        assert!(pak.read("game/missing").unwrap().is_none());
        assert_eq!(pak.names_with_prefix("game/sub"), vec!["game/sub/b.xml"]);
        let filtered = Pak::open_filtered(&path, |n| n.starts_with("game/sub")).unwrap();
        assert_eq!(filtered.len(), 1);
        drop(pak);
        drop(filtered);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_entry_that_reaches_past_the_pak_is_an_error_not_an_allocation() {
        let dir = crate::test_dir("pak-range");
        let path = dir.join("ranges.pak");
        std::fs::write(
            &path,
            build_pak(&[("game/a.txt", b"hello"), ("game/b.txt", b"xy")]),
        )
        .unwrap();
        let mut pak = Pak::open(&path).unwrap();
        assert_eq!(pak.read("game/a.txt").unwrap().unwrap(), b"hello");
        let len = pak.len;
        // sizes that would be terabytes (or wrap around) if they were trusted
        for (offset, size) in [
            (0, len + 1),
            (0, u64::MAX),
            (0, 1 << 40),
            (len - 1, 2),
            (len, 1),
            (u64::MAX, 1),
            (u64::MAX - 2, 8),
            (1 << 62, 1 << 62),
        ] {
            *pak.entries.get_mut("game/a.txt").unwrap() = PakEntry { offset, size };
            let err = pak.read("game/a.txt").expect_err("must be refused");
            assert!(format!("{err:#}").contains("past the end"), "{err:#}");
        }
        // an entry that ends exactly at the end of the file is fine, so is an empty one at the end
        *pak.entries.get_mut("game/a.txt").unwrap() = PakEntry {
            offset: len - 4,
            size: 4,
        };
        assert_eq!(pak.read("game/a.txt").unwrap().unwrap().len(), 4);
        *pak.entries.get_mut("game/a.txt").unwrap() = PakEntry {
            offset: len,
            size: 0,
        };
        assert!(pak.read("game/a.txt").unwrap().unwrap().is_empty());
        drop(pak);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_non_pak() {
        let dir = crate::test_dir("pak-bad");
        let path = dir.join("not-a-pak.bin");
        std::fs::write(&path, vec![1u8; 4096]).unwrap();
        assert!(Pak::open(&path).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn decodes_text_boms() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFabc"), "abc");
        assert_eq!(decode_text(b"\xFF\xFEa\0b\0"), "ab");
        assert_eq!(decode_text(b"plain"), "plain");
    }

    /// A folder of extracted client files serves the same purpose as an archive.
    #[test]
    fn a_folder_of_extracted_files_is_a_client_source() {
        let dir = crate::test_dir("dir-source");
        std::fs::create_dir_all(dir.join("game/worlds/main_world")).unwrap();
        std::fs::write(dir.join("game/worlds/main_world/world.xml"), b"<world/>").unwrap();
        std::fs::create_dir_all(dir.join("game/ui")).unwrap();
        std::fs::write(dir.join("game/ui/map.png"), b"png").unwrap();

        let mut files = DirFiles::open(&dir).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains("game/worlds/main_world/world.xml"));
        assert!(!files.is_empty());
        assert_eq!(
            files
                .read("game/worlds/main_world/world.xml")
                .unwrap()
                .unwrap(),
            b"<world/>"
        );
        // A missing file is None, not an error.
        assert!(files.read("game/nope").unwrap().is_none());
        assert_eq!(
            files.names_with_prefix("game/worlds/"),
            vec!["game/worlds/main_world/world.xml"]
        );
        // The names are lower case, like the archive's.
        assert!(files.contains("GAME/WORLDS/MAIN_WORLD/WORLD.XML"));
        assert_eq!(
            files
                .read_text("game/worlds/main_world/world.xml")
                .unwrap()
                .unwrap(),
            "<world/>"
        );
    }

    /// Both sources answer the same questions the bake asks.
    #[test]
    fn a_folder_and_an_archive_answer_the_same() {
        let dir = crate::test_dir("dir-vs-pak");
        std::fs::create_dir_all(dir.join("game/sub")).unwrap();
        std::fs::write(dir.join("game/sub/b.xml"), b"big").unwrap();
        std::fs::write(dir.join("game/a.txt"), b"hello").unwrap();

        let mut folder = DirFiles::open(&dir).unwrap();
        let mut archive = Pak::open(&build_pak_file(&dir, &[("game/a.txt", b"hello")])).unwrap();

        assert_eq!(
            folder.read("game/a.txt").unwrap().unwrap(),
            archive.read("game/a.txt").unwrap().unwrap()
        );
        assert!(folder.contains("game/a.txt"));
        assert!(archive.contains("game/a.txt"));
    }

    /// Writes a TypeA pak with the given files and returns its path.
    fn build_pak_file(dir: &std::path::Path, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let bytes = build_pak(files);
        let path = dir.join("game.aacm.pak");
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
