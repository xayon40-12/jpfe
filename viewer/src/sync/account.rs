//! Signing in to Dropbox and out, and the "Dropbox sync" section of the settings.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use egui::{RichText, Ui};

use super::dropbox::{self, Dropbox, Grant, APP_KEY};
use super::engine::{Engine, Status};
use crate::platform::{Inbox, Incoming};
use crate::theme::{self, Palette};

pub enum Account {
    /// No Dropbox app key in this build
    Unavailable,
    SignedOut { message: Option<String> },
    /// Desktop: waiting for the code Dropbox shows in the browser
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    Pasting { verifier: String, code: String },
    /// Exchanging the code for the tokens
    Connecting,
    SignedIn { engine: Rc<RefCell<Engine>>, dropbox: Dropbox },
}

thread_local! {
    /// The engine signed in, for [`flush_now`]
    static ACTIVE: RefCell<Weak<RefCell<Engine>>> = const { RefCell::new(Weak::new()) };
}

/// Push what waits to be synced right away (the page is being hidden or closed).
pub fn flush_now() {
    if let Some(engine) = ACTIVE.with_borrow(Weak::upgrade) {
        if let Ok(mut engine) = engine.try_borrow_mut() {
            engine.flush_now();
        }
    }
}

impl Account {
    /// Signed in if a refresh token is kept on this device; on the web, also finishes a sign-in when Dropbox
    /// sent the user back to the page.
    pub fn new(inbox: &Inbox, ctx: &egui::Context) -> Self {
        let Some(key) = APP_KEY else { return Account::Unavailable };
        #[cfg(target_arch = "wasm32")]
        {
            let (inbox, ctx2) = (inbox.clone(), ctx.clone());
            let finishing = dropbox::web::finish(key, move |result| {
                inbox.push(Incoming::SyncSignIn(result));
                ctx2.request_repaint();
            });
            if finishing.is_some() {
                return Account::Connecting;
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        let _ = inbox;
        match dropbox::load_token() {
            Some(token) => Self::signed_in(key, Grant::stored(token), ctx),
            None => Account::SignedOut { message: None },
        }
    }

    fn signed_in(key: &str, grant: Grant, ctx: &egui::Context) -> Self {
        let dropbox = Dropbox::new(key, grant);
        let ctx = ctx.clone();
        let mut engine = Engine::new(Box::new(dropbox.clone()), move || ctx.request_repaint());
        engine.sync_now();
        let engine = Rc::new(RefCell::new(engine));
        ACTIVE.set(Rc::downgrade(&engine));
        Account::SignedIn { engine, dropbox }
    }

    /// The answer of a sign-in.
    pub fn finish_sign_in(&mut self, result: Result<Grant, String>, ctx: &egui::Context) {
        let Some(key) = APP_KEY else { return };
        *self = match result {
            Ok(grant) => match dropbox::save_token(Some(&grant.refresh)) {
                Ok(()) => Self::signed_in(key, grant, ctx),
                Err(e) => Account::SignedOut { message: Some(format!("Could not keep the sign-in on this device: {e}")) },
            },
            Err(e) => Account::SignedOut { message: Some(e) },
        };
    }

    pub fn engine(&self) -> Option<Rc<RefCell<Engine>>> {
        match self {
            Account::SignedIn { engine, .. } => Some(engine.clone()),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<Status> {
        match self {
            Account::SignedIn { engine, .. } => Some(engine.borrow().status().clone()),
            _ => None,
        }
    }

    /// Forget the sign-in on this device (and revoke it unless Dropbox already did).
    fn sign_out(&mut self, message: Option<String>) {
        if let Account::SignedIn { dropbox, engine } = self {
            if !engine.borrow().status().signed_out {
                dropbox.revoke();
            }
        }
        let message = match dropbox::save_token(None) {
            Err(e) => Some(format!("Could not forget the sign-in: {e}")),
            Ok(()) => message,
        };
        *self = Account::SignedOut { message };
    }

    /// Signed out by Dropbox (the access was revoked): back to signed out, saying so.
    pub fn check_revoked(&mut self) {
        if self.status().is_some_and(|s| s.signed_out) {
            let reason = self.status().and_then(|s| s.error).unwrap_or_default();
            self.sign_out(Some(format!("Dropbox signed this device out ({reason}); sign in again.")));
        }
    }

    /// The "Dropbox sync" section of the settings.
    pub fn ui(&mut self, ui: &mut Ui, palette: Palette, inbox: &Inbox) {
        let note = |ui: &mut Ui, text: &str| {
            ui.label(RichText::new(text).small().color(palette.secondary_text));
        };
        ui.label("Dropbox sync");
        let Some(key) = APP_KEY else {
            note(ui, "Not available in this build: it needs a Dropbox app key (see docs/dropbox-setup.md).");
            return;
        };
        let mut action = None;
        match self {
            Account::Unavailable => {}
            Account::SignedOut { message } => {
                note(ui, "Your books, places and settings on all your devices, kept in a folder of your Dropbox only this app can see.");
                if let Some(message) = message {
                    ui.label(RichText::new(message.as_str()).small().color(ui.visuals().error_fg_color));
                }
                if ui.add(theme::primary_button("Sign in to Dropbox", palette)).clicked() {
                    action = Some(Action::SignIn);
                }
            }
            Account::Pasting { code, .. } => {
                note(ui, "Allow access in the page that opened in your browser, then paste the code Dropbox shows:");
                ui.add(egui::TextEdit::singleline(code).hint_text("Code").desired_width(ui.available_width().min(320.0)));
                ui.horizontal(|ui| {
                    let ready = !code.trim().is_empty();
                    if ui.add_enabled(ready, theme::primary_button("Connect", palette)).clicked() {
                        action = Some(Action::Connect);
                    }
                    if ui.button("Cancel").clicked() {
                        action = Some(Action::Cancel);
                    }
                });
            }
            Account::Connecting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    note(ui, "Connecting to Dropbox…");
                });
            }
            Account::SignedIn { engine, .. } => {
                let status = engine.borrow().status().clone();
                ui.horizontal_wrapped(|ui| {
                    if status.busy {
                        ui.spinner();
                        note(ui, "Syncing…");
                    } else if let Some(error) = &status.error {
                        ui.label(RichText::new(format!("Not synced: {error}")).small().color(ui.visuals().error_fg_color));
                    } else if let Some(at) = status.last_synced {
                        note(ui, &format!("Synced {}", ago(at)));
                    } else {
                        note(ui, "Not synced yet");
                    }
                });
                let mut books = Vec::new();
                if status.uploads > 0 {
                    books.push(format!("{} to upload", status.uploads));
                }
                if status.downloads > 0 {
                    books.push(format!("{} to download", status.downloads));
                }
                if status.waiting > 0 {
                    books.push(format!("{} waiting for another device to upload", status.waiting));
                }
                if !books.is_empty() {
                    note(ui, &format!("Books: {}", books.join(", ")));
                }
                for problem in &status.problems {
                    note(ui, problem);
                }
                ui.horizontal(|ui| {
                    let label = if status.error.is_some() { "Retry" } else { "Sync now" };
                    if ui.add_enabled(!status.busy, egui::Button::new(label)).clicked() {
                        engine.borrow_mut().sync_now();
                    }
                    if ui.button("Sign out").clicked() {
                        action = Some(Action::SignOut);
                    }
                });
            }
        }
        match action {
            Some(Action::SignIn) => {
                #[cfg(target_arch = "wasm32")]
                if let Err(e) = dropbox::web::start(key) {
                    *self = Account::SignedOut { message: Some(e) };
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let sign_in = dropbox::sign_in(key, None);
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&sign_in.url));
                    *self = Account::Pasting { verifier: sign_in.verifier, code: String::new() };
                }
            }
            Some(Action::Connect) => {
                if let Account::Pasting { verifier, code } = self {
                    let (inbox, ctx) = (inbox.clone(), ui.ctx().clone());
                    dropbox::exchange(key, code, verifier, None, move |result| {
                        inbox.push(Incoming::SyncSignIn(result));
                        ctx.request_repaint();
                    });
                    *self = Account::Connecting;
                }
            }
            Some(Action::Cancel) => *self = Account::SignedOut { message: None },
            Some(Action::SignOut) => self.sign_out(None),
            None => {}
        }
    }
}

enum Action {
    SignIn,
    Connect,
    Cancel,
    SignOut,
}

/// "just now", "5 min ago", "3 h ago".
fn ago(at: u64) -> String {
    let seconds = crate::state::now_millis().saturating_sub(at) / 1000;
    match seconds {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", seconds / 60),
        3600..86400 => format!("{} h ago", seconds / 3600),
        _ => format!("{} days ago", seconds / 86400),
    }
}
