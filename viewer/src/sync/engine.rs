//! The syncing itself: when to sync, and the steps of a sync, against any storage behind [`Provider`].
//!
//! A sync downloads `/state.json`, lets the app take what is newer ([`Host::adopt`]), uploads the merged
//! records if they differ (only if nobody wrote them in between: the file's revision must still match, or
//! the sync starts over), then lists the folder to upload the books only this device has, download the ones
//! it lacks, and delete the files of removed books. A local change is pushed directly, against the revision
//! of the last sync, falling back to a full sync when another device wrote in between.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use super::{book_path, merge, BookRecord, SyncDoc, FORMAT};

/// The state file in the synced folder.
pub const STATE_PATH: &str = "/state.json";
/// Seconds after a local change before it is pushed, so that several changes go together.
pub const DEBOUNCE: f64 = 3.0;
/// Seconds between cheap checks for changes made by other devices.
pub const CHECK_EVERY: f64 = 60.0;
/// Seconds between full syncs, in case a check missed something.
pub const PULL_EVERY: f64 = 600.0;
/// Seconds before retrying after the first error; doubled after each, up to [`MAX_RETRY`].
const FIRST_RETRY: f64 = 15.0;
const MAX_RETRY: f64 = 600.0;
/// Dropbox takes files of up to 150 MB in a single upload.
pub const MAX_UPLOAD: usize = 150 << 20;

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderError {
    /// A write with a revision that is no longer the file's (someone wrote in between)
    Conflict,
    /// The cursor of [`Provider::changes`] is no longer valid: list again
    Reset,
    /// Signed out (the access was revoked): sign in again
    Auth(String),
    /// Anything else (offline, server error…), retried later
    Other(String),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::Conflict => write!(f, "written by another device in the meantime"),
            ProviderError::Reset => write!(f, "the folder's history was reset"),
            ProviderError::Auth(e) => write!(f, "signed out: {e}"),
            ProviderError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub type Done<T> = Box<dyn FnOnce(Result<T, ProviderError>) + Send>;

/// How to write a file.
#[derive(Debug, Clone, PartialEq)]
pub enum WriteMode {
    /// Whatever is there
    Overwrite,
    /// Only if there is no file yet
    Add,
    /// Only if the file still has this revision
    Update(String),
}

/// A file read: its contents and revision, and what the storage said about it (shown if it turns out damaged).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub rev: String,
    pub detail: String,
}

/// Files of the synced folder, as paths from its root in lower case ("/books/<hash>.epub"), and a cursor to
/// ask what changed since.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Listing {
    pub files: Vec<String>,
    pub cursor: String,
}

/// A storage for the synced files: a folder of files read, written and listed by path. Every call answers
/// once through `done`, from any thread.
pub trait Provider {
    /// The file's contents and revision, or `None` if there is no such file.
    fn read(&mut self, path: &str, done: Done<Option<Fetched>>);
    /// Write the file; answers with its new revision.
    fn write(&mut self, path: &str, bytes: Vec<u8>, mode: WriteMode, done: Done<String>);
    /// Every file of the folder, and a cursor.
    fn list(&mut self, done: Done<Listing>);
    /// The files added, changed or deleted since `cursor`, and a new cursor.
    fn changes(&mut self, cursor: &str, done: Done<Listing>);
    /// Delete the file; a missing file is not an error.
    fn delete(&mut self, path: &str, done: Done<()>);
}

/// What the app gives the engine, and takes from it.
pub trait Host {
    /// This device's records.
    fn local(&self) -> SyncDoc;
    /// Take what is newer in `remote`.
    fn adopt(&mut self, remote: &SyncDoc);
    /// Hashes of the books whose file is stored on this device, which can be uploaded.
    fn local_files(&self) -> Vec<String>;
    /// Whether every local book is named by its hash yet: until then, a book to download may be one this
    /// device already has.
    fn can_download(&self) -> bool {
        true
    }
    /// Read a book's file, to upload it, and answer with [`Engine::source`]; false if there is no such book.
    fn read_source(&mut self, hash: &str) -> bool;
    /// A book's file downloaded: add the book. An error is shown, and the book not downloaded again this session.
    /// `detail` is what the storage said about the file (status, type, size), for the error if it is damaged.
    fn downloaded(&mut self, hash: &str, record: &BookRecord, bytes: Vec<u8>, detail: &str) -> Result<(), String>;
}

