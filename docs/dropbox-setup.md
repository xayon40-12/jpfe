# Setting up Dropbox sync

The viewer syncs through the user's own Dropbox, in a folder only this app can see (see "Cloud sync" in [plans-sync-and-highlighting.md](plans-sync-and-highlighting.md)). For that, it needs a **Dropbox app**: an entry in Dropbox's App Console that users sign in to. Its **app key** is a public value (the app signs in with OAuth and PKCE, so there is no secret anywhere), set when building the viewer. Without a key, the build shows "Dropbox sync: not available in this build" in the settings, and everything else works as before.

Nobody but the owner of the repository can create this app: it belongs to their Dropbox account.

## 1. Create the app

1. Sign in at <https://www.dropbox.com/developers/apps> and click **Create app**.
2. **Choose an API:** *Scoped access*.
3. **Choose the type of access:** *App folder* (the app sees only its own folder, `Apps/<app name>/`, not the rest of the Dropbox).
4. **Name your app:** any name not taken yet, for example `jpfe-sync`. It is the folder's name in the Dropbox, and what users see when they sign in.
5. Click **Create app**.

## 2. Permissions

In the app's **Permissions** tab, tick:

- `files.metadata.read` (listing the folder, and checking for changes),
- `files.content.read` (downloading the state and the books),
- `files.content.write` (uploading them, and deleting removed books),

then click **Submit** at the bottom. Do this before anyone signs in: a sign-in only grants the permissions ticked at that time (after changing them, sign out and in again in the viewer).

## 3. Redirect URIs

In the **Settings** tab, under **OAuth 2**:

- **Allow public clients (Implicit Grant & PKCE):** *Allow* (the default). The viewer uses PKCE, never the implicit grant.
- **Redirect URIs:** add each address the web viewer is opened at, exactly, with the final slash:
  - `http://localhost:8766/`, for testing locally (see below);
  - `https://xayon40-12.github.io/jpfe/`, the GitHub Pages site (the repository `xayon40-12/jpfe`, published by `.github/workflows/pages.yml`).

The desktop app needs no redirect URI: Dropbox shows it a code to paste instead (its "no redirect" flow).

The **App secret** on the same page is not used. Never put it in the code, a build or a variable.

## 4. The app key

The **App key** is on the **Settings** tab. The viewer reads it when it is compiled, from the `JPFE_DROPBOX_APP_KEY` environment variable.

**Locally:**

```sh
# Web, at http://localhost:8766/ (the redirect URI above; 127.0.0.1 would not match)
cd viewer && JPFE_DROPBOX_APP_KEY=<app key> trunk serve --release --port 8766

# Desktop
JPFE_DROPBOX_APP_KEY=<app key> cargo run --release -p jpfe-viewer
```

**On GitHub Pages:** in the repository's **Settings → Secrets and variables → Actions → Variables**, add a **repository variable** (not a secret: the key is public, and ends up in the page anyway) named `DROPBOX_APP_KEY` with the app key. The workflow passes it to `trunk build`. Then run the workflow (**Actions → Deploy viewer to GitHub Pages → Run workflow**) or push to `main`.

## 5. Signing in

- **Web:** **⚙ Settings → Dropbox sync → Sign in to Dropbox**. The page goes to Dropbox, which asks to allow the app, and comes back to the viewer, signed in.
- **Desktop:** the same button opens the browser on Dropbox's page; after allowing the app, Dropbox shows a code. Paste it into the field the viewer shows, and click **Connect**.

The first sync then uploads the books, places and settings, and every other device signed in to the same Dropbox account downloads them. The section shows the last sync, errors (with **Retry**), and the books waiting to be uploaded or downloaded. **Sign out** forgets the sign-in on that device and revokes it; nothing synced is deleted. A user can also revoke the app in their Dropbox account's **Settings → Connected apps**, which signs every device out.

The sign-in is kept on the device, apart from the synced state: in the browser's local storage for the site on the web, and in `dropbox-token` in the data folder (e.g. `~/Library/Application Support/jpfe-viewer/`) on the desktop. Clearing the site's data signs the browser out.

## Development status and other users

A new app is in **Development** status. Its owner can use it right away. For other Dropbox accounts to sign in, click **Enable additional users** on the **Settings** tab (Development apps allow up to 500 users); once 50 users have linked it, Dropbox gives two weeks to **Apply for production**, after which it reviews the app. For the owner and a few people, Development status is enough.

## What is in the folder

`Apps/<app name>/` in the Dropbox holds:

- `state.json`: the synced records (settings, each book's entry or the record of its removal, each book's place, the view), readable JSON;
- `books/<sha256>.<epub|pdf>`: each book's original file, named by the SHA-256 of its contents.

Deleting the folder's contents starts over: the next sync uploads each device's books and records again.
