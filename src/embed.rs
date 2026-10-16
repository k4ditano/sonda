//! EmbeddingGemma 2 (csrc/embed.c over llama.cpp): words, pictures and sound
//! as 768 numbers in one space, where "a dog on the beach" lands next to the
//! photo of one.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

use anyhow::{Result, anyhow};
use image::RgbImage;

unsafe extern "C" {
    fn sonda_open(text: *const c_char, mmproj: *const c_char, gpu: c_int, threads: c_int, verbose: c_int) -> *mut c_void;
    fn sonda_close(e: *mut c_void);
    fn sonda_dim(e: *mut c_void) -> c_int;
    fn sonda_embed_texts(e: *mut c_void, texts: *const *const c_char, count: c_int, out: *mut f32) -> c_int;
    fn sonda_embed_rgb(e: *mut c_void, w: c_int, h: c_int, rgb: *const u8, out: *mut f32) -> c_int;
    fn sonda_embed_pcm(e: *mut c_void, samples: *const f32, count: usize, out: *mut f32) -> c_int;
    fn sonda_error() -> *const c_char;
    fn sonda_has_gpu() -> c_int;
}

pub const DIM: usize = 768;

/// The model card's task prompts. Media take none.
pub fn query(text: &str) -> String {
    format!("task: search result | query: {text}")
}

pub fn code_query(text: &str) -> String {
    format!("task: code retrieval | query: {text}")
}

pub fn document(title: &str, text: &str) -> String {
    let title = if title.trim().is_empty() { "none" } else { title };
    format!("title: {title} | text: {text}")
}

pub struct Embedder {
    handle: *mut c_void,
    pub media: bool,
}

// One thread at a time: the daemon keeps it behind a lock.
unsafe impl Send for Embedder {}

fn error() -> anyhow::Error {
    let text = unsafe { CStr::from_ptr(sonda_error()) }.to_string_lossy().into_owned();
    anyhow!(if text.is_empty() { "llama.cpp failed".to_owned() } else { text })
}

impl Embedder {
    /// `media`: with the picture and sound encoders (about 1 GB), or words only.
    pub fn open(media: bool, gpu: bool) -> Result<Embedder> {
        let dir = crate::paths::models();
        let text = dir.join(crate::paths::TEXT_MODEL);
        let mmproj = dir.join(crate::paths::MEDIA_MODEL);
        if !text.exists() {
            return Err(anyhow!("the model is not in {} (run `sonda setup`)", dir.display()));
        }
        if media && !mmproj.exists() {
            return Err(anyhow!("the picture and sound encoder is not in {}", dir.display()));
        }
        let t = CString::new(text.to_string_lossy().as_bytes())?;
        let m = CString::new(mmproj.to_string_lossy().as_bytes())?;
        let threads = std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4) as c_int;
        let verbose = std::env::var_os("SONDA_VERBOSE").is_some() as c_int;
        let handle = unsafe { sonda_open(t.as_ptr(), if media { m.as_ptr() } else { std::ptr::null() }, gpu as c_int, threads, verbose) };
        if handle.is_null() {
            return Err(error());
        }
        let dim = unsafe { sonda_dim(handle) } as usize;
        if dim != DIM {
            unsafe { sonda_close(handle) };
            return Err(anyhow!("the model makes {dim} numbers, not {DIM}"));
        }
        Ok(Embedder { handle, media })
    }

    pub fn has_gpu() -> bool {
        unsafe { sonda_has_gpu() != 0 }
    }

    pub fn texts(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let owned: Vec<CString> = texts.iter().map(|t| CString::new(t.replace('\0', " ")).unwrap()).collect();
        let ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        let mut out = vec![0f32; DIM * texts.len()];
        let status = unsafe { sonda_embed_texts(self.handle, ptrs.as_ptr(), texts.len() as c_int, out.as_mut_ptr()) };
        if status != 0 {
            return Err(error());
        }
        Ok(out.chunks(DIM).map(|c| c.to_vec()).collect())
    }

    pub fn text(&mut self, text: &str) -> Result<Vec<f32>> {
        Ok(self.texts(&[text.to_owned()])?.remove(0))
    }

    /// A picture, scaled to fill `budget` soft tokens (70, 140, 280, 560, 1120).
    pub fn picture(&mut self, img: &RgbImage, budget: usize) -> Result<Vec<f32>> {
        let (w, h) = img.dimensions();
        let (tw, th) = target_size(w as usize, h as usize, budget).ok_or_else(|| anyhow!("a picture too thin ({w}×{h})"))?;
        let sized;
        let img = if (tw, th) == (w as usize, h as usize) {
            img
        } else {
            sized = image::imageops::resize(img, tw as u32, th as u32, image::imageops::FilterType::CatmullRom);
            &sized
        };
        let mut out = vec![0f32; DIM];
        let status = unsafe { sonda_embed_rgb(self.handle, tw as c_int, th as c_int, img.as_raw().as_ptr(), out.as_mut_ptr()) };
        if status != 0 {
            return Err(error());
        }
        Ok(out)
    }

    /// Sound, mono at 16 kHz, 30 s at most.
    pub fn sound(&mut self, samples: &[f32]) -> Result<Vec<f32>> {
        let mut out = vec![0f32; DIM];
        let status = unsafe { sonda_embed_pcm(self.handle, samples.as_ptr(), samples.len(), out.as_mut_ptr()) };
        if status != 0 {
            return Err(error());
        }
        Ok(out)
    }
}

impl Drop for Embedder {
    fn drop(&mut self) {
        unsafe { sonda_close(self.handle) };
    }
}

/// How the reference sizes a picture for the vision encoder (transformers'
/// Gemma image processor): scaled to fill `budget` soft tokens, each 3 × 3
/// patches of 16 px, keeping its proportions, each side rounded down to a
/// multiple of 48 px.
pub fn target_size(width: usize, height: usize, budget: usize) -> Option<(usize, usize)> {
    let (patch, pooling) = (16usize, 3usize);
    let max_patches = budget * pooling * pooling;
    let factor = ((max_patches * patch * patch) as f64 / (height * width).max(1) as f64).sqrt();
    let side = pooling * patch;
    let mut th = ((factor * height as f64 / side as f64).floor() as usize) * side;
    let mut tw = ((factor * width as f64 / side as f64).floor() as usize) * side;
    if th == 0 && tw == 0 {
        return None;
    }
    let max_side = (max_patches / (pooling * pooling)) * side;
    if th == 0 {
        th = side;
        tw = (width / height.max(1) * side).min(max_side);
    } else if tw == 0 {
        tw = side;
        th = (height / width.max(1) * side).min(max_side);
    }
    Some((tw, th))
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
