//! What differs between the desktop app and the web app: opening and saving files,
//! and where the dictionary and the Japanese font come from.

use std::sync::{Arc, Mutex};

use jpfe::dictionary::{DEFINITIONS_FILE, KANJI_FILE};
use jpfe::tokenizer::TOKENIZER_FILES;

use crate::storage::Key;

/// Files of the dictionary the web app downloads: the tokenizer's, then the reader's definitions and kanji.
pub const WEB_DICTIONARY_FILES: [&str; TOKENIZER_FILES.len() + 2] = {
    let mut files = [""; TOKENIZER_FILES.len() + 2];
    let mut i = 0;
    while i < TOKENIZER_FILES.len() {
        files[i] = TOKENIZER_FILES[i];
        i += 1;
    }
    files[i] = DEFINITIONS_FILE;
    files[i + 1] = KANJI_FILE;
    files
};

/// Results of dialogs, downloads and storage, which arrive outside of the UI code.
pub enum Incoming {
    File { name: String, bytes: Vec<u8> },
    /// Version of the dictionary on the server, `None` if it could not be fetched (web only)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    DictionaryVersion(Option<String>),
    /// A downloaded, zstd-compressed dictionary file (web only)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    DictionaryFile { name: String, bytes: Result<Vec<u8>, String> },
    /// Contents read from storage, `None` if missing
    Stored { key: Key, bytes: Option<Vec<u8>> },
    /// A storage write finished
    Written(Key),
    StorageError(String),
    /// A font to add; the primary one draws Latin text too, the others only fill in missing glyphs
    Font { name: String, bytes: Vec<u8>, primary: bool },
    Error(String),
}

#[derive(Clone, Default)]
pub struct Inbox(Arc<Mutex<Vec<Incoming>>>);

impl Inbox {
    pub fn push(&self, message: Incoming) {
        self.0.lock().unwrap().push(message);
    }

