//! Portable, versioned reconstruction states. No Rust/GC pointers are written.
use chrono::{DateTime, FixedOffset, Local, Utc};
use ruffle_core::backend::{locale::LocaleBackend, storage::StorageBackend};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

pub const CAPACITY: usize = 16 * 1024 * 1024;
const RAW_LIMIT: usize = 128 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"RFLSTATE";
const BUILD: &str = "ruffle-libretro-replay-1";
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|x| format!("{x:02x}"))
        .collect()
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Input {
    pub mouse: [f64; 2],
    pub buttons: u8,
    pub wheel: i16,
    pub physical: Vec<u16>,
    pub pad: Vec<u16>,
    pub callbacks: Vec<crate::keyboard::Callback>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Frame {
    pub input: Option<Input>,
    pub render: bool,
    pub volume: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Initial {
    pub movie_hash: String,
    pub options: BTreeMap<String, String>,
    pub settings: serde_json::Value,
    pub epoch_micros: i64,
    pub timezone: i32,
    pub memory_storage: bool,
    pub saves: BTreeMap<String, Option<Vec<u8>>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tape {
    build: String,
    pub initial: Initial,
    pub frames: Vec<Frame>,
    pub audio_signature: Option<String>,
    #[serde(skip)]
    last: Option<Input>,
}
impl Tape {
    pub fn new(initial: Initial) -> Self {
        Self {
            build: BUILD.into(),
            initial,
            frames: vec![],
            audio_signature: None,
            last: None,
        }
    }
    pub fn record(&mut self, input: Input, render: bool, volume: f32) {
        let changed = self.last.as_ref() != Some(&input);
        self.frames.push(Frame {
            input: if changed { Some(input.clone()) } else { None },
            render,
            volume,
            quality: None,
        });
        self.last = Some(input);
    }
    pub fn resume(&mut self) {
        self.last = self.frames.iter().rev().find_map(|f| f.input.clone());
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        if self.frames.len() > 2_160_000 {
            return Err("State history exceeds ten hours".into());
        }
        let raw = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if raw.len() > RAW_LIMIT {
            return Err("State history exceeds 128 MiB".into());
        }
        let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).map_err(|e| e.to_string())?;
        let data = encoder.finish().map_err(|e| e.to_string())?;
        if data.len() + 52 > CAPACITY {
            return Err("Compressed state exceeds 16 MiB".into());
        }
        let mut out = Vec::with_capacity(data.len() + 52);
        out.extend(MAGIC);
        out.extend(1u32.to_le_bytes());
        out.extend((data.len() as u32).to_le_bytes());
        out.extend((raw.len() as u32).to_le_bytes());
        out.extend(Sha256::digest(&data));
        out.extend(data);
        Ok(out)
    }
    pub fn decode(bytes: &[u8], movie_hash: &str) -> Result<Self, String> {
        if bytes.len() < 52 || &bytes[..8] != MAGIC || bytes[8..12] != 1u32.to_le_bytes() {
            return Err("Unknown savestate format".into());
        }
        let compressed = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let length = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
        if compressed > CAPACITY - 52 || length > RAW_LIMIT || bytes.len() < 52 + compressed {
            return Err("Invalid savestate length".into());
        }
        let data = &bytes[52..52 + compressed];
        if Sha256::digest(data).as_slice() != &bytes[20..52] {
            return Err("Savestate checksum mismatch".into());
        }
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(data)
            .take((RAW_LIMIT + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() != length {
            return Err("Invalid decompressed state length".into());
        }
        let mut tape: Self = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        if tape.build != BUILD || tape.initial.movie_hash != movie_hash {
            return Err("State belongs to another core format or SWF".into());
        }
        if tape.frames.len() > 2_160_000
            || tape.frames.first().is_some_and(|f| f.input.is_none())
            || FixedOffset::east_opt(tape.initial.timezone).is_none()
            || DateTime::from_timestamp_micros(tape.initial.epoch_micros).is_none()
        {
            return Err("Invalid state metadata".into());
        }
        for key in tape.initial.saves.keys() {
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("Invalid saved storage key".into());
            }
        }
        for frame in &tape.frames {
            if frame.quality.as_ref().is_some_and(|q| {
                q.len() > 32 || q.parse::<ruffle_render::quality::StageQuality>().is_err()
            }) {
                return Err("Invalid recorded quality".into());
            }
            if !frame.volume.is_finite() || !(0. ..=4.).contains(&frame.volume) {
                return Err("Invalid audio volume".into());
            }
            if let Some(i) = &frame.input
                && (i
                    .mouse
                    .iter()
                    .any(|x| !x.is_finite() || *x < 0. || *x > 4096.)
                    || i.buttons > 7
                    || i.physical.len() > 324
                    || i.pad.len() > 324
                    || i.physical.iter().chain(&i.pad).any(|x| *x >= 324)
                    || i.callbacks.len() > 4096)
            {
                return Err("Invalid recorded input".into());
            }
        }
        tape.resume();
        Ok(tape)
    }
}

#[derive(Clone)]
pub struct Clock {
    pub elapsed: Arc<AtomicU64>,
    epoch: i64,
    offset: FixedOffset,
}
impl Clock {
    pub fn now() -> (i64, i32) {
        (
            Utc::now().timestamp_micros(),
            Local::now().offset().local_minus_utc(),
        )
    }
    pub fn new(epoch: i64, offset: i32) -> Self {
        Self {
            elapsed: Arc::new(AtomicU64::new(0)),
            epoch,
            offset: FixedOffset::east_opt(offset).unwrap(),
        }
    }
    pub fn frame(&self, frame: u64) {
        self.elapsed
            .store(frame.saturating_mul(1_000_000) / 60, Ordering::Relaxed);
    }
}
impl LocaleBackend for Clock {
    fn get_current_date_time(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(
            self.epoch
                .saturating_add(self.elapsed.load(Ordering::Relaxed) as i64),
        )
        .unwrap_or(DateTime::UNIX_EPOCH)
    }
    fn get_timezone(&self) -> FixedOffset {
        self.offset
    }
}

#[derive(Default)]
struct StorageData {
    initial: BTreeMap<String, Option<Vec<u8>>>,
    overlay: BTreeMap<String, Option<Vec<u8>>>,
    replay: bool,
    failed: bool,
}
#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
    memory: bool,
    data: Arc<Mutex<StorageData>>,
}
impl Storage {
    pub fn new(
        root: PathBuf,
        memory: bool,
        initial: Option<BTreeMap<String, Option<Vec<u8>>>>,
    ) -> Self {
        let replay = initial.is_some();
        Self {
            root,
            memory,
            data: Arc::new(Mutex::new(StorageData {
                initial: initial.unwrap_or_default(),
                replay,
                ..Default::default()
            })),
        }
    }
    pub fn initial(&self) -> Result<BTreeMap<String, Option<Vec<u8>>>, String> {
        let d = self.data.lock().unwrap();
        if d.failed {
            Err("Storage I/O failed; cannot reconstruct this session reliably".into())
        } else {
            Ok(d.initial.clone())
        }
    }
    pub fn finish_replay(&self) {
        self.data.lock().unwrap().replay = false;
    }
    fn persist(&self, key: &str, value: &Option<Vec<u8>>) -> bool {
        if self.memory {
            return true;
        }
        let Ok(path) = crate::offline::local_path(&self.root.join(format!("{key}.sol"))) else {
            return false;
        };
        match value {
            Some(bytes) => path.parent().is_some_and(|p| {
                std::fs::create_dir_all(p)
                    .and_then(|_| std::fs::write(&path, bytes))
                    .is_ok()
            }),
            None => match std::fs::remove_file(path) {
                Ok(()) => true,
                Err(e) => e.kind() == std::io::ErrorKind::NotFound,
            },
        }
    }
}
impl StorageBackend for Storage {
    fn get(&self, name: &str) -> Option<Vec<u8>> {
        let key = hash(name.as_bytes());
        let mut d = self.data.lock().unwrap();
        if let Some(value) = d.overlay.get(&key).or_else(|| d.initial.get(&key)) {
            return value.clone();
        }
        if d.replay {
            d.failed = true;
            return None;
        }
        let value = if self.memory {
            None
        } else {
            crate::offline::local_path(&self.root.join(format!("{key}.sol")))
                .ok()
                .and_then(|p| std::fs::read(p).ok())
        };
        d.initial.insert(key, value.clone());
        value
    }
    fn put(&mut self, name: &str, value: &[u8]) -> bool {
        let key = hash(name.as_bytes());
        let value = Some(value.to_vec());
        let mut d = self.data.lock().unwrap();
        if !d.replay && !self.persist(&key, &value) {
            d.failed = true;
            return false;
        }
        d.overlay.insert(key, value);
        true
    }
    fn remove_key(&mut self, name: &str) {
        let key = hash(name.as_bytes());
        let mut d = self.data.lock().unwrap();
        if !d.replay && !self.persist(&key, &None) {
            d.failed = true;
        }
        d.overlay.insert(key, None);
    }
}

pub struct Restore {
    pub tape: Tape,
    pub position: usize,
    pub input: Input,
    pub started: bool,
    pub resume_quality: Option<String>,
    pub last_progress: u8,
}
impl Restore {
    pub fn new(tape: Tape) -> Self {
        Self {
            tape,
            position: 0,
            input: Input::default(),
            started: false,
            resume_quality: None,
            last_progress: u8::MAX,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tape() -> Tape {
        Tape::new(Initial {
            movie_hash: "test".into(),
            options: BTreeMap::new(),
            settings: serde_json::Value::Null,
            epoch_micros: 1_000_000,
            timezone: -10800,
            memory_storage: true,
            saves: BTreeMap::new(),
        })
    }
    #[test]
    fn state_roundtrip_is_portable_and_rejects_corruption_or_another_game() {
        let mut t = tape();
        for _ in 0..1000 {
            t.record(Input::default(), true, 1.);
        }
        let bytes = t.encode().unwrap();
        let restored = Tape::decode(&bytes, "test").unwrap();
        assert_eq!(restored.frames.len(), 1000);
        assert!(restored.frames[1].input.is_none());
        assert!(Tape::decode(&bytes, "other").is_err());
        let mut bad = bytes;
        *bad.last_mut().unwrap() ^= 1;
        assert!(Tape::decode(&bad, "test").is_err());
    }
    #[test]
    fn storage_reconstruction_is_isolated_from_later_disk_changes() {
        let mut s = Storage::new(PathBuf::new(), true, None);
        assert_eq!(s.get("game"), None);
        s.put("game", &[1]);
        let mut r = Storage::new(PathBuf::new(), true, Some(s.initial().unwrap()));
        assert_eq!(r.get("game"), None);
        r.put("game", &[1]);
        assert_eq!(r.get("game"), Some(vec![1]));
        r.remove_key("game");
        assert_eq!(r.get("game"), None);
    }
}
