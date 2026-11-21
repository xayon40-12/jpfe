//! The merge rules, and two devices syncing through an in-memory provider.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use super::engine::{Done, Engine, Fetched, Rejected, Host, Listing, Provider, ProviderError, WriteMode, STATE_PATH};
use super::*;
use crate::book::SavedBook;
use crate::state::{stamp_at, SavedState};

fn place(chapter: usize, modified: u64) -> BookReaderState {
    BookReaderState { chapter, offset: 0, text: String::new(), modified }
}

fn book(id: u64, hash: &str, learned: Option<usize>, modified: u64) -> SavedBook {
    SavedBook {
        id,
        name: format!("Book {hash}"),
        file_name: format!("{hash}.epub"),
        learned,
        hash: Some(hash.into()),
        modified,
    }
}

fn entry(learned: Option<usize>, modified: u64) -> BookRecord {
    BookRecord { modified, removed: false, name: "A".into(), file_name: "a.epub".into(), learned }
}

// ----- Merging records

#[test]
fn newest_wins_record_by_record() {
    let mut a = SyncDoc::default();
    let mut b = SyncDoc::default();
    a.books.insert("x".into(), entry(Some(1), 10));
    b.books.insert("x".into(), entry(Some(2), 20));
    a.books.insert("y".into(), entry(Some(5), 30));
    b.books.insert("y".into(), entry(Some(6), 25));
    a.places.insert("x".into(), place(3, 100));
    b.places.insert("y".into(), place(4, 100));
    b.places.insert("x".into(), place(1, 90));
    let mut settings = SettingsRecord { modified: 5, ..SettingsRecord::default() };
    a.settings = Some(settings.clone());
    settings.modified = 6;
    settings.reader_font_size = 30.0;
    b.settings = Some(settings);
    a.view = Some(ViewRecord { modified: 50, reading: Some("x".into()) });

    let merged = merge(&a, &b);
    assert_eq!(merged.books["x"].learned, Some(2));
    assert_eq!(merged.books["y"].learned, Some(5));
    // Each book keeps its own newest place
    assert_eq!(merged.places["x"].chapter, 3);
    assert_eq!(merged.places["y"].chapter, 4);
    assert_eq!(merged.settings.as_ref().unwrap().reader_font_size, 30.0);
    assert_eq!(merged.view.as_ref().unwrap().reading.as_deref(), Some("x"));
    // The same whichever way round, and merging again changes nothing
    assert_eq!(merge(&b, &a), merged);
    assert_eq!(merge(&merged, &a), merged);
}

#[test]
fn ties_pick_the_same_copy_on_every_device() {
    let mut a = SyncDoc::default();
    let mut b = SyncDoc::default();
    a.books.insert("x".into(), entry(Some(1), 10));
    b.books.insert("x".into(), entry(Some(2), 10));
    assert_eq!(merge(&a, &b), merge(&b, &a));
}

#[test]
fn removals_win_over_older_entries_and_lose_to_newer_ones() {
    let mut a = SyncDoc::default();
    let mut b = SyncDoc::default();
    a.books.insert("x".into(), entry(Some(1), 10));
    a.places.insert("x".into(), place(2, 15));
    b.books.insert("x".into(), BookRecord::removal(20));
    let merged = merge(&a, &b);
    assert!(merged.books["x"].removed);
    // The place of a removed book is dropped
    assert!(merged.places.is_empty());

    // Added again later
    a.books.insert("x".into(), entry(Some(0), 30));
    let merged = merge(&a, &b);
    assert!(!merged.books["x"].removed);
    assert_eq!(merged.places["x"].chapter, 2);
}

