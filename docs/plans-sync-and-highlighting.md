# Cloud sync and word highlighting: decisions and plan

This document records two features decided on 18 November 2026, before any code was written (both since built: cloud sync below, word highlighting in [decisions-reader.md](decisions-reader.md)): **syncing the viewer through the cloud** and **highlighting the parts of every word in the reader**. It covers what was decided and why, what was rejected, and the order of the work. When the work lands, its decisions move into their commits' history here (or into [decisions-reader.md](decisions-reader.md) for the reader), as the other documents do.

Both features build on the paginated reader with a saved place per book: decision 7 of [decisions-reader.md](decisions-reader.md) and its follow-up.

## Order of the work

1. **Finish the paginated reader** (done: decision 7 of [decisions-reader.md](decisions-reader.md), with the Safari startup fix). The user tried the first version and asked for three fixes:
   - **No drag animation:** a drag switches the page at once, like the arrow keys.
   - **Restored on reload:** the app remembers its view. If the reader was showing a book, reloading reopens the reader on that book at the same page. This is part of the saved settings, so it will be synced.
   - **Where the reader opens:** a book opens at its saved place if it has one, otherwise at its first chapter, which may be chapter 0, a prologue. It no longer depends on *Learned up to*, since readers usually learn a few chapters ahead.
2. **Commit and push** the working reader.
3. **Cloud sync and word highlighting in parallel.** Each runs in its own worktree on its own branch, since both touch `viewer/src/app.rs`: sync adds to the saved state, highlighting adds a reader setting.

## Cloud sync

### Decided: the user's own Dropbox, in an app folder

The settings, the reader's place in each book, the view the app was in, and the books themselves are synced through the user's Dropbox, in a folder only this app can see.

**Why Dropbox:**

- **No server to run, and no cost.** The viewer is a static site on GitHub Pages plus a desktop app, and stays that way.
- **The books stay in the user's own storage.** They are copyrighted, bought files: storing them on a server of ours would mean hosting every user's books.
- **It stays signed in.** Sign-in uses OAuth with PKCE, which needs no secret key, so it works from the web page and from the desktop app. An access token lasts 4 hours, and the app renews it with a refresh token that has no fixed expiry and can be reused, until the user revokes the app. The user only signs in again if they revoke it, or if the browser clears the site's storage, where the token is kept.
- **The app folder permission** limits what a stolen token could reach to that folder.
- **It is fast enough.** Settings and places are one small JSON file, read or written in a single request. Dropbox can report whether anything changed since a cursor (`list_folder/continue`), so checking costs almost nothing. Books are 2–23 MB each, sent once per book per device.
- **It works on every platform**, an iOS or Android app included, with the same HTTP code.

### Rejected or deferred

How long each option keeps the user signed in, for an app that runs in the browser:

| Option | Signed in for | Why not |
|---|---|---|
| **Dropbox** (chosen) | until the app is revoked | |
| OneDrive | 24 hours, fixed (renewing doesn't extend it) | signing in every day |
| Google Drive, `appDataFolder` | about 1 hour without a server of ours | renewing every hour; files are found by id, not path; desktop sign-in is harder |
| iCloud (CloudKit JS) | 30 minutes, or 2 weeks with "Keep me signed in" | see below |
| A server of ours (e.g. Cloudflare Workers, R2, D1) | as long as we choose | we build sign-in and store everyone's books; only reasonable for a single user |

**iCloud, for a future iOS app.** It can be shared between the web and an iOS app through CloudKit, not iCloud Drive. Both use the same container: the iOS app natively, the web page through CloudKit JS, with the data in the user's private database. Deferred, not rejected:

- **Sign-in:** the web version must sign in again at least every 2 weeks.
- **Cost:** it needs the Apple Developer Program ($99 a year).
- **Apple ID required:** Android and Windows users need one.
- **Storage:** the books count against the user's iCloud storage.
- **Tooling:** CloudKit JS is old, with recent forum reports of sign-in bugs on the web.

It can be added later as a second storage provider (see the design below).

### Built: the decisions (20 November 2026, commit "Dropbox sync: settings, places, the view and the books, merged record by record")

The design planned here (a small storage interface, books named by a hash of their file, newest wins record by record) was built as planned. The code is in `viewer/src/sync/` (`mod.rs`: the records and the merge; `engine.rs`: the storage interface and when and how a sync runs; `dropbox.rs`: Dropbox and its sign-in; `account.rs`: signing in and out, and the settings section) and `viewer/src/state.rs` (the saved state, moved out of `app.rs`, and the clock). Setting up the Dropbox app: [dropbox-setup.md](dropbox-setup.md).

**1. A small storage interface** (`Provider` in `engine.rs`): read a file (its contents and revision), write one (overwrite, add only if missing, or update only if it still has a given revision), list every file of the folder with a cursor, list what changed since a cursor, and delete one. Every call answers once through a callback, which suits both `ehttp` on the web and threads on the desktop. Dropbox is the first implementation; CloudKit would be a second one (its records map to files, its change tokens to the cursor). The engine itself knows nothing of Dropbox, and the tests run it against an in-memory folder shared by two simulated devices.

**2. Books named by their contents, beside their local id.** Each book gets the SHA-256 of its original file (the `sha2` crate, pure Rust, which builds for wasm), stored as `hash` in its saved entry. It **sits beside the local id rather than replacing it**: the id stays the key of the book's files in the device's storage (`books/<id>/`), of `reader_books`, of the view and of the order on screen, so none of that changed, and old storage needs no moving of files. Only the synced records are named by hash; `local_doc` and `adopt` (in `sync/mod.rs`) map between the two, and a book downloaded from another device gets the next local id there.

- **Mapping states saved before sync:** a book without a hash gets one the next time its stored file is read: when a new build analyzes it again (which every web update does), or, while signed in to Dropbox, by reading its file once after it is restored (one book at a time). Its place, progress and settings keep their local id, so nothing is lost. Books are only hashed when sync needs it, so users who do not sync pay nothing beyond the next update. Until every local book is hashed, the sync downloads nothing, so that a book this device already has is not downloaded twice.
- **The same file twice is the same book:** opening a file already open says so ("already open, as …") instead of adding it again. Two copies opened before this (with two ids) sync as the first one.

**3. What syncs** (`SyncDoc`, `/state.json` in the app folder): the settings as one record (word-list settings, the plots' log scales, the reader's font size, direction, *Paginate* and *Highlight words*), each book's entry (name, file name, *Learned up to*) or the record of its removal, each book's `BookReaderState`, the `ViewState`, and the books' original files (`/books/<sha256>.<epub|pdf>`). Not synced: analyses and the dictionary (each device rebuilds them), *Show debug information*, which book's statistics are shown, and the Dropbox sign-in, which stays on the device (local storage on the web, `dropbox-token` in the data folder on the desktop).

**4. Merging: newest wins, record by record.** Each record carries when it last changed (milliseconds since 1970): the places and the view already did (decision 7 of [decisions-reader.md](decisions-reader.md)); the settings got `settings_modified`, and each book entry `modified`. Two copies merge by keeping, for each record, the one changed last; at the same time, the one whose JSON sorts last, so that every device keeps the same copy. Reading book A on the phone and book B on the computer keeps both places (tested).

- **Old state files load:** every new field has a default. A state saved before sync gets the oldest possible time (1) for its settings, if they differ from the defaults, and for its books: they win over a new device's defaults (which are not synced at all until changed), and lose to any change made since.
- **Removing a book removes it everywhere:** it leaves a record of its removal with its time (`removed` in the saved state, `removed: true` in the synced entry), so another device that still has it removes it too, rather than uploading it again; the device that syncs the removal deletes its file from Dropbox. The removal records are kept (a hash and a time each). A book added again later is stamped after its removal, so it comes back everywhere (tested). If the removed book is open in the reader on another device, it is removed there once the reader leaves it.
- **Clock skew:** times are used as given, but a change made on a device is stamped `max(now, the record's time + 1)` (`stamp` in `state.rs`): a change always replaces the record it changes, even one stamped in the future by a device whose clock is ahead, so no record is ever frozen by a future time. What a skewed clock can still do is win a conflict between two changes made without either device seeing the other's, as any newest-wins rule does.

**5. Dropbox.** App folder access only; OAuth 2 with PKCE (S256) and `token_access_type=offline`, with no app secret anywhere. Access tokens (4 hours) are renewed with the refresh token when they have less than 5 minutes left, or when Dropbox refuses one; a refused refresh token (the app revoked) signs the device out, saying so.

- **Web:** the page goes to Dropbox and comes back to its own address (origin and path) with a code; the PKCE verifier and a random `state` wait in the tab's session storage, and the code is exchanged when the page loads, then removed from the address. The refresh token is kept in local storage.
- **Desktop: Dropbox's no-redirect flow**, where the browser shows a code that the user pastes into the app. Rather than a loopback redirect on 127.0.0.1: Dropbox matches redirect URIs exactly, so the port would be fixed and registered, and a busy port would break signing in; the pasted code needs no listening socket, no extra redirect URI, and works whatever the browser and firewall. It costs one paste, once per device.
- **The state file is written only over the revision read** (`mode: update` with the revision, `strict_conflict`), or only if missing (`add`): if another device wrote in between, the sync starts over from reading it, so no update is lost (tested). Book files are written with `overwrite`, since their name is their contents. Single uploads are enough: books are 2–23 MB and Dropbox takes 150 MB in one request (larger files are reported, not uploaded); upload sessions are not needed.
- **CORS** was checked against Dropbox's servers (preflight requests, with no account): `content.dropboxapi.com` allows the `Authorization`, `Content-Type` and `Dropbox-API-Arg` headers and exposes `Dropbox-API-Result` (which carries the downloaded file's revision); `api.dropboxapi.com` and the token endpoint allow `Authorization` and `Content-Type`. So the headers are used, not the `arg` query parameter. `Dropbox-API-Arg` escapes anything outside ASCII.
- **HTTP:** `ehttp` on both, as it also works natively (with `ureq`, on a thread per request), so the code is the same.
- **The app key** is read at compile time from `JPFE_DROPBOX_APP_KEY` (`option_env!`); without it, the settings say sync is not available in this build. The Pages workflow passes the repository variable `DROPBOX_APP_KEY`.