    pub fn drain(&self) -> Vec<Incoming> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// A message for whoever investigates (the browser console on the web).
pub fn log(message: &str) {
    #[cfg(target_arch = "wasm32")]
    web_sys::console::log_1(&message.into());
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{message}");
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
pub use web::*;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::{Inbox, Incoming};
    use anyhow::Result;
    use jpfe::tokenizer::JapaneseTokenizer;

    /// Japanese system fonts, tried in order.
    const SYSTEM_FONTS: [&str; 8] = [
        "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        "C:\\Windows\\Fonts\\YuGothM.ttc",
        "C:\\Windows\\Fonts\\meiryo.ttc",
        "C:\\Windows\\Fonts\\msgothic.ttc",
    ];

    pub fn embedded_tokenizer() -> Option<Result<JapaneseTokenizer>> {
        Some(JapaneseTokenizer::new(None, true, false))
    }

    /// The definitions and kanji built into the app.
    pub fn embedded_lookup() -> Option<crate::reader::Lookup> {
        Some(crate::reader::Lookup::embedded())
    }

    /// San Francisco, the macOS system font.
    const SYSTEM_UI_FONT: &str = "/System/Library/Fonts/SFNS.ttf";

    pub fn load_assets(inbox: &Inbox, _ctx: &egui::Context) {
        if let Ok(bytes) = std::fs::read(SYSTEM_UI_FONT) {
            inbox.push(Incoming::Font { name: "system-ui".into(), bytes, primary: true });
        }
        match SYSTEM_FONTS.iter().find_map(|p| std::fs::read(p).ok()) {
            Some(bytes) => inbox.push(Incoming::Font { name: "japanese".into(), bytes, primary: false }),
            None => inbox.push(Incoming::Error("No Japanese system font found; titles may not display".into())),
        }
    }

    pub fn pick_files(inbox: &Inbox, _ctx: &egui::Context) {
        let Some(paths) = rfd::FileDialog::new().add_filter("EPUB or PDF", &["epub", "pdf"]).pick_files() else {
            return;
        };
        for path in paths {
            read_path(inbox, &path);
        }
    }

    pub fn read_path(inbox: &Inbox, path: &std::path::Path) {
        let name = path.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        match std::fs::read(path) {
            Ok(bytes) => inbox.push(Incoming::File { name, bytes }),
            Err(e) => inbox.push(Incoming::Error(format!("{name}: {e}"))),
        }
    }

    pub fn read_dropped(inbox: &Inbox, file: egui::DroppedFileHandle, _ctx: &egui::Context) {
        read_path(inbox, file.path());
    }

    pub fn save_file(file_name: &str, bytes: Vec<u8>) -> Result<()> {
        if let Some(path) = rfd::FileDialog::new().set_file_name(file_name).add_filter("Zip", &["zip"]).save_file() {
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::{Inbox, Incoming, WEB_DICTIONARY_FILES};
    use anyhow::{anyhow, Result};
    use jpfe::tokenizer::JapaneseTokenizer;
    use std::io::Read;
    use wasm_bindgen::JsCast;

    /// Served next to the page (see `build-web-assets.sh`).
    const FONT_URL: &str = "assets/NotoSansJP-Regular.otf";

    pub fn embedded_tokenizer() -> Option<Result<JapaneseTokenizer>> {
        None
    }

    /// Downloaded with the dictionary instead.
    pub fn embedded_lookup() -> Option<crate::reader::Lookup> {
        None
    }

    /// Download the font, and ask the server which dictionary version it has.
    pub fn load_assets(inbox: &Inbox, ctx: &egui::Context) {
        {
            let (inbox, ctx) = (inbox.clone(), ctx.clone());
            // The query avoids a stale HTTP-cached answer
            let url = format!("assets/dict/version.txt?t={}", js_sys::Date::now() as u64);
            ehttp::fetch(ehttp::Request::get(url), move |result| {
                let version = result.ok().filter(|r| r.ok).and_then(|r| r.text().map(|t| t.trim().to_string()));
                inbox.push(Incoming::DictionaryVersion(version.filter(|v| !v.is_empty())));
                ctx.request_repaint();
            });
        }
        let (inbox, ctx) = (inbox.clone(), ctx.clone());
        ehttp::fetch(ehttp::Request::get(FONT_URL), move |result| {
            match result {
                Ok(r) if r.ok => inbox.push(Incoming::Font { name: "noto-sans-jp".into(), bytes: r.bytes, primary: true }),
                _ => inbox.push(Incoming::Error("Could not download the Japanese font".into())),
            }
            ctx.request_repaint();
        });
    }

    /// Download the compressed dictionary files of `version`.
    pub fn download_dictionary(inbox: &Inbox, ctx: &egui::Context, version: &str) {
        for name in WEB_DICTIONARY_FILES {
            let (inbox, ctx) = (inbox.clone(), ctx.clone());
            // The version in the URL avoids getting older files from the HTTP cache
            ehttp::fetch(ehttp::Request::get(format!("assets/dict/{name}.zst?v={version}")), move |result| {
                let bytes = match result {
                    Ok(r) if r.ok => Ok(r.bytes),
                    Ok(r) => Err(format!("HTTP {} {}", r.status, r.status_text)),
                    Err(e) => Err(e),
                };
                inbox.push(Incoming::DictionaryFile { name: name.to_string(), bytes });
                ctx.request_repaint();
            });
        }
    }

    pub fn decompress(bytes: &[u8]) -> Result<Vec<u8>> {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(bytes).map_err(|e| anyhow!("{e}"))?;
        // The frame header gives the size: allocate it once instead of growing (and copying) the buffer
        let size = decoder.decoder.content_size().min(1 << 30) as usize;
        let mut out = Vec::with_capacity(size);
        decoder.read_to_end(&mut out)?;
        Ok(out)
    }

    pub fn pick_files(inbox: &Inbox, ctx: &egui::Context) {
        let (inbox, ctx) = (inbox.clone(), ctx.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let dialog = rfd::AsyncFileDialog::new().add_filter("EPUB or PDF", &["epub", "pdf"]);
            for file in dialog.pick_files().await.unwrap_or_default() {
                inbox.push(Incoming::File { name: file.file_name(), bytes: file.read().await });
            }
            ctx.request_repaint();
        });
    }

    pub fn read_dropped(inbox: &Inbox, file: egui::DroppedFileHandle, ctx: &egui::Context) {
        let (inbox, ctx) = (inbox.clone(), ctx.clone());
        wasm_bindgen_futures::spawn_local(async move {
            let name = file.path().to_string_lossy().into_owned();
            match file.bytes_async().await {
                Ok(bytes) => inbox.push(Incoming::File { name, bytes }),
                Err(e) => inbox.push(Incoming::Error(format!("{name}: {e}"))),
            }
            ctx.request_repaint();
        });
    }

    /// Hand the file to the browser as a download.
    pub fn save_file(file_name: &str, bytes: Vec<u8>) -> Result<()> {
        let fail = |e: wasm_bindgen::JsValue| anyhow!("{e:?}");
        let parts = js_sys::Array::of1(&js_sys::Uint8Array::from(bytes.as_slice()));
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("application/zip");
        let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(fail)?;
        let url = web_sys::Url::create_object_url_with_blob(&blob).map_err(fail)?;
        let document = web_sys::window().and_then(|w| w.document()).ok_or_else(|| anyhow!("no document"))?;
        let link: web_sys::HtmlAnchorElement = document.create_element("a").map_err(fail)?.unchecked_into();
        link.set_href(&url);
        link.set_download(file_name);
        link.click();
        web_sys::Url::revoke_object_url(&url).map_err(fail)?;
        Ok(())
    }
}
