//! The dictionary card shown next to a looked-up word (a double-click, or the selection bar's
//! Dictionary button): the user's glossary entry, the Wiktionary definition, and buttons to save
//! the word to the glossary or open it on Dicio.

use std::sync::{Arc, Mutex};

use egui::{Rect, pos2};

use crate::dictionary::{self, Definition};

const WIDTH: f32 = 340.0;
/// Room kept for the card when deciding whether it fits below the word.
const ROOM: f32 = 320.0;

/// A word being looked up, and the card's state.
pub(crate) struct Lookup {
    word: String,
    /// The looked-up word's box on screen.
    word_rect: Rect,
    definition: Arc<Mutex<Definition>>,
    draft: Option<Draft>,
    /// Opened this frame: the click that opened it doesn't count as a click elsewhere.
    fresh: bool,
    /// My glossary was opened from the card, which then makes way for it.
    closing: bool,
}

/// The glossary form: a new entry, or an edit of `editing`.
struct Draft {
    term: String,
    definition: String,
    editing: Option<String>,
    error: Option<&'static str>,
}

impl Lookup {
    pub(crate) fn new(ctx: &egui::Context, word: String, word_rect: Rect) -> Self {
        let definition = dictionary::definition(ctx, &word);
        Self { word, word_rect, definition, draft: None, fresh: true, closing: false }
    }
}

/// What the card asks the view to do once drawn.
pub(crate) enum CardResult {
    Keep,
    Close,
}

/// Draws the card for `lookup` inside `viewport`.
pub(crate) fn show(ui: &mut egui::Ui, viewport: Rect, lookup: &mut Lookup) -> CardResult {
    let ctx = ui.ctx().clone();
    let x = lookup.word_rect.left().clamp(viewport.left() + 8.0, (viewport.right() - WIDTH - 24.0).max(viewport.left() + 8.0));
    let below = lookup.word_rect.bottom() + 6.0;
    let y = if below + ROOM < viewport.bottom() { below } else { (lookup.word_rect.top() - ROOM - 6.0).max(viewport.top() + 6.0) };
    let mut close = false;
    let area = egui::Area::new(egui::Id::unique("dictionary-card")).order(egui::Order::Foreground).fixed_pos(pos2(x, y)).show(&ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_width(WIDTH);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&lookup.word).strong().size(16.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::icons::button(ui, "x", 22.0, false, tl!("Close")).clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            if lookup.draft.is_some() {
                draft_form(ui, &mut lookup.draft);
            } else {
                card_body(ui, lookup);
            }
        });
    });
    let clicked_elsewhere = !std::mem::take(&mut lookup.fresh) && area.response.clicked_elsewhere();
    if close || lookup.closing || clicked_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        // Escape while typing in the form only leaves the form.
        if lookup.draft.is_some() && !close && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            lookup.draft = None;
            return CardResult::Keep;
        }
        return CardResult::Close;
    }
    CardResult::Keep
}