#[test]
fn local_ids_map_to_hashes_and_back() {
    let mut phone = SavedState::default();
    phone.books.push(book(0, "aa", Some(1), 5));
    phone.books.push(book(1, "bb", None, 5));
    // Not hashed yet (saved before sync): not synced
    phone.books.push(SavedBook { id: 2, name: "C".into(), file_name: "c.pdf".into(), ..SavedBook::default() });
    phone.reader_books.insert(1, place(4, 50));
    phone.reader_books.insert(2, place(9, 50));
    phone.view = crate::state::ViewState { reading: Some(1), modified: 60 };
    let doc = local_doc(&phone);
    assert_eq!(doc.books.keys().collect::<Vec<_>>(), ["aa", "bb"]);
    assert_eq!(doc.places.keys().collect::<Vec<_>>(), ["bb"]);
    assert_eq!(doc.view.as_ref().unwrap().reading.as_deref(), Some("bb"));
    // Settings never changed are not synced
    assert!(doc.settings.is_none());

    // The same books on the computer, under other ids
    let mut computer = SavedState::default();
    computer.books.push(book(7, "bb", None, 1));
    computer.books.push(book(8, "aa", Some(1), 1));
    let adopted = adopt(&mut computer, &doc, None, true);
    assert_eq!(adopted.places, [7]);
    assert_eq!(computer.reader_books[&7].chapter, 4);
    assert_eq!(computer.view.reading, Some(7));
    assert!(adopted.view);

    // A book opened twice is synced as the first
    let mut twice = SavedState::default();
    twice.books.push(book(3, "aa", Some(2), 5));
    twice.books.push(book(1, "aa", Some(0), 5));
    assert_eq!(local_doc(&twice).books["aa"].learned, Some(0));
}

#[test]
fn old_states_sync_what_they_had_without_overwriting_newer_changes() {
    // A state saved before sync, once its book is hashed
    let old = r#"{"settings":{"min_chapters":4},"next_id":1,"books":[{"id":0,"name":"本","file_name":"本.epub","learned":3}]}"#;
    let mut migrated = SavedState::load(Some(old.as_bytes()));
    migrated.books[0].hash = Some("aa".into());
    let doc = local_doc(&migrated);
    assert_eq!(doc.settings.as_ref().unwrap().modified, 1);
    assert_eq!(doc.books["aa"].modified, 1);

    // A new device takes its settings and progress
    let mut fresh = SavedState::default();
    fresh.books.push(book(0, "aa", None, 0));
    adopt(&mut fresh, &doc, None, false);
    assert_eq!(fresh.settings.min_chapters, 4);
    assert_eq!(fresh.books[0].learned, Some(3));

    // But anything changed since sync wins over it
    let mut changed = SavedState { settings_modified: 1000, ..SavedState::default() };
    changed.books.push(book(0, "aa", Some(9), 1000));
    adopt(&mut changed, &doc, None, false);
    assert_eq!(changed.settings.min_chapters, SavedState::default().settings.min_chapters);
    assert_eq!(changed.books[0].learned, Some(9));
}

#[test]
fn the_book_being_read_never_jumps() {
    let mut local = SavedState::default();
    local.books.push(book(0, "aa", None, 1));
    local.books.push(book(1, "bb", None, 1));
    local.reader_books.insert(0, place(1, 10));
    local.reader_books.insert(1, place(1, 10));
    let mut remote = local_doc(&local);
    remote.places.insert("aa".into(), place(5, 20));
    remote.places.insert("bb".into(), place(6, 20));
    remote.books.insert("aa".into(), BookRecord::removal(30));
    remote.view = Some(ViewRecord { modified: 40, reading: Some("bb".into()) });

    // Book 0 is open in the reader: its newer place is offered, not applied, and its removal waits
    let adopted = adopt(&mut local, &remote, Some(0), false);
    assert_eq!(adopted.offered.as_ref().map(|(id, p)| (*id, p.chapter)), Some((0, 5)));
    assert_eq!(local.reader_books[&0].chapter, 1);
    assert_eq!(local.reader_books[&1].chapter, 6);
    assert!(adopted.removed.is_empty() && local.books.len() == 2);
    // Nor does the view switch
    assert!(!adopted.view && local.view.reading.is_none());

    // Once the reader has left it
    let adopted = adopt(&mut local, &remote, None, false);
    assert_eq!(adopted.removed, [0]);
    assert!(adopted.offered.is_none());
    assert!(!local.reader_books.contains_key(&0));
    assert_eq!(local.removed["aa"], 30);
}