/// What the sync is doing, to show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub busy: bool,
    /// When the last sync finished, in milliseconds since 1970-01-01 UTC
    pub last_synced: Option<u64>,
    /// The last error, until a sync succeeds
    pub error: Option<String>,
    /// The access was revoked: sign in again
    pub signed_out: bool,
    pub uploads: usize,
    pub downloads: usize,
    /// Books listed by another device whose file is not uploaded yet
    pub waiting: usize,
    /// Books that could not be synced, with why (shown, never stopping the sync)
    pub problems: Vec<String>,
}

enum Reply {
    State(Result<Option<Fetched>, ProviderError>),
    Wrote(SyncDoc, Result<String, ProviderError>),
    Listed(Result<Listing, ProviderError>),
    Changes(Result<Listing, ProviderError>),
    Book(String, BookRecord, Result<Option<Fetched>, ProviderError>),
    Uploaded(Result<String, ProviderError>),
    Deleted(Result<(), ProviderError>),
    /// A book not uploaded after all (its file is missing, or too large)
    Skipped,
}

#[derive(Debug, Clone, PartialEq)]
enum Transfer {
    Upload { hash: String, path: String },
    Download { hash: String, record: BookRecord },
    Delete { path: String },
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Idle,
    /// Downloading the state file
    Reading,
    /// Uploading the merged records
    Writing,
    /// Listing the folder: before the books' transfers, or after them for the cursor (`last`)
    Listing { last: bool },
    /// Waiting for the app to read a book's file
    Source { hash: String, path: String },
    /// A book's file going up or down, or being deleted
    Transferring,
    /// Asking what changed since the cursor
    Checking,
}

pub struct Engine {
    provider: Box<dyn Provider>,
    replies: Arc<Mutex<Vec<Reply>>>,
    wake: Arc<dyn Fn() + Send + Sync>,
    phase: Phase,
    /// Time given to the last [`Engine::poll`] (seconds)
    now: f64,
    /// The state file as of the last sync, and its revision
    remote: Option<(SyncDoc, String)>,
    cursor: Option<String>,
    /// Files of the folder as last listed
    files: HashSet<String>,
    transfers: VecDeque<Transfer>,
    /// Some file was uploaded or deleted in this sync: list again at the end, for the cursor
    files_changed: bool,
    /// When a local change waits to be pushed, and the local records then (to push them without the app)
    dirty_since: Option<f64>,
    pending: Option<SyncDoc>,
    next_check: f64,
    next_pull: f64,
    retry_at: Option<f64>,
    failures: u32,
    /// Writes refused because of another device's write, in this sync
    conflicts: u32,
    /// Downloads that could not be added, not tried again this session
    skipped: HashSet<String>,
    status: Status,
}