fn card_body(ui: &mut egui::Ui, lookup: &mut Lookup) {
    let entry = dictionary::glossary().lock().ok().and_then(|g| g.find(&lookup.word).cloned());
    let definition = lookup.definition.lock().map(|d| d.clone()).unwrap_or(Definition::Failed);
    egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(ui, |ui| {
        if let Some(entry) = &entry {
            egui::Frame::group(ui.style()).fill(ui.visuals().selection.bg_fill.gamma_multiply(0.25)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mut title = tl!("My glossary").to_uppercase();
                    if dictionary::fold(&entry.term) != dictionary::fold(&lookup.word) {
                        title = format!("{title} · {}", entry.term);
                    }
                    ui.label(egui::RichText::new(title).small().strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button(tl!("Edit")).clicked() {
                            lookup.draft = Some(Draft {
                                term: entry.term.clone(),
                                definition: entry.definition.clone(),
                                editing: Some(entry.term.clone()),
                                error: None,
                            });
                        }
                    });
                });
                ui.add(egui::Label::new(&entry.definition).wrap());
            });
            ui.add_space(6.0);
        }
        match &definition {
            Definition::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Looking up…"));
                });
            }
            Definition::NotFound => {
                ui.label(egui::RichText::new(tl!("No definition found in Wiktionary. Try Dicio.")).weak());
            }
            Definition::Failed => {
                ui.label(egui::RichText::new(tl!("Couldn't reach the dictionary. Check the connection or try Dicio.")).weak());
            }
            Definition::Found { sections, .. } => {
                for section in sections {
                    ui.label(egui::RichText::new(section.heading.to_uppercase()).small().strong().weak());
                    for line in &section.lines {
                        ui.add(egui::Label::new(line).wrap());
                    }
                    ui.add_space(4.0);
                }
            }
        }
        if let Definition::Found { title, .. } = &definition
            && ui.link(egui::RichText::new(tl!("Source: Wiktionary")).small()).clicked()
        {
            ui.ctx().open_url(egui::OpenUrl::new_tab(dictionary::wiktionary_url(title)));
        }
    });
    ui.separator();
    ui.horizontal(|ui| {
        if entry.is_none() && ui.button(tl!("Save to glossary")).clicked() {
            let suggested = match &definition {
                Definition::Found { sections, .. } => dictionary::suggested_definition(sections),
                _ => String::new(),
            };
            lookup.draft = Some(Draft { term: lookup.word.to_lowercase(), definition: suggested, editing: None, error: None });
        }
        if ui.link(egui::RichText::new(tl!("My glossary…")).small()).clicked() {
            open_manager();
            lookup.closing = true;
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(tl!("Open in Dicio")).clicked() {
                ui.ctx().open_url(egui::OpenUrl::new_tab(dictionary::dicio_url(&lookup.word)));
            }
        });
    });
}

fn draft_form(ui: &mut egui::Ui, slot: &mut Option<Draft>) {
    let Some(draft) = slot.as_mut() else { return };
    ui.label(egui::RichText::new(tl!("Term")).small().strong());
    ui.add(egui::TextEdit::singleline(&mut draft.term).desired_width(f32::INFINITY));
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Meaning")).small().strong());
    let edit = ui.add(
        egui::TextEdit::multiline(&mut draft.definition)
            .desired_rows(4)
            .desired_width(f32::INFINITY)
            .hint_text(tl!("Write the meaning in your own words…")),
    );
    if edit.gained_focus() {
        draft.error = None;
    }
    if let Some(error) = draft.error {
        ui.colored_label(egui::Color32::from_rgb(0xD1, 0x3B, 0x3B), tl!(error));
    }
    ui.add_space(6.0);
    let mut done = false;
    ui.horizontal(|ui| {
        if let Some(editing) = draft.editing.clone()
            && ui.button(tl!("Delete")).clicked()
        {
            match dictionary::glossary().lock() {
                Ok(mut g) => match g.remove(&editing) {
                    Ok(()) => done = true,
                    Err(_) => draft.error = Some("Couldn't save the glossary."),
                },
                Err(_) => draft.error = Some("Couldn't save the glossary."),
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(tl!("Save")).clicked() {
                draft.error = save_draft(draft);
                done = draft.error.is_none();
            }
            if ui.button(tl!("Cancel")).clicked() {
                done = true;
            }
        });
    });
    if done {
        *slot = None;
    }
}

/// Saves the form; the message to show when it can't.
fn save_draft(draft: &Draft) -> Option<&'static str> {
    if draft.term.trim().is_empty() || draft.definition.trim().is_empty() {
        return Some("Fill in the term and the meaning.");
    }
    let Ok(mut g) = dictionary::glossary().lock() else { return Some("Couldn't save the glossary.") };
    // A new entry, or a rename, onto a term that already exists would replace it silently.
    let renaming_onto_other = g
        .get(&draft.term)
        .is_some_and(|existing| draft.editing.as_deref().is_none_or(|editing| dictionary::fold(editing) != dictionary::fold(&existing.term)));
    if renaming_onto_other {
        return Some("This term is already in your glossary. Edit it instead.");
    }
    match g.upsert(&draft.term, &draft.definition, draft.editing.as_deref()) {
        Ok(()) => None,
        Err(_) => Some("Couldn't save the glossary."),
    }
}

// ---------------------------------------------------------------------------------------------
// My glossary: the whole list, with search, editing, export and import
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Manager {
    open: bool,
    query: String,
    draft: Option<Draft>,
    message: Option<String>,
}

fn manager() -> &'static Mutex<Manager> {
    static MANAGER: std::sync::OnceLock<Mutex<Manager>> = std::sync::OnceLock::new();
    MANAGER.get_or_init(Default::default)
}

