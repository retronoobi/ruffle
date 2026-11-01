//! Free GPU cache images, separate from live bitmaps and private scratch images.
use super::*;
#[derive(Debug)]
pub(super) struct Entry {
    pub id: glow::Texture,
    pub w: u32,
    pub h: u32,
    pub sampling: (u32, u32),
}
#[derive(Debug)]
pub(super) struct ImagePool {
    gl: Arc<glow::Context>,
    alive: Arc<AtomicBool>,
    entries: std::collections::VecDeque<Entry>,
    bytes: u64,
}
impl ImagePool {
    pub fn new(gl: Arc<glow::Context>, alive: Arc<AtomicBool>) -> Self {
        Self {
            gl,
            alive,
            entries: Default::default(),
            bytes: 0,
        }
    }
    pub fn take(&mut self, w: u32, h: u32) -> Option<Entry> {
        let index = self.entries.iter().position(|e| e.w == w && e.h == h)?;
        let entry = self.entries.remove(index).unwrap();
        self.bytes -= w as u64 * h as u64 * 4;
        Some(entry)
    }
    pub fn retire(&mut self, entry: Entry) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let size = entry.w as u64 * entry.h as u64 * 4;
        if size > 64 * 1024 * 1024 {
            unsafe {
                self.gl.delete_texture(entry.id);
            }
            return;
        }
        while self.entries.len() >= 256 || self.bytes + size > 64 * 1024 * 1024 {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.bytes -= old.w as u64 * old.h as u64 * 4;
            unsafe {
                self.gl.delete_texture(old.id);
            }
        }
        self.bytes += size;
        self.entries.push_back(entry);
    }
}
impl Drop for ImagePool {
    fn drop(&mut self) {
        if self.alive.load(Ordering::Relaxed) {
            for entry in self.entries.drain(..) {
                unsafe {
                    self.gl.delete_texture(entry.id);
                }
            }
        }
    }
}