**6. When it syncs:** at startup once signed in (a full sync); 3 s after the last local change (a page turn, a setting, a book added or removed), pushed directly against the revision of the last sync; when the page is hidden or closed (`visibilitychange` and `pagehide`), from the event handler itself since a hidden page draws no more frames, with the records given at the last change (best effort: the browser may cancel the request when the page closes; what was not pushed is pushed at the next start, and keeps its time); every minute, a cheap `list_folder/continue` check, which leads to a full sync only if something changed; every 10 minutes, a full sync anyway. After an error, it retries after 15 s, doubling up to 10 minutes; **Retry** retries at once. Errors never block the app: everything is saved locally first, and sync only reads and writes on the side.

- **Books from other devices** download in the background, one at a time, after the records: each is checked against its hash, stored, and analyzed like an opened file (the sidebar shows "Downloading … from Dropbox…"). A book listed by another device whose file is not uploaded yet waits ("waiting for another device to upload").
- **A book being read never jumps.** A newer place from another device applies at once to books not showing in the reader (and to a book left in the reader behind the statistics, which then reopens at that place). For the book showing, the reader stays where it is, and a line under its bar offers it: "Newer place from another device: chapter … — Go there / Stay here". Turning a page, or *Stay here*, keeps this device's place, stamped after the offered one, so it is synced as the newest; leaving the reader takes the offered place, so the book opens there next time. Asking rather than applying, because the user may be reading on both, and a page that changes under the eyes loses their place; applying on leaving, because the user then comes back to the latest place. The same holds for the view: another device's view is taken only by the first sync after startup, before this device has shown the reader or switched views, so the app can reopen where the user last was, on any device, without ever switching views under them.

**7. The UI:** a "Dropbox sync" section in the settings window (statistics or reader), above *Show debug information*: *Sign in to Dropbox* when signed out (with why, after an error or a revoked sign-in); when signed in, the status line (syncing, "Synced 2 min ago", or the error), the books waiting to upload or download, *Sync now* (*Retry* after an error) and *Sign out*. It fits a phone screen (checked at 375 points wide).

**Tested:** unit tests for the merge (newest wins per record, the same copy on a tie whichever way round, removals against older and newer entries, places of removed books dropped), old state files loading, the local id to hash mapping and back, the reader-open rule, and a future timestamp; and two devices syncing through an in-memory provider (a place on each, a book added on one and removed on the other then added again, conflicting settings, a write in between, the offered place, an offline provider retried, the cheap check). In the browser, without a Dropbox key: the app works with sync shown as not available, an old state with a book and a place migrates (the book gets the hash of its file, the place and settings are kept, the reader reopens there), and a page turn and a setting change are stamped. With a dummy key: the signed-out section on a phone, a sign-in coming back with a wrong `state` refused (and removed from the address), and a refused refresh token signing the device out with the app working on.