#[test]
fn a_future_timestamp_does_not_freeze_a_record() {
    let mut local = SavedState::default();
    local.books.push(book(0, "aa", None, 1));
    let mut remote = local_doc(&local);
    // From a device whose clock is a year ahead
    let future = 1_830_000_000_000;
    remote.places.insert("aa".into(), place(5, future));
    adopt(&mut local, &remote, None, false);
    // The next page turn here is stamped after it, so it replaces it
    let now = 1_800_000_000_000;
    let turned = place(6, stamp_at(now, local.reader_books[&0].modified));
    local.reader_books.insert(0, turned);
    let merged = merge(&local_doc(&local), &remote);
    assert_eq!(merged.places["aa"].chapter, 6);
}

// ----- Two devices through an in-memory provider

/// An in-memory folder, shared by the devices, answering at once.
#[derive(Clone, Default)]
struct FakeCloud(Rc<RefCell<Cloud>>);

#[derive(Default)]
struct Cloud {
    files: BTreeMap<String, (Vec<u8>, String)>,
    /// Every change, numbered: the cursor is the number of changes seen
    changes: Vec<String>,
    revisions: u64,
    offline: bool,
}

impl Cloud {
    fn changed(&mut self, path: &str) -> String {
        self.revisions += 1;
        self.changes.push(path.to_string());
        format!("r{}", self.revisions)
    }
}

impl Provider for FakeCloud {
    fn read(&mut self, path: &str, done: Done<Option<Fetched>>) {
        let cloud = self.0.borrow();
        if cloud.offline {
            return done(Err(ProviderError::Other("offline".into())));
        }
        done(Ok(cloud.files.get(path).map(|(bytes, rev)| Fetched { bytes: bytes.clone(), rev: rev.clone(), detail: String::new() })));
    }

    fn write(&mut self, path: &str, bytes: Vec<u8>, mode: WriteMode, done: Done<String>) {
        let mut cloud = self.0.borrow_mut();
        if cloud.offline {
            return done(Err(ProviderError::Other("offline".into())));
        }
        let current = cloud.files.get(path).map(|(_, rev)| rev.clone());
        let allowed = match &mode {
            WriteMode::Overwrite => true,
            WriteMode::Add => current.is_none(),
            WriteMode::Update(rev) => current.as_ref() == Some(rev),
        };
        if !allowed {
            return done(Err(ProviderError::Conflict));
        }
        let rev = cloud.changed(path);
        cloud.files.insert(path.to_string(), (bytes, rev.clone()));
        done(Ok(rev));
    }

    fn list(&mut self, done: Done<Listing>) {
        let cloud = self.0.borrow();
        done(Ok(Listing { files: cloud.files.keys().cloned().collect(), cursor: cloud.changes.len().to_string() }))
    }

    fn changes(&mut self, cursor: &str, done: Done<Listing>) {
        let cloud = self.0.borrow();
        let seen: usize = cursor.parse().unwrap_or(0);
        done(Ok(Listing { files: cloud.changes[seen.min(cloud.changes.len())..].to_vec(), cursor: cloud.changes.len().to_string() }))
    }

    fn delete(&mut self, path: &str, done: Done<()>) {
        let mut cloud = self.0.borrow_mut();
        if cloud.files.remove(path).is_some() {
            cloud.changed(path);
        }
        done(Ok(()));
    }
}

/// A device: its saved state and stored book files, as the app keeps them.
#[derive(Default)]
struct Device {
    state: SavedState,
    files: HashMap<u64, Vec<u8>>,
    /// The book open in the reader
    open: Option<u64>,
    offered: Option<(u64, BookReaderState)>,
    reads: Vec<String>,
    /// The clock (ms)
    clock: u64,
}

impl Device {
    fn add_book(&mut self, bytes: &[u8], name: &str) -> u64 {
        self.clock += 1000;
        let hash = content_hash(bytes);
        let id = self.state.next_id;
        self.state.next_id += 1;
        let modified = stamp_at(self.clock, self.state.removed.remove(&hash).unwrap_or(0));
        self.state.books.push(SavedBook {
            id,
            name: name.into(),
            file_name: format!("{name}.epub"),
            learned: None,
            hash: Some(hash),
            modified,
        });
        self.files.insert(id, bytes.to_vec());
        id
    }

