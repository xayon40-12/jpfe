//! A small persistent file store: the browser's origin private file system (OPFS) on the web,
//! and a folder in the user's data directory on the desktop. Reads and writes report back
//! through the [`Inbox`], since they are asynchronous on the web.

use crate::platform::{Inbox, Incoming};

/// What is stored, and where.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    /// Book list, reading progress and settings (JSON)
    State,
    /// Version of the stored dictionary files (web only)
    DictionaryVersion,
    /// One zstd-compressed dictionary file, as downloaded (web only; unpacked on each load,
    /// since keeping the 200 MB unpacked dictionary makes Safari run out of memory)
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
            Key::DictionaryFile(name) => format!("dict/{name}.zst"),
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
/// On the web the bytes are copied out of the app's memory right away; writes happen one at a time.
pub fn write(inbox: &Inbox, ctx: &egui::Context, key: Key, bytes: &[u8]) {
    imp::write(inbox.clone(), ctx.clone(), key, bytes);
}

/// Forget the stored dictionary (before storing another version).
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
pub fn remove_dictionary(inbox: &Inbox, ctx: &egui::Context) {
    imp::remove(inbox.clone(), ctx.clone(), "dict".to_string());
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

    pub fn write(inbox: Inbox, ctx: egui::Context, key: Key, bytes: &[u8]) {
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

    /// Large single writes fail in Safari ("UnknownError ... e.g. out of memory"), so files are written in parts.
    const CHUNK: u32 = 4 << 20;

    thread_local! {
        /// The last write in the queue: Safari also fails when many large writes run at once
        static QUEUE: std::cell::RefCell<js_sys::Promise> = std::cell::RefCell::new(js_sys::Promise::resolve(&JsValue::UNDEFINED));
    }

    /// Run `task` after every write queued before it.
    async fn queued<T>(task: impl std::future::Future<Output = T>) -> T {
        let mut finish = None;
        let this = js_sys::Promise::new(&mut |resolve, _| finish = Some(resolve));
        let previous = QUEUE.with(|q| q.replace(this));
        let _ = JsFuture::from(previous).await;
        let result = task.await;
        if let Some(finish) = finish {
            let _ = finish.call0(&JsValue::NULL);
        }
        result
    }

    async fn write_file(path: &str, bytes: js_sys::Uint8Array) -> Result<(), JsValue> {
        let (dir, name) = parent(path, true).await?;
        let options = FileSystemGetFileOptions::new();
        options.set_create(true);
        let handle: FileSystemFileHandle =
            JsFuture::from(dir.get_file_handle_with_options(&name, &options)).await?.unchecked_into();
        // Safari has no `createWritable` (or refuses it) outside of workers
        if js_sys::Reflect::has(&handle, &"createWritable".into()).unwrap_or(false) {
            let written = async {
                let stream: FileSystemWritableFileStream = JsFuture::from(handle.create_writable()).await?.unchecked_into();
                let mut at = 0;
                while at < bytes.length() {
                    let end = (at + CHUNK).min(bytes.length());
                    JsFuture::from(stream.write_with_js_u8_array(&bytes.subarray(at, end))?).await?;
                    at = end;
                }
                // The file is replaced only when the stream is closed, so a half-written file is never seen
                JsFuture::from(stream.close()).await
            };
            if written.await.is_ok() {
                return Ok(());
            }
        }
        worker::write(path, bytes).await
    }

    /// Writes through a dedicated worker with a sync access handle, which every browser with OPFS supports.
    mod worker {
        use std::cell::{Cell, RefCell};
        use std::collections::HashMap;
        use wasm_bindgen::prelude::*;
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;

        /// Writes are queued, since a file can only have one sync access handle at a time.
        const SCRIPT: &str = r#"
            const CHUNK = 4 << 20;
            let queue = Promise.resolve();
            async function write({ id, path, bytes }) {
                try {
                    let dir = await navigator.storage.getDirectory();
                    const parts = path.split('/');
                    const name = parts.pop();
                    for (const part of parts) dir = await dir.getDirectoryHandle(part, { create: true });
                    const file = await dir.getFileHandle(name, { create: true });
                    const handle = await file.createSyncAccessHandle();
                    try {
                        handle.truncate(0);
                        let at = 0;
                        while (at < bytes.byteLength) at += handle.write(bytes.subarray(at, at + CHUNK), { at });
                        handle.flush();
                    } finally {
                        handle.close();
                    }
                    postMessage({ id, ok: true });
                } catch (error) {
                    postMessage({ id, ok: false, error: String(error) });
                }
            }
            onmessage = (event) => { queue = queue.then(() => write(event.data)); };
        "#;

        struct Writer {
            worker: web_sys::Worker,
            _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
        }

        thread_local! {
            static WRITER: RefCell<Option<Writer>> = const { RefCell::new(None) };
            static NEXT_ID: Cell<u32> = const { Cell::new(0) };
            /// Resolve and reject functions of the writes in progress
            static PENDING: RefCell<HashMap<u32, (js_sys::Function, js_sys::Function)>> = RefCell::new(HashMap::new());
        }

        fn start() -> Result<Writer, JsValue> {
            let parts = js_sys::Array::of1(&JsValue::from_str(SCRIPT));
            let options = web_sys::BlobPropertyBag::new();
            options.set_type("text/javascript");
            let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)?;
            let url = web_sys::Url::create_object_url_with_blob(&blob)?;
            let worker = web_sys::Worker::new(&url)?;
            let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|event: web_sys::MessageEvent| {
                let data = event.data();
                let get = |name: &str| js_sys::Reflect::get(&data, &name.into()).unwrap_or(JsValue::UNDEFINED);
                let Some(id) = get("id").as_f64().map(|id| id as u32) else { return };
                if let Some((resolve, reject)) = PENDING.with_borrow_mut(|p| p.remove(&id)) {
                    let _ = if get("ok").is_truthy() { resolve.call0(&JsValue::NULL) } else { reject.call1(&JsValue::NULL, &get("error")) };
                }
            });
            worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
            Ok(Writer { worker, _on_message: on_message })
        }

        pub async fn write(path: &str, array: js_sys::Uint8Array) -> Result<(), JsValue> {
            let id = NEXT_ID.with(|n| {
                let id = n.get();
                n.set(id.wrapping_add(1));
                id
            });
            let message = js_sys::Object::new();
            js_sys::Reflect::set(&message, &"id".into(), &id.into())?;
            js_sys::Reflect::set(&message, &"path".into(), &path.into())?;
            js_sys::Reflect::set(&message, &"bytes".into(), &array)?;
            let promise = js_sys::Promise::new(&mut |resolve, reject| {
                PENDING.with_borrow_mut(|p| p.insert(id, (resolve, reject)));
            });
            WRITER.with_borrow_mut(|writer| {
                if writer.is_none() {
                    *writer = Some(start()?);
                }
                let worker = &writer.as_ref().unwrap().worker;
                // Transfer the buffer instead of copying it once more
                worker.post_message_with_transfer(&message, &js_sys::Array::of1(&array.buffer()))
            })?;
            JsFuture::from(promise).await.map(|_| ())
        }
    }

    async fn remove_entry(path: &str) -> Result<(), JsValue> {
        let (dir, name) = parent(path, false).await?;
        let options = FileSystemRemoveOptions::new();
        options.set_recursive(true);
        JsFuture::from(dir.remove_entry_with_options(&name, &options)).await?;
        Ok(())
    }

    fn is_not_found(e: &JsValue) -> bool {
        e.dyn_ref::<js_sys::Error>().is_some_and(|e| e.name() == "NotFoundError")
    }

    /// Reported in the browser console too, to diagnose browsers where storage fails.
    fn failure(action: &str, path: &str, e: &JsValue) -> Incoming {
        let message = format!("{action} {path}: {}", e.as_string().unwrap_or_else(|| format!("{e:?}")));
        web_sys::console::error_1(&format!("jpfe storage: {message}").into());
        Incoming::StorageError(message)
    }

    pub fn read(inbox: Inbox, ctx: egui::Context, key: Key) {
        spawn_local(async move {
            let path = key.path();
            let bytes = match read_file(&path).await {
                Ok(bytes) => Some(bytes),
                Err(e) if is_not_found(&e) => None,
                Err(e) => {
                    done(&inbox, &ctx, failure("reading", &path, &e));
                    None
                }
            };
            done(&inbox, &ctx, Incoming::Stored { key, bytes });
        });
    }

    pub fn write(inbox: Inbox, ctx: egui::Context, key: Key, bytes: &[u8]) {
        // Copied now, so the app does not keep a second copy of large files in its own memory
        let array = js_sys::Uint8Array::from(bytes);
        spawn_local(async move {
            let message = match queued(write_file(&key.path(), array)).await {
                Ok(()) => Incoming::Written(key),
                Err(e) => failure("writing", &key.path(), &e),
            };
            done(&inbox, &ctx, message);
        });
    }

    pub fn remove(inbox: Inbox, ctx: egui::Context, path: String) {
        spawn_local(async move {
            // A missing entry is fine: the book may never have been saved
            if let Err(e) = queued(remove_entry(&path)).await {
                if !is_not_found(&e) {
                    done(&inbox, &ctx, failure("removing", &path, &e));
                }
            }
        });
    }
}