impl Engine {
    /// `wake` is called when a reply arrives (to redraw, so that [`Engine::poll`] runs).
    pub fn new(provider: Box<dyn Provider>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            provider,
            replies: Arc::new(Mutex::new(Vec::new())),
            wake: Arc::new(wake),
            phase: Phase::Idle,
            now: 0.0,
            remote: None,
            cursor: None,
            files: HashSet::new(),
            transfers: VecDeque::new(),
            files_changed: false,
            dirty_since: None,
            pending: None,
            next_check: 0.0,
            next_pull: 0.0,
            retry_at: None,
            failures: 0,
            conflicts: 0,
            skipped: HashSet::new(),
            status: Status::default(),
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// The records as of the last sync.
    pub fn remote(&self) -> Option<&SyncDoc> {
        self.remote.as_ref().map(|(doc, _)| doc)
    }

    /// Something synced changed on this device, which now has these records: push them a few seconds from now.
    pub fn touch(&mut self, now: f64, local: SyncDoc) {
        self.dirty_since.get_or_insert(now);
        self.pending = Some(local);
    }

    /// Push a waiting change right away, with the records given to [`Engine::touch`] (the page is being hidden
    /// or closed, and may not draw another frame).
    pub fn flush_now(&mut self) {
        if self.phase == Phase::Idle && !self.status.signed_out && self.dirty_since.is_some() {
            if let Some(local) = self.pending.take() {
                self.push(local);
            }
        }
    }

    /// Sync fully as soon as possible (startup, the user asked, or after an error).
    pub fn sync_now(&mut self) {
        self.next_pull = f64::NEG_INFINITY;
        self.retry_at = None;
    }

    /// Handle the replies that arrived, and start what is due. Returns in how many seconds to call it again
    /// if nothing arrives before.
    pub fn poll(&mut self, now: f64, host: &mut dyn Host) -> Option<f64> {
        self.now = now;
        let replies = std::mem::take(&mut *self.replies.lock().unwrap());
        for reply in replies {
            self.handle(reply, host);
        }
        if self.phase == Phase::Idle && !self.status.signed_out {
            if let Some(retry) = self.retry_at {
                if now < retry {
                    return Some(retry - now);
                }
                self.retry_at = None;
                self.start_pull();
            } else if self.dirty_since.is_some_and(|since| now - since >= DEBOUNCE) {
                self.push(host.local());
            } else if now >= self.next_pull {
                self.start_pull();
            } else if now >= self.next_check && self.cursor.is_some() {
                self.start_check();
            }
        }
        self.status.busy = self.phase != Phase::Idle;
        if self.phase != Phase::Idle || self.status.signed_out {
            return None;
        }
        let mut next = self.next_pull.min(self.next_check);
        if let Some(since) = self.dirty_since {
            next = next.min(since + DEBOUNCE);
        }
        Some((next - now).max(0.0))
    }

    /// A book's file asked for with [`Host::read_source`], `None` if it is missing.
    pub fn source(&mut self, hash: &str, bytes: Option<Vec<u8>>) {
        let Phase::Source { hash: wanted, path } = &self.phase else { return };
        if wanted != hash {
            return;
        }
        let path = path.clone();
        match bytes {
            Some(bytes) if bytes.len() > MAX_UPLOAD => {
                self.problem(format!("{path}: larger than 150 MB, not uploaded"));
                self.skipped.insert(hash.to_string());
                self.phase = Phase::Transferring;
                self.reply(Reply::Skipped);
            }
            Some(bytes) => {
                self.phase = Phase::Transferring;
                let reply = self.replier(Reply::Uploaded);
                self.provider.write(&path, bytes, WriteMode::Overwrite, reply);
            }
            None => {
                self.phase = Phase::Transferring;
                self.reply(Reply::Skipped);
            }
        }
    }

    fn problem(&mut self, problem: String) {
        if !self.status.problems.contains(&problem) {
            self.status.problems.push(problem);
        }
    }

    fn reply(&self, reply: Reply) {
        self.replies.lock().unwrap().push(reply);
        (self.wake)();
    }

    /// A `done` callback that queues its result as a reply.
    fn replier<T: 'static>(&self, wrap: impl FnOnce(Result<T, ProviderError>) -> Reply + Send + 'static) -> Done<T> {
        let (replies, wake) = (self.replies.clone(), self.wake.clone());
        Box::new(move |result| {
            replies.lock().unwrap().push(wrap(result));
            wake();
        })
    }

    fn start_pull(&mut self) {
        self.phase = Phase::Reading;
        self.conflicts = 0;
        let reply = self.replier(Reply::State);
        self.provider.read(STATE_PATH, reply);
    }

    /// Push local changes against the revision of the last sync (a full sync if there was none).
    fn push(&mut self, local: SyncDoc) {
        let Some((remote, rev)) = &self.remote else {
            self.start_pull();
            return;
        };
        self.dirty_since = None;
        self.pending = None;
        let doc = merge(&local, remote);
        if &doc == remote {
            return;
        }
        let mode = WriteMode::Update(rev.clone());
        self.write_state(doc, mode);
    }

    fn start_check(&mut self) {
        let Some(cursor) = self.cursor.clone() else { return };
        self.phase = Phase::Checking;
        let reply = self.replier(Reply::Changes);
        self.provider.changes(&cursor, reply);
    }