    fn remove_book(&mut self, id: u64) {
        self.clock += 1000;
        let book = self.state.books.iter().find(|b| b.id == id).unwrap().clone();
        self.state.removed.insert(book.hash.unwrap(), stamp_at(self.clock, book.modified));
        self.state.books.retain(|b| b.id != id);
        self.state.reader_books.remove(&id);
        self.files.remove(&id);
    }

    fn turn_to(&mut self, id: u64, chapter: usize) {
        self.clock += 1000;
        let previous = self.state.reader_books.get(&id).map_or(0, |p| p.modified);
        self.state.reader_books.insert(id, place(chapter, stamp_at(self.clock, previous)));
    }

    fn id_of(&self, bytes: &[u8]) -> Option<u64> {
        let hash = content_hash(bytes);
        self.state.books.iter().find(|b| b.hash.as_deref() == Some(&hash)).map(|b| b.id)
    }
}

impl Host for Device {
    fn local(&self) -> SyncDoc {
        local_doc(&self.state)
    }

    fn adopt(&mut self, remote: &SyncDoc) {
        let adopted = adopt(&mut self.state, remote, self.open, false);
        for id in adopted.removed {
            self.files.remove(&id);
        }
        self.offered = adopted.offered;
    }

    fn local_files(&self) -> Vec<String> {
        self.state.books.iter().filter(|b| self.files.contains_key(&b.id)).filter_map(|b| b.hash.clone()).collect()
    }

    fn read_source(&mut self, hash: &str) -> bool {
        // As the app: a stored copy that is not the book is not uploaded
        let id = self.id_of_hash(hash);
        if id.and_then(|id| self.files.get(&id)).is_none_or(|bytes| content_hash(bytes) != hash) {
            return false;
        }
        self.reads.push(hash.to_string());
        true
    }

    fn downloaded(&mut self, hash: &str, record: &BookRecord, bytes: Vec<u8>, _detail: &str) -> Result<(), Rejected> {
        if content_hash(&bytes) != hash {
            return Err(Rejected::Damaged("damaged".into()));
        }
        let id = self.state.next_id;
        self.state.next_id += 1;
        self.state.removed.remove(hash);
        self.state.books.push(SavedBook {
            id,
            name: record.name.clone(),
            file_name: record.file_name.clone(),
            learned: record.learned,
            hash: Some(hash.to_string()),
            modified: record.modified,
        });
        self.files.insert(id, bytes);
        Ok(())
    }
}

struct Pair {
    cloud: FakeCloud,
    phone: (Device, Engine),
    computer: (Device, Engine),
    /// Seconds, for the engines
    time: f64,
}

impl Pair {
    fn new() -> Self {
        let cloud = FakeCloud::default();
        let engine = || Engine::new(Box::new(cloud.clone()), || {});
        Pair { phone: (Device::default(), engine()), computer: (Device::default(), engine()), cloud: cloud.clone(), time: 0.0 }
    }

    /// Run a device's engine until it is idle, answering its file reads.
    fn settle((device, engine): &mut (Device, Engine), time: f64) {
        for _ in 0..200 {
            engine.poll(time, device);
            for hash in std::mem::take(&mut device.reads) {
                let bytes = device.id_of_hash(&hash).and_then(|id| device.files.get(&id).cloned());
                engine.source(&hash, bytes);
            }
            if engine.is_idle() && device.reads.is_empty() {
                engine.poll(time, device);
                if engine.is_idle() {
                    return;
                }
            }
        }
        panic!("the sync never settled");
    }

    /// A full sync on one device.
    fn sync(&mut self, phone: bool) {
        self.time += 1.0;
        let device = if phone { &mut self.phone } else { &mut self.computer };
        device.1.sync_now();
        Self::settle(device, self.time);
    }

    /// Both devices, twice each, so that each sees the other's changes.
    fn sync_both(&mut self) {
        for phone in [true, false, true, false] {
            self.sync(phone);
        }
    }
}

impl Device {
    fn id_of_hash(&self, hash: &str) -> Option<u64> {
        self.state.books.iter().find(|b| b.hash.as_deref() == Some(hash)).map(|b| b.id)
    }
}

