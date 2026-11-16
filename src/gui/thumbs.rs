//! Thumbnails off the UI thread: common formats decoded directly, the rest (raw, HEIC, AVIF,
//! PSD…) rendered small by LightCraft first.

use super::jobs::Msg;
use eframe::egui::{ColorImage, Context};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

const DIRECT: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp"];

pub struct Queue {
    items: Mutex<(u64, VecDeque<PathBuf>)>,
    wake: Condvar,
    lightcraft: Mutex<PathBuf>,
}

impl Queue {
    pub fn start(tx: Sender<Msg>, ctx: Context, lightcraft: PathBuf) -> Arc<Queue> {
        let q = Arc::new(Queue {
            items: Mutex::new((0, VecDeque::new())),
            wake: Condvar::new(),
            lightcraft: Mutex::new(lightcraft),
        });
        for _ in 0..3 {
            let (q, tx, ctx) = (q.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || q.work(&tx, &ctx));
        }
        q
    }

    /// Replaces the pending list (a new folder).
    pub fn reset(&self, paths: Vec<PathBuf>) {
        if let Ok(mut g) = self.items.lock() {
            g.0 = g.0.wrapping_add(1);
            g.1 = paths.into();
        }
        self.wake.notify_all();
    }

    pub fn set_lightcraft(&self, cli: PathBuf) {
        if let Ok(mut g) = self.lightcraft.lock() {
            *g = cli;
        }
    }

    fn work(&self, tx: &Sender<Msg>, ctx: &Context) {
        loop {
            let next = {
                let Ok(mut g) = self.items.lock() else { return };
                loop {
                    if let Some(p) = g.1.pop_front() {
                        break p;
                    }
                    g = match self.wake.wait(g) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
            };
            let lc = self
                .lightcraft
                .lock()
                .map(|g| g.clone())
                .unwrap_or_default();
            let img = load_rgba(&next, &lc, 320);
            if tx.send(Msg::Thumb(next, img)).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}

fn ext(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Whether the window can show this file (directly or through LightCraft).
pub fn is_viewable(p: &Path) -> bool {
    crate::engine::is_image(p)
}

/// A picture of `path` no larger than `max` pixels on its long edge.
pub fn load_rgba(path: &Path, lightcraft: &Path, max: u32) -> Option<ColorImage> {
    let direct = DIRECT.contains(&ext(path).as_str());
    let img = if direct { image::open(path).ok() } else { None };
    let img = match img {
        Some(i) => i,
        None => {
            // Raw and other formats: a small render by LightCraft, cached in the temp folder.
            let dir = std::env::temp_dir().join("kimera-thumbs");
            std::fs::create_dir_all(&dir).ok()?;
            let key = format!(
                "{:x}",
                fnv(path.to_string_lossy().as_bytes()) ^ u64::from(max)
            );
            let out = dir.join(format!("{key}.jpg"));
            if !out.is_file() {
                crate::analysis::preview(lightcraft, path, &out, max, &AtomicBool::new(false))
                    .ok()?;
            }
            image::open(&out).ok()?
        }
    };
    let small = img.thumbnail(max, max).to_rgba8();
    let size = [small.width() as usize, small.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, small.as_raw()))
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}