    fn write_state(&mut self, doc: SyncDoc, mode: WriteMode) {
        self.phase = Phase::Writing;
        let bytes = serde_json::to_vec_pretty(&doc).unwrap_or_default();
        let reply = self.replier(move |result| Reply::Wrote(doc, result));
        self.provider.write(STATE_PATH, bytes, mode, reply);
    }

    fn list(&mut self, last: bool) {
        self.phase = Phase::Listing { last };
        let reply = self.replier(Reply::Listed);
        self.provider.list(reply);
    }

    fn fail(&mut self, error: ProviderError) {
        self.phase = Phase::Idle;
        self.transfers.clear();
        self.status.uploads = 0;
        self.status.downloads = 0;
        if let ProviderError::Auth(_) = error {
            self.status.signed_out = true;
        } else {
            self.failures += 1;
            let wait = (FIRST_RETRY * 2f64.powi(self.failures as i32 - 1)).min(MAX_RETRY);
            self.retry_at = Some(self.now + wait);
        }
        self.status.error = Some(error.to_string());
    }

    fn finish(&mut self) {
        self.phase = Phase::Idle;
        self.failures = 0;
        self.status.error = None;
        self.status.uploads = 0;
        self.status.downloads = 0;
        self.status.last_synced = Some(crate::state::now_millis());
        self.next_pull = self.now + PULL_EVERY;
        self.next_check = self.now + CHECK_EVERY;
    }

    fn handle(&mut self, reply: Reply, host: &mut dyn Host) {
        match reply {
            Reply::State(Ok(found)) if self.phase == Phase::Reading => {
                let (remote, rev) = match found {
                    Some(Fetched { bytes, rev, .. }) => match serde_json::from_slice::<SyncDoc>(&bytes) {
                        Ok(doc) if doc.format > FORMAT => {
                            return self.fail(ProviderError::Other("synced by a newer version of the app: update it (reload the page)".into()));
                        }
                        Ok(doc) => (Some(doc), Some(rev)),
                        // Never written over: it may be another version's
                        Err(e) => return self.fail(ProviderError::Other(format!("{} is unreadable: {e}", STATE_PATH))),
                    },
                    None => (None, None),
                };
                if let Some(remote) = &remote {
                    host.adopt(remote);
                }
                self.dirty_since = None;
                self.pending = None;
                let doc = merge(&host.local(), remote.as_ref().unwrap_or(&SyncDoc::default()));
                match (remote, rev) {
                    (Some(remote), Some(rev)) if remote == doc => {
                        self.remote = Some((remote, rev));
                        self.list(false);
                    }
                    (_, Some(rev)) => self.write_state(doc, WriteMode::Update(rev)),
                    (_, None) => self.write_state(doc, WriteMode::Add),
                }
            }
            Reply::Wrote(doc, Ok(rev)) if self.phase == Phase::Writing => {
                self.remote = Some((doc, rev));
                self.list(false);
            }
            Reply::Wrote(_, Err(ProviderError::Conflict)) if self.phase == Phase::Writing => {
                self.conflicts += 1;
                if self.conflicts > 5 {
                    self.fail(ProviderError::Other("the synced state keeps changing".into()));
                } else {
                    let conflicts = self.conflicts;
                    self.start_pull();
                    self.conflicts = conflicts;
                }
            }
            Reply::Listed(Ok(listing)) if matches!(self.phase, Phase::Listing { .. }) => {
                self.cursor = Some(listing.cursor);
                self.files = listing.files.into_iter().map(|f| f.to_lowercase()).collect();
                if self.phase == (Phase::Listing { last: true }) {
                    self.finish();
                } else {
                    self.plan_transfers(host);
                    self.next_transfer(host);
                }
            }
            Reply::Changes(Ok(listing)) if self.phase == Phase::Checking => {
                self.phase = Phase::Idle;
                if listing.files.is_empty() {
                    self.cursor = Some(listing.cursor);
                    self.next_check = self.now + CHECK_EVERY;
                } else {
                    self.start_pull();
                }
            }
            Reply::Changes(Err(ProviderError::Reset)) if self.phase == Phase::Checking => self.start_pull(),
            Reply::Book(hash, record, Ok(found)) if self.phase == Phase::Transferring => {
                self.status.downloads = self.status.downloads.saturating_sub(1);
                if let Some(Fetched { bytes, detail, .. }) = found {
                    if let Err(e) = host.downloaded(&hash, &record, bytes, &detail) {
                        self.problem(format!("{}: {e}", record.name));
                        self.skipped.insert(hash);
                    } else if let Some((remote, _)) = &self.remote {
                        // Its place, and the view if it was waiting for it
                        host.adopt(remote);
                    }
                }
                self.next_transfer(host);
            }
            Reply::Uploaded(result) if self.phase == Phase::Transferring => {
                self.status.uploads = self.status.uploads.saturating_sub(1);
                match result {
                    Ok(_) => self.files_changed = true,
                    Err(e) => return self.fail(e),
                }
                self.next_transfer(host);
            }
            Reply::Skipped if self.phase == Phase::Transferring => {
                self.status.uploads = self.status.uploads.saturating_sub(1);
                self.next_transfer(host);
            }
            Reply::Deleted(result) if self.phase == Phase::Transferring => match result {
                Ok(()) => {
                    self.files_changed = true;
                    self.next_transfer(host);
                }
                Err(e) => self.fail(e),
            },
            Reply::State(Err(e))
            | Reply::Wrote(_, Err(e))
            | Reply::Listed(Err(e))
            | Reply::Changes(Err(e))
            | Reply::Book(_, _, Err(e)) => self.fail(e),
            // Replies from before a failure
            _ => {}
        }
    }