const BOOK_A: &[u8] = b"PK book A";
const BOOK_B: &[u8] = b"PK book B";

#[test]
fn a_place_on_each_device_keeps_both() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    pair.computer.0.clock = 1_000_500;
    let a = pair.phone.0.add_book(BOOK_A, "A");
    let b = pair.phone.0.add_book(BOOK_B, "B");
    pair.sync_both();
    // The computer downloaded both books, under its own ids
    let (ca, cb) = (pair.computer.0.id_of(BOOK_A).unwrap(), pair.computer.0.id_of(BOOK_B).unwrap());
    assert_eq!(pair.computer.0.files[&ca], BOOK_A);
    assert_eq!(pair.computer.0.files[&cb], BOOK_B);

    // Reading A on the phone and B on the computer, without syncing in between
    pair.phone.0.turn_to(a, 3);
    pair.computer.0.turn_to(cb, 7);
    pair.sync_both();
    for (device, a, b) in [(&pair.phone.0, a, b), (&pair.computer.0, ca, cb)] {
        assert_eq!(device.state.reader_books[&a].chapter, 3);
        assert_eq!(device.state.reader_books[&b].chapter, 7);
    }
}

#[test]
fn a_book_added_on_one_device_and_removed_on_the_other() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    pair.computer.0.clock = 1_000_000;
    pair.computer.0.add_book(BOOK_A, "A");
    pair.sync_both();
    let on_phone = pair.phone.0.id_of(BOOK_A).expect("downloaded on the phone");
    assert!(pair.cloud.0.borrow().files.contains_key(&book_path(&content_hash(BOOK_A), "A.epub")));

    // Removed on the phone: removed on the computer, and its file deleted from the folder
    pair.phone.0.remove_book(on_phone);
    pair.sync_both();
    assert!(pair.computer.0.id_of(BOOK_A).is_none());
    assert!(pair.computer.0.files.is_empty());
    let path = book_path(&content_hash(BOOK_A), "A.epub");
    assert!(!pair.cloud.0.borrow().files.contains_key(&path));
    // The removal is kept, so the book does not come back
    pair.sync_both();
    assert!(pair.phone.0.id_of(BOOK_A).is_none() && pair.computer.0.id_of(BOOK_A).is_none());

    // Added again later on the computer: it comes back everywhere
    pair.computer.0.add_book(BOOK_A, "A");
    pair.sync_both();
    assert!(pair.phone.0.id_of(BOOK_A).is_some());
    assert!(pair.cloud.0.borrow().files.contains_key(&path));
}

#[test]
fn conflicting_settings_keep_the_newest() {
    let mut pair = Pair::new();
    pair.sync_both();
    pair.phone.0.state.reader_font_size = 30.0;
    pair.phone.0.state.settings_modified = 2_000;
    pair.computer.0.state.reader_font_size = 14.0;
    pair.computer.0.state.reader_vertical = false;
    pair.computer.0.state.settings_modified = 3_000;
    // The phone writes first; the computer's write then conflicts, and it merges first
    pair.sync(true);
    pair.sync(false);
    pair.sync(true);
    for device in [&pair.phone.0, &pair.computer.0] {
        assert_eq!(device.state.reader_font_size, 14.0);
        assert!(!device.state.reader_vertical);
    }
}

#[test]
fn a_write_in_between_is_merged_not_lost() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    let a = pair.phone.0.add_book(BOOK_A, "A");
    let b = pair.phone.0.add_book(BOOK_B, "B");
    pair.sync_both();
    let cb = pair.computer.0.id_of(BOOK_B).unwrap();
    // Both push changes against the same revision
    pair.phone.0.turn_to(a, 2);
    pair.computer.0.clock = 2_000_000;
    pair.computer.0.turn_to(cb, 5);
    pair.time += 10.0;
    let (phone, computer) = (pair.phone.0.local(), pair.computer.0.local());
    pair.phone.1.touch(pair.time, phone);
    pair.computer.1.touch(pair.time, computer);
    pair.time += 10.0;
    Pair::settle(&mut pair.phone, pair.time);
    Pair::settle(&mut pair.computer, pair.time);
    let doc: SyncDoc = serde_json::from_slice(&pair.cloud.0.borrow().files[STATE_PATH].0).unwrap();
    assert_eq!(doc.places[&content_hash(BOOK_A)].chapter, 2);
    assert_eq!(doc.places[&content_hash(BOOK_B)].chapter, 5);
    pair.sync(true);
    assert_eq!(pair.phone.0.state.reader_books[&b].chapter, 5);
}