pub(crate) fn open_manager() {
    if let Ok(mut m) = manager().lock() {
        m.open = true;
        m.message = None;
    }
}

/// Draws the My glossary window while it is open (each frame).
pub(crate) fn show_manager(ctx: &egui::Context) {
    let Ok(mut m) = manager().lock() else { return };
    if !m.open {
        return;
    }
    let mut open = true;
    egui::Window::new(tl!("My glossary"))
        .id(egui::Id::unique("glossary-manager"))
        .open(&mut open)
        .default_size([480.0, 520.0])
        .collapsible(false)
        .show(ctx, |ui| manager_body(ui, &mut m));
    m.open = open;
}

fn manager_body(ui: &mut egui::Ui, m: &mut Manager) {
    if m.draft.is_some() {
        draft_form(ui, &mut m.draft);
        return;
    }
    let entries: Vec<dictionary::Entry> = dictionary::glossary().lock().map(|g| g.entries().to_vec()).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut m.query).hint_text(tl!("Search term or meaning")).desired_width(200.0));
        if ui.button(tl!("New term")).clicked() {
            m.draft = Some(Draft { term: m.query.trim().to_string(), definition: String::new(), editing: None, error: None });
            m.message = None;
        }
        if ui.add_enabled(!entries.is_empty(), egui::Button::new(tl!("Export…"))).clicked() {
            m.message = export();
        }
        if ui.button(tl!("Import…")).clicked() {
            m.message = import();
        }
    });
    if let Some(message) = &m.message {
        ui.label(egui::RichText::new(message).small());
    }
    ui.label(
        egui::RichText::new(format!("{} · {}", terms(entries.len()), tl!("saved on this computer only; export now and then as a backup.")))
            .small()
            .weak(),
    );
    ui.separator();
    let key = dictionary::fold(&m.query);
    let shown: Vec<&dictionary::Entry> = entries
        .iter()
        .filter(|e| key.is_empty() || dictionary::fold(&e.term).contains(&key) || dictionary::fold(&e.definition).contains(&key))
        .collect();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if shown.is_empty() {
            let empty = if entries.is_empty() {
                tl!("Your glossary is empty. Double-click a word in a PDF and choose Save to glossary.")
            } else {
                tl!("No terms match the search.")
            };
            ui.label(egui::RichText::new(empty).weak());
        }
        for entry in shown {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&entry.term).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button(tl!("Edit")).clicked() {
                        m.draft = Some(Draft {
                            term: entry.term.clone(),
                            definition: entry.definition.clone(),
                            editing: Some(entry.term.clone()),
                            error: None,
                        });
                        m.message = None;
                    }
                });
            });
            ui.add(egui::Label::new(egui::RichText::new(&entry.definition).weak()).wrap());
            ui.separator();
        }
    });
}

#[cfg(not(target_arch = "wasm32"))]
fn export() -> Option<String> {
    let path = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_file_name("glossario.json").save_file()?;
    let (text, count) = dictionary::glossary().lock().map(|g| (g.export(), g.entries().len())).ok()?;
    Some(match std::fs::write(&path, text) {
        Ok(()) => plural(count, "{n} term exported.", "{n} terms exported."),
        Err(_) => tl!("Couldn't write the file.").to_string(),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn import() -> Option<String> {
    let path = rfd::FileDialog::new().add_filter("JSON", &["json"]).pick_file()?;
    let text = std::fs::read_to_string(&path).ok();
    let result = text.ok_or(()).and_then(|t| dictionary::glossary().lock().map_err(|_| ()).and_then(|mut g| g.import(&t).map_err(|_| ())));
    Some(match result {
        Ok(n) => plural(n, "{n} term imported.", "{n} terms imported."),
        Err(()) => tl!("This file isn't a valid glossary.").to_string(),
    })
}

#[cfg(target_arch = "wasm32")]
fn export() -> Option<String> {
    None
}

#[cfg(target_arch = "wasm32")]
fn import() -> Option<String> {
    None
}

/// `one` or `other` for `n`, in the current language.
fn plural(n: usize, one: &str, other: &str) -> String {
    crate::i18n::trn(crate::i18n::current(), u64::try_from(n).unwrap_or(u64::MAX), one, other)
}

/// "1 term" / "3 terms".
fn terms(n: usize) -> String {
    plural(n, "{n} term", "{n} terms")
}
