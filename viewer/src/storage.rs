//! A small persistent file store: the browser's origin private file system (OPFS) on the web,
//! and a folder in the user's data directory on the desktop. Reads and writes report back
//! through the [`Inbox`], since they are asynchronous on the web.

use crate::platform::{Inbox, Incoming};

/// What is stored, and where.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// Book list, reading progress and settings (JSON)
    State,
    /// Version of the unpacked dictionary files (web only)
    DictionaryVersion,
    /// One unpacked dictionary file (web only)
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    DictionaryFile(String),
    /// A book's original EPUB or PDF
    Source(u64),
    /// A book's tokenized chapters and word counts
    Analysis(u64),
}

impl Key {
    fn path(&self) -> String {
        match self {
            Key::State => "state.json".into(),
            Key::DictionaryVersion => "dict/version.txt".into(),
            Key::DictionaryFile(name) => format!("dict/{name}"),
            Key::Source(id) => format!("books/{id}/source"),
            Key::Analysis(id) => format!("books/{id}/analysis.bin"),
        }
    }
}

/// Read `key`; answers with `Incoming::Stored`, whose bytes are `None` if it is missing.
pub fn read(inbox: &Inbox, ctx: &egui::Context, key: Key) {
    imp::read(inbox.clone(), ctx.clone(), key);
}

/// Write `key`; answers with `Incoming::Written` once it is saved, or `Incoming::StorageError`.
pub fn write(inbox: &Inbox, ctx: &egui::Context, key: Key, bytes: Vec<u8>) {
    imp::write(inbox.clone(), ctx.clone(), key, bytes);
}

/// Forget everything stored for a book.
pub fn remove_book(inbox: &Inbox, ctx: &egui::Context, id: u64) {
    imp::remove(inbox.clone(), ctx.clone(), format!("books/{id}"));
}

fn done(inbox: &Inbox, ctx: &egui::Context, message: Incoming) {
    inbox.push(message);
    ctx.request_repaint();
}

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use super::{done, Key};
    use crate::platform::{Inbox, Incoming};
    use std::path::PathBuf;

    fn root() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("jpfe-viewer"))
    }

    pub fn read(inbox: Inbox, ctx: egui::Context, key: Key) {
        let bytes = root().and_then(|r| std::fs::read(r.join(key.path())).ok());
        done(&inbox, &ctx, Incoming::Stored { key, bytes });
    }

    pub fn write(inbox: Inbox, ctx: egui::Context, key: Key, bytes: Vec<u8>) {
        let result = root().ok_or_else(|| "no data directory".to_string()).and_then(|r| {
            let path = r.join(key.path());
            std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(&path, bytes).map_err(|e| e.to_string())
        });
        let message = match result {
            Ok(()) => Incoming::Written(key),
            Err(e) => Incoming::StorageError(e),
        };
        done(&inbox, &ctx, message);
    }

    pub fn remove(inbox: Inbox, ctx: egui::Context, path: String) {
        if let Some(dir) = root().map(|r| r.join(path)) {
            if let Err(e) = std::fs::remove_dir_all(dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    done(&inbox, &ctx, Incoming::StorageError(e.to_string()));
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use super::{done, Key};
    use crate::platform::{Inbox, Incoming};
    use wasm_bindgen::{JsCast, JsValue};
    use wasm_bindgen_futures::{spawn_local, JsFuture};
    use web_sys::{
        FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions,
        FileSystemRemoveOptions, FileSystemWritableFileStream,
    };

    /// The directory holding `path`'s last component, and that component.
    async fn parent(path: &str, create: bool) -> Result<(FileSystemDirectoryHandle, String), JsValue> {
        let storage = web_sys::window().ok_or("no window")?.navigator().storage();
        let mut dir: FileSystemDirectoryHandle = JsFuture::from(storage.get_directory()).await?.unchecked_into();
        let mut parts: Vec<&str> = path.split('/').collect();
        let name = parts.pop().unwrap_or_default().to_string();
        for part in parts {
            let options = FileSystemGetDirectoryOptions::new();
            options.set_create(create);
            dir = JsFuture::from(dir.get_directory_handle_with_options(part, &options)).await?.unchecked_into();
        }
        Ok((dir, name))
    }

    async fn read_file(path: &str) -> Result<Vec<u8>, JsValue> {
        let (dir, name) = parent(path, false).await?;
        let handle: FileSystemFileHandle = JsFuture::from(dir.get_file_handle(&name)).await?.unchecked_into();
        let file: web_sys::File = JsFuture::from(handle.get_file()).await?.unchecked_into();
        let buffer = JsFuture::from(file.array_buffer()).await?;
        Ok(js_sys::Uint8Array::new(&buffer).to_vec())
    }

    async fn write_file(path: &str, bytes: &[u8]) -> Result<(), JsValue> {
        let (dir, name) = parent(path, true).await?;
        let options = FileSystemGetFileOptions::new();
        options.set_create(true);
        let handle: FileSystemFileHandle =
            JsFuture::from(dir.get_file_handle_with_options(&name, &options)).await?.unchecked_into();
        let stream: FileSystemWritableFileStream = JsFuture::from(handle.create_writable()).await?.unchecked_into();
        JsFuture::from(stream.write_with_u8_array(bytes)?).await?;
        // The file is replaced only when the stream is closed, so a half-written file is never seen
        JsFuture::from(stream.close()).await?;
        Ok(())
    }

    async fn remove_entry(path: &str) -> Result<(), JsValue> {
        let (dir, name) = parent(path, false).await?;
        let options = FileSystemRemoveOptions::new();
        options.set_recursive(true);
        JsFuture::from(dir.remove_entry_with_options(&name, &options)).await?;
        Ok(())
    }

    pub fn read(inbox: Inbox, ctx: egui::Context, key: Key) {
        spawn_local(async move {
            let bytes = read_file(&key.path()).await.ok();
            done(&inbox, &ctx, Incoming::Stored { key, bytes });
        });
    }

    pub fn write(inbox: Inbox, ctx: egui::Context, key: Key, bytes: Vec<u8>) {
        spawn_local(async move {
            let message = match write_file(&key.path(), &bytes).await {
                Ok(()) => Incoming::Written(key),
                Err(e) => Incoming::StorageError(format!("{e:?}")),
            };
            done(&inbox, &ctx, message);
        });
    }

    pub fn remove(inbox: Inbox, ctx: egui::Context, path: String) {
        spawn_local(async move {
            // A missing entry is fine: the book may never have been saved
            if let Err(e) = remove_entry(&path).await {
                let not_found = e.dyn_ref::<js_sys::Error>().is_some_and(|e| e.name() == "NotFoundError");
                if !not_found {
                    done(&inbox, &ctx, Incoming::StorageError(format!("{e:?}")));
                }
            }
        });
    }
}