**8. Tried with the real Dropbox (21 November 2026, commits "Sync: say what differs when a downloaded book is damaged" and "Stored files written exactly once; damaged copies never synced").** The user created the Dropbox app and signed in from Safari and Chrome on `localhost:8769`: sign-in, settings and places synced at once, but Chrome refused the book Safari had uploaded ("the downloaded file is damaged"). The error now says what differs (expected and received SHA-256 and size, the HTTP status and type, Dropbox's size and content hash), and it showed 14,714,586 bytes for a 7,357,293-byte PDF: exactly twice, intact in transit, so stored twice over by Safari.

- **The cause, in the storage, not the sync:** files are written to the browser's storage in 4 MB parts, and each part was given to the writable stream as a view (`subarray`) of the whole file; Safari wrote each view's whole buffer, so a file of two parts was stored twice (three parts, three times). Safari only recently gained `createWritable` outside workers, which is why the worker path, used before, never did it, and Chromium writes views correctly. Each part is now a copy (`slice`), every write checks the stored size and writes again through the worker if it differs, and the worker truncates to the exact length. **This also fixes main:** its stored books and analyses over 4 MB written by Safari were affected the same way (the analyses' format hid it, and a book is only read again when a new build analyzes it).
- **Never syncing a damaged copy:** a book's stored file is checked against its hash before it is uploaded; a damaged one is reported ("the copy stored in this browser is damaged … open the file again to repair it"), and opening the file again replaces it. A download that differs from what Dropbox stores is an error, retried; a file in Dropbox that is not the book its name says is deleted by the device that downloads it, so that a device with the right file uploads it again.
- After the fix, the user repaired Safari's copy, and books downloaded between Safari and Chrome.

**Not tried yet:** the desktop app's sign-in (the pasted code) and sync.

### Still open

- **The desktop app with the real Dropbox** (see [dropbox-setup.md](dropbox-setup.md)): its sign-in with a pasted code, and syncing with the web.
- **Desktop close:** the desktop app does not push on quitting; a change made in the last 3 s is pushed at the next start.
- **Removal records are kept forever** (a few dozen bytes each); they could be dropped after a year.
- **CloudKit**, for an iOS app, as a second provider (see above).

## Word highlighting in the reader

Built: its decisions (the parts and their colors, the corner rule) are now decision 8 of [decisions-reader.md](decisions-reader.md), with what is left open (names UniDic does not know, which show as plain nouns; okurigana shading; the default and the colors, not reviewed by the user yet).

### Survey: colors against the dictionary window (19 November 2026)

The user saw the highlight and the dictionary window disagree (家 and 心 colored as suffixes, 伊達 as a name). A survey of the six books compared each word's color with the part of speech of the JMdict entry the window showed: 92.4% agreed, 3.2% had no entry, 4.4% disagreed (UniDic's nouns that are な-adjectives or adverbs in JMdict, grammar shown in another sense, suffixes standing alone, common words tagged as names). It also found two tokenizer bugs, which affected word lists too, both fixed (commit "Read the word after a full-width space, and each word in its sentence": decisions 18 and 19 of [word-grouping.md](word-grouping.md)):

- **Paragraph indents** (fixed: every non-ASCII space reaches UniDic as ASCII spaces, also inside a paragraph, after ！ and ？). UniDic read the full-width space starting a paragraph (418,384 of them) as a noun, and the word after it became a suffix with a wrong reading: 　心は → 心【しん】, 　家で → 家【か】, 　人を → 人【にん】 (8,046 words).
- **Readings cached by dictionary form** (fixed: each occurrence takes UniDic's reading in its sentence; the kana spelling, homograph and じる rules are kept). One misreading stuck to later occurrences: 1.81% of single-token words got another reading than UniDic gives in context (心 しん for こころ 3,886 times). The window picks its entry by this reading, so it showed the wrong entry too.

The rule the user then chose, and its results, are decision 9 of [decisions-reader.md](decisions-reader.md).

## Next investigation (after the Dropbox sync and the reader's top-bar change are merged)

The user found these issues on the first page of chapter 0 (the prologue) of 無職転生, on 20 November 2026. The first two show in both the PDF and the EPUB; the third only in the EPUB:

- **A に shown as だ in the dictionary window.** In 逆にぼっこぼこにされた, clicking the に of 逆に opens the entry だ ("as used here: auxiliary", then た and 兊【だ】). UniDic likely tags this に as the continuative form of the copula だ, so the window looks up its dictionary form. Find whether the tokenization is right, and whether the window should show the form as written (に, "continuative of だ") rather than だ.
- **Line breaks after 、 that look inconsistent.** Some columns end right after a comma (兄弟たちの奸計にハマり、 / 見事に家を追い出された。, 床ドンと壁ドンをマスターし、 / 家で傍若無人に…), others run on. Find whether the EPUB itself breaks these lines (the prologue's layout), or whether the extraction or the reader joins or splits sentences and whitespace wrongly.
- **Spacing in the EPUB much larger than in the PDF.** In the EPUB, the same passage shows an empty column between each line (兄弟たちの…追い出された。 / empty / 床ドンと壁ドンをマスターし、 / empty / 家で傍若無人に…), and a blank cell after ハマり、 before 見事に (likely a full-width space, now given to UniDic as ASCII spaces but still drawn as a cell). Find what the EPUB's markup holds there (empty paragraphs, `<br/>`, spaces) and how the EPUB extraction turns it into lines, then decide how the reader should show it: e.g. collapse empty lines, or treat a full-width space after 、 as a line's continuation.
