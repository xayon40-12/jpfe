# Cloud sync and word highlighting: decisions and plan

This document records two features decided on 18 November 2026, before any code was written: **syncing the viewer through the cloud** and **highlighting the parts of every word in the reader**. It covers what was decided and why, what was rejected, and the order of the work. When the work lands, its decisions move into their commits' history here (or into [decisions-reader.md](decisions-reader.md) for the reader), as the other documents do.

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

### Design to follow

- **Hide the provider behind a small interface:** read, write and list files by name, plus "what changed since last time". Dropbox is its first implementation, and CloudKit could be a second one.
- **Books need an id from their content,** such as a SHA-256 hash of the original file. Today's ids are counters local to each device (`next_id` in `SavedState`, `viewer/src/app.rs`), so the same book has different ids on two devices.
- **What is synced:** the settings, each book's reader state (`BookReaderState` in `viewer/src/reader.rs`), the view state from step 1, and the original book files.
- **What is not synced:** the analyses, which each device rebuilds (they are cached by a hash of the code anyway), and the dictionary.
- **Merging:** each synced record carries a `modified` timestamp, and two copies merge record by record, keeping the newest. Places merge book by book, so reading book A on the phone and book B on the computer keeps both.

### Open

- Where the sign-in button goes, and how sync status and errors are shown.
- What happens when a book is removed on one device: remove it everywhere, or only locally.
- How a book's local id maps to its content hash in states saved before sync.

## Word highlighting in the reader

Built: its decisions (the parts and their colors, the corner rule) are now decision 8 of [decisions-reader.md](decisions-reader.md), with what is left open (names UniDic does not know, which show as plain nouns; okurigana shading; the default and the colors, not reviewed by the user yet).

### Survey: colors against the dictionary window (19 November 2026)

The user saw the highlight and the dictionary window disagree (家 and 心 colored as suffixes, 伊達 as a name). A survey of the six books compared each word's color with the part of speech of the JMdict entry the window showed: 92.4% agreed, 3.2% had no entry, 4.4% disagreed (UniDic's nouns that are な-adjectives or adverbs in JMdict, grammar shown in another sense, suffixes standing alone, common words tagged as names). It also found two tokenizer bugs, fixed on another branch: the full-width space indenting a paragraph read as a noun (the word after it becomes a misread suffix), and readings cached by dictionary form. The rule the user then chose, and its results, are decision 9 of [decisions-reader.md](decisions-reader.md).