    /// The books to upload, download and delete, from the records of the last sync and the folder's files.
    fn plan_transfers(&mut self, host: &mut dyn Host) {
        let Some((doc, _)) = &self.remote else { return };
        self.transfers.clear();
        self.files_changed = false;
        self.status.waiting = 0;
        let local = host.local();
        for hash in host.local_files() {
            let Some(record) = doc.books.get(&hash).filter(|r| !r.removed) else { continue };
            let path = record.path(&hash);
            if !self.files.contains(&path) && !self.skipped.contains(&hash) {
                self.transfers.push_back(Transfer::Upload { hash, path });
            }
        }
        let can_download = host.can_download();
        for (hash, record) in &doc.books {
            let path = record.path(hash);
            if record.removed {
                // Also any other extension the file was uploaded with
                for file in self.files.iter().filter(|f| f.starts_with(&format!("/books/{hash}."))) {
                    self.transfers.push_back(Transfer::Delete { path: file.clone() });
                }
            } else if !local.books.get(hash).is_some_and(|r| !r.removed) && !self.skipped.contains(hash) {
                if !self.files.contains(&path) {
                    self.status.waiting += 1;
                } else if can_download {
                    self.transfers.push_back(Transfer::Download { hash: hash.clone(), record: record.clone() });
                }
            }
        }
        self.status.uploads = self.transfers.iter().filter(|t| matches!(t, Transfer::Upload { .. })).count();
        self.status.downloads = self.transfers.iter().filter(|t| matches!(t, Transfer::Download { .. })).count();
    }

    fn next_transfer(&mut self, host: &mut dyn Host) {
        match self.transfers.pop_front() {
            Some(Transfer::Upload { hash, path }) => {
                self.phase = Phase::Source { hash: hash.clone(), path };
                if !host.read_source(&hash) {
                    self.phase = Phase::Transferring;
                    self.reply(Reply::Skipped);
                }
            }
            Some(Transfer::Download { hash, record }) => {
                self.phase = Phase::Transferring;
                let path = book_path(&hash, &record.file_name);
                let reply = self.replier(move |result| Reply::Book(hash, record, result));
                self.provider.read(&path, reply);
            }
            Some(Transfer::Delete { path }) => {
                self.phase = Phase::Transferring;
                let reply = self.replier(Reply::Deleted);
                self.provider.delete(&path, reply);
            }
            None if self.files_changed => self.list(true),
            None => self.finish(),
        }
    }
}