#[test]
fn a_newer_place_for_the_open_book_is_offered() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    let a = pair.phone.0.add_book(BOOK_A, "A");
    pair.phone.0.turn_to(a, 1);
    pair.sync_both();
    let ca = pair.computer.0.id_of(BOOK_A).unwrap();
    pair.phone.0.open = Some(a);
    pair.computer.0.clock = 2_000_000;
    pair.computer.0.turn_to(ca, 4);
    pair.sync_both();
    assert_eq!(pair.phone.0.state.reader_books[&a].chapter, 1);
    assert_eq!(pair.phone.0.offered.as_ref().map(|(_, p)| p.chapter), Some(4));
    // The computer's place stays in the folder until the phone turns a page or leaves the book
    pair.phone.0.open = None;
    pair.sync(true);
    assert_eq!(pair.phone.0.state.reader_books[&a].chapter, 4);
}

#[test]
fn a_book_stored_damaged_is_replaced_by_the_right_file() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    let a = pair.phone.0.add_book(BOOK_A, "A");
    // The phone stored its copy twice over (Safari's bug) and uploaded that
    pair.phone.0.files.insert(a, [BOOK_A, BOOK_A].concat());
    let path = book_path(&content_hash(BOOK_A), "A.epub");
    pair.sync(true);
    assert!(!pair.cloud.0.borrow().files.contains_key(&path), "a stored copy that is not the book is not uploaded");
    pair.cloud.0.borrow_mut().files.insert(path.clone(), ([BOOK_A, BOOK_A].concat(), "bad".into()));
    // The computer refuses it and deletes it from the folder
    pair.sync(false);
    assert!(pair.computer.0.id_of(BOOK_A).is_none());
    assert!(!pair.cloud.0.borrow().files.contains_key(&path));
    // The phone's copy repaired (the file opened again): uploaded, then downloaded
    pair.phone.0.files.insert(a, BOOK_A.to_vec());
    pair.sync_both();
    assert_eq!(pair.cloud.0.borrow().files[&path].0, BOOK_A);
    let on_computer = pair.computer.0.id_of(BOOK_A).expect("downloaded once uploaded right");
    assert_eq!(pair.computer.0.files[&on_computer], BOOK_A);
}

#[test]
fn errors_are_retried_and_never_lose_local_changes() {
    let mut pair = Pair::new();
    pair.phone.0.clock = 1_000_000;
    pair.cloud.0.borrow_mut().offline = true;
    pair.phone.0.add_book(BOOK_A, "A");
    pair.sync(true);
    assert!(pair.phone.1.status().error.is_some());
    assert!(pair.phone.0.id_of(BOOK_A).is_some());
    pair.cloud.0.borrow_mut().offline = false;
    // Retried after a while
    pair.time += 60.0;
    Pair::settle(&mut pair.phone, pair.time);
    assert!(pair.phone.1.status().error.is_none());
    pair.sync(false);
    assert!(pair.computer.0.id_of(BOOK_A).is_some());
}

#[test]
fn a_cheap_check_finds_other_devices_changes() {
    let mut pair = Pair::new();
    pair.sync_both();
    pair.phone.0.state.settings_modified = 5_000;
    pair.phone.0.state.reader_highlight = true;
    pair.sync(true);
    // Nothing for the computer until its next check
    pair.time += 1.0;
    Pair::settle(&mut pair.computer, pair.time);
    assert!(!pair.computer.0.state.reader_highlight);
    pair.time += engine::CHECK_EVERY + 1.0;
    Pair::settle(&mut pair.computer, pair.time);
    assert!(pair.computer.0.state.reader_highlight);
}
