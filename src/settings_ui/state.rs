// What the Settings window edits and how it saves, without the window: the
// config being edited, the API keys, the autostart flag, the snapshot they are
// compared against for "unsaved changes", and the save that writes them out.
// The panes bind to a `Form`'s fields and ask it whether Save is on; nothing
// here knows about egui, so it is asserted directly.

use crate::config::{ChatBackend, Config, Provider};
use crate::llm::{self, Chat};
use crate::secrets::{self, KeySlot, KeySource, KeyWrite};
use crate::transcribe::vocabulary;

/// Picker order for the Provider dropdown, and the set of Providers the window
/// knows. Which of them take an API key is `secrets::slot_name`'s to say.
pub(super) const ALL_PROVIDERS: &[Provider] = &[
    Provider::LocalParakeet,
    Provider::Mistral,
    Provider::Groq,
    Provider::Openai,
    Provider::Elevenlabs,
    Provider::Reson8,
];

/// One API key per keyring slot: each Provider that takes one — those
/// `secrets::slot_name` gives a slot — so a new Provider in `ALL_PROVIDERS`
/// gets its key here without another edit, then the Cerebras Chat backend's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Keys(Vec<(KeySlot, String)>);

impl Default for Keys {
    fn default() -> Self {
        Keys(
            ALL_PROVIDERS
                .iter()
                .map(|&p| KeySlot::from(p))
                .chain([KeySlot::Cerebras])
                .filter(|&s| secrets::slot_name(s).is_some())
                .map(|s| (s, String::new()))
                .collect(),
        )
    }
}

impl Keys {
    /// The slots: the Providers' in picker order, then Cerebras.
    pub fn slots(&self) -> impl Iterator<Item = KeySlot> + '_ {
        self.0.iter().map(|(s, _)| *s)
    }

    /// The key in `slot`; empty for none, and for a Provider that takes none.
    pub fn get(&self, slot: impl Into<KeySlot>) -> &str {
        let slot = slot.into();
        self.0
            .iter()
            .find(|(s, _)| *s == slot)
            .map_or("", |(_, key)| key)
    }

    /// Whether `slot` holds a key.
    pub fn has(&self, slot: impl Into<KeySlot>) -> bool {
        !self.get(slot).trim().is_empty()
    }

    /// Ignored for a Provider that takes no key.
    pub fn set(&mut self, slot: impl Into<KeySlot>, key: String) {
        let slot = slot.into();
        if let Some((_, k)) = self.0.iter_mut().find(|(s, _)| *s == slot) {
            *k = key;
        }
    }
}

/// What the Model dropdown shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ModelPick {
    /// A model Draft lists for the backend, by id.
    Listed(&'static str),
    /// Any id, typed and sent as-is.
    Other,
}

/// On-open (and post-save) snapshot used to detect unsaved changes.
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    cfg: Config,
    keys: Keys,
    autostart_enabled: bool,
}

/// What's wrong with each hotkey field, as the main process's parser would
/// find it on reload. A spec that fails there is logged and the old binding
/// kept — invisible from here — so Save refuses to write one.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct HotkeyErrors {
    pub dictate: Option<String>,
    /// Only checked while push-to-command is on: that's the only time the main
    /// process parses it, and the only time its field is on screen.
    pub command: Option<String>,
}

impl HotkeyErrors {
    pub fn of(cfg: &Config) -> Self {
        let check = |spec: &str| crate::hotkey::parse(spec).err().map(|e| e.to_string());
        HotkeyErrors {
            dictate: check(&cfg.hotkey),
            command: cfg
                .push_to_command
                .then(|| check(&cfg.command_hotkey))
                .flatten(),
        }
    }

    pub fn is_clear(&self) -> bool {
        self.dictate.is_none() && self.command.is_none()
    }
}

/// Why Save is off while there are edits, said wherever Save is offered.
const HOTKEY_BLOCKS_SAVE: &str = "Fix the hotkey before saving.";
const MODEL_BLOCKS_SAVE: &str = "Enter a model id before saving.";

/// Where a save lands: the config file, the keyring, the autostart entry.
pub(super) trait Store {
    fn save_config(&mut self, cfg: &Config) -> anyhow::Result<()>;
    fn write_key(&mut self, slot: KeySlot, write: &KeyWrite) -> anyhow::Result<()>;
    fn set_autostart(&mut self, enabled: bool) -> anyhow::Result<()>;
}

/// The machine's own: `config.toml`, Windows Credential Manager, HKCU Run.
pub(super) struct SystemStore;

impl Store for SystemStore {
    fn save_config(&mut self, cfg: &Config) -> anyhow::Result<()> {
        cfg.save()
    }

    fn write_key(&mut self, slot: KeySlot, write: &KeyWrite) -> anyhow::Result<()> {
        secrets::apply_write(slot, write)
    }

    fn set_autostart(&mut self, enabled: bool) -> anyhow::Result<()> {
        crate::autostart::set_enabled(enabled)
    }
}

/// The vocabulary's unique terms, and how many of them are used.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct VocabularyCount {
    pub used: usize,
    pub total: usize,
}

impl VocabularyCount {
    /// The note under the vocabulary box, when some terms go unused.
    pub fn caption(&self) -> Option<String> {
        (self.total > self.used).then(|| {
            format!(
                "{} terms — only the first {} are used.",
                self.total, self.used
            )
        })
    }
}

/// Everything the Settings window edits, and what it last loaded or saved.
pub(super) struct Form {
    pub cfg: Config,
    /// Editable text behind `cfg.vocabulary` — one term per line. The Vec is
    /// re-derived from this by `vocabulary_edited`; the buffer keeps blank
    /// lines the user is still typing around.
    pub vocab_buffer: String,
    pub keys: Keys,
    pub autostart_enabled: bool,
    /// The id typed for "Other…", while that is the model chosen. Not in the
    /// snapshot: `cfg.chat_model` carries what it saves.
    other_model: Option<String>,
    /// Where each key came from, as of open or the last save. Save consults it
    /// through `secrets::key_write` so an environment key is never persisted
    /// and a key that failed to load is never deleted.
    key_sources: Vec<(KeySlot, KeySource)>,
    baseline: Snapshot,
}

impl Form {
    /// Reads the keys and the autostart entry off the machine.
    pub fn load(cfg: Config) -> Self {
        let loaded: Vec<_> = Keys::default()
            .slots()
            .map(|s| {
                let (key, source) = secrets::load_key_with_source(s);
                (s, key, source)
            })
            .collect();
        Form::new(cfg, loaded, crate::autostart::is_enabled())
    }

    /// `loaded` is each slot's key as loaded, and where it came from.
    pub fn new(
        cfg: Config,
        loaded: impl IntoIterator<Item = (KeySlot, Option<String>, KeySource)>,
        autostart_enabled: bool,
    ) -> Self {
        let mut keys = Keys::default();
        // A slot `loaded` doesn't mention has no key to speak of.
        let mut key_sources: Vec<_> = keys.slots().map(|s| (s, KeySource::Absent)).collect();
        for (s, key, source) in loaded {
            keys.set(s, key.unwrap_or_default());
            if let Some((_, slot)) = key_sources.iter_mut().find(|(q, _)| *q == s) {
                *slot = source;
            }
        }
        // A chosen model Draft doesn't list opens as "Other…", showing its id.
        let other_model = match (cfg.chat_backend, cfg.chat_model.as_deref()) {
            (Some(b), Some(m)) if llm::listed(b, m).is_none() => Some(m.to_string()),
            _ => None,
        };
        let baseline = Snapshot {
            cfg: cfg.clone(),
            keys: keys.clone(),
            autostart_enabled,
        };
        Form {
            vocab_buffer: cfg.vocabulary.join("\n"),
            cfg,
            keys,
            autostart_enabled,
            other_model,
            key_sources,
            baseline,
        }
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            cfg: self.cfg.clone(),
            keys: self.keys.clone(),
            autostart_enabled: self.autostart_enabled,
        }
    }

    pub fn hotkey_errors(&self) -> HotkeyErrors {
        HotkeyErrors::of(&self.cfg)
    }

    pub fn is_dirty(&self) -> bool {
        self.snapshot() != self.baseline
    }

    /// What's wrong with the typed model id. Only an empty one is: Draft
    /// can't know which ids a backend serves.
    pub fn model_error(&self) -> Option<&'static str> {
        self.other_model
            .as_deref()
            .is_some_and(|m| m.trim().is_empty())
            .then_some("Enter a model id.")
    }

    /// Why Save is off despite edits. The field at fault may be on a pane the
    /// user has since left, so this is said wherever Save is offered.
    pub fn save_blocker(&self) -> Option<&'static str> {
        if !self.hotkey_errors().is_clear() {
            Some(HOTKEY_BLOCKS_SAVE)
        } else if self.model_error().is_some() {
            Some(MODEL_BLOCKS_SAVE)
        } else {
            None
        }
    }

    /// Save is offered only when there's something to save and nothing in it
    /// the main process would refuse.
    pub fn can_save(&self) -> bool {
        self.is_dirty() && self.save_blocker().is_none()
    }

    /// The Chat backend and model push-to-command asks once this form is
    /// saved: Draft's pick by the keys held here, until the user chooses.
    pub fn chat(&self) -> Chat {
        Chat::resolve(self.cfg.chat_backend, self.cfg.chat_model.as_deref(), |b| {
            self.keys.has(b)
        })
    }

    /// What the Model dropdown shows.
    pub fn model_pick(&self) -> ModelPick {
        let chat = self.chat();
        match (&self.other_model, llm::listed(chat.backend, &chat.model)) {
            (None, Some(m)) => ModelPick::Listed(m.id),
            _ => ModelPick::Other,
        }
    }

    /// The id typed for "Other…"; `None` while a listed model is chosen.
    pub fn other_model_mut(&mut self) -> Option<&mut String> {
        self.other_model.as_mut()
    }

    /// The key the chosen backend needs and this form doesn't hold.
    pub fn missing_chat_key(&self) -> Option<KeySlot> {
        let backend = self.chat().backend;
        (!self.keys.has(backend)).then_some(KeySlot::from(backend))
    }

    /// Choosing a backend asks its default model.
    pub fn choose_backend(&mut self, backend: ChatBackend) {
        if backend != self.chat().backend {
            self.choose(backend, llm::default_model(backend));
        }
    }

    pub fn choose_model(&mut self, pick: ModelPick) {
        let backend = self.chat().backend;
        match pick {
            ModelPick::Listed(id) => self.choose(backend, id),
            ModelPick::Other if self.other_model.is_none() => {
                self.other_model = Some(String::new());
                self.cfg.chat_backend = Some(backend);
                self.cfg.chat_model = Some(String::new());
            }
            ModelPick::Other => {}
        }
    }

    /// Call after the "Other…" id changes. Sent as typed, less the edges.
    pub fn other_model_edited(&mut self) {
        if let Some(id) = &self.other_model {
            self.cfg.chat_model = Some(id.trim().to_string());
        }
    }

    /// A listed choice. Always written down, even when it is what Draft
    /// would pick: a choice the user made must not move when a key does.
    fn choose(&mut self, backend: ChatBackend, model: &str) {
        self.other_model = None;
        self.cfg.chat_backend = Some(backend);
        self.cfg.chat_model = Some(model.to_string());
    }

    /// Call after `vocab_buffer` changes: one term per non-blank line, trimmed.
    pub fn vocabulary_edited(&mut self) {
        self.cfg.vocabulary = self
            .vocab_buffer
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
    }

    /// How many vocabulary terms a Provider is handed, of how many the list
    /// holds — counted by the same normalisation the transcription path uses.
    pub fn vocabulary_count(&self) -> VocabularyCount {
        let total = vocabulary::unique_terms(&self.cfg.vocabulary).len();
        VocabularyCount {
            used: total.min(vocabulary::MAX_TERMS),
            total,
        }
    }

    /// Writes everything out; `Err` is the message to show. Stops at the first
    /// failure, with what did land already counted as saved.
    pub fn save(&mut self, store: &mut impl Store) -> Result<(), String> {
        store
            .save_config(&self.cfg)
            .map_err(|e| format!("Couldn't save settings: {e}"))?;
        for (s, source) in self.key_sources.iter_mut() {
            let Some(write) =
                secrets::key_write(self.baseline.keys.get(*s), self.keys.get(*s), *source)
            else {
                continue;
            };
            store
                .write_key(*s, &write)
                .map_err(|e| format!("Couldn't save the {} API key: {e}", s.label()))?;
            // Advance this key's baseline now, not with the rest: if a later
            // step fails, the next save must compare against what the keyring
            // holds, or a Remove made in between would read as "no edit".
            *source = write.leaves();
            self.baseline.keys.set(*s, self.keys.get(*s).to_string());
        }
        store
            .set_autostart(self.autostart_enabled)
            .map_err(|e| format!("Couldn't change Start with Windows: {e}"))?;
        self.baseline = self.snapshot();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeStore {
        config: Option<Config>,
        keys: Vec<(KeySlot, KeyWrite)>,
        autostart: Option<bool>,
        /// "config", "keyring" or "autostart": that write fails.
        fail: Option<&'static str>,
    }

    impl FakeStore {
        fn check(&self, what: &str) -> anyhow::Result<()> {
            match self.fail {
                Some(f) if f == what => Err(anyhow::anyhow!("{what} is broken")),
                _ => Ok(()),
            }
        }
    }

    impl Store for FakeStore {
        fn save_config(&mut self, cfg: &Config) -> anyhow::Result<()> {
            self.check("config")?;
            self.config = Some(cfg.clone());
            Ok(())
        }

        fn write_key(&mut self, slot: KeySlot, write: &KeyWrite) -> anyhow::Result<()> {
            self.check("keyring")?;
            self.keys.push((slot, write.clone()));
            Ok(())
        }

        fn set_autostart(&mut self, enabled: bool) -> anyhow::Result<()> {
            self.check("autostart")?;
            self.autostart = Some(enabled);
            Ok(())
        }
    }

    fn form() -> Form {
        Form::new(Config::default(), [], false)
    }

    const GROQ: KeySlot = KeySlot::Provider(Provider::Groq);

    fn with_keys(keys: &[(KeySlot, &str, KeySource)]) -> Form {
        with_config_and_keys(Config::default(), keys)
    }

    fn with_config_and_keys(cfg: Config, keys: &[(KeySlot, &str, KeySource)]) -> Form {
        let loaded = keys
            .iter()
            .map(|&(s, k, source)| (s, (!k.is_empty()).then(|| k.to_string()), source));
        Form::new(cfg, loaded, false)
    }

    #[test]
    fn a_form_as_loaded_has_nothing_to_save() {
        let form = with_keys(&[(GROQ, "gsk", KeySource::Keyring)]);
        assert_eq!(form.keys.get(Provider::Groq), "gsk");
        assert!(!form.is_dirty());
        assert!(!form.can_save());
    }

    #[test]
    fn an_edit_is_unsaved_until_it_is_saved() {
        let mut form = form();
        form.cfg.append_trailing_space = !form.cfg.append_trailing_space;
        assert!(form.is_dirty());
        assert!(form.can_save());

        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(store.config.as_ref(), Some(&form.cfg));
        assert_eq!(store.autostart, Some(false));
        assert!(!form.is_dirty());
    }

    #[test]
    fn editing_a_setting_back_leaves_nothing_to_save() {
        let mut form = form();
        form.autostart_enabled = true;
        assert!(form.is_dirty());
        form.autostart_enabled = false;
        assert!(!form.is_dirty());
    }

    #[test]
    fn a_bad_hotkey_blocks_save() {
        let mut form = form();
        form.cfg.hotkey = "Ctrl+Bakslash".into();
        assert!(form.is_dirty());
        assert!(!form.can_save());
    }

    #[test]
    fn save_writes_only_the_keys_that_were_edited() {
        let mut form = with_keys(&[
            (GROQ, "gsk-old", KeySource::Keyring),
            (Provider::Openai.into(), "sk", KeySource::Keyring),
        ]);
        form.keys.set(GROQ, "gsk-new".into());
        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(store.keys, vec![(GROQ, KeyWrite::Set("gsk-new".into()))]);
    }

    #[test]
    fn clearing_a_key_from_the_environment_writes_nothing() {
        let mut form = with_keys(&[(GROQ, "gsk-env", KeySource::Environment)]);
        form.keys.set(Provider::Groq, String::new());
        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(store.keys, vec![]);
    }

    /// A key added and saved is in the keyring now, so removing it later in
    /// the same window deletes it.
    #[test]
    fn a_key_saved_in_this_window_can_be_removed_in_it() {
        let mut form = form();
        form.keys.set(Provider::Mistral, "m".into());
        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        form.keys.set(Provider::Mistral, String::new());
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(
            store.keys,
            vec![
                (Provider::Mistral.into(), KeyWrite::Set("m".into())),
                (Provider::Mistral.into(), KeyWrite::Delete),
            ]
        );
    }

    #[test]
    fn a_failed_config_save_writes_nothing_else_and_stays_unsaved() {
        let mut form = form();
        form.autostart_enabled = true;
        form.keys.set(GROQ, "gsk".into());
        let mut store = FakeStore {
            fail: Some("config"),
            ..Default::default()
        };
        let err = form.save(&mut store).unwrap_err();
        assert!(err.starts_with("Couldn't save settings: "), "{err}");
        assert_eq!(store.keys, vec![]);
        assert_eq!(store.autostart, None);
        assert!(form.is_dirty());
    }

    #[test]
    fn a_failed_keyring_write_names_the_provider() {
        let mut form = form();
        form.keys.set(GROQ, "gsk".into());
        let mut store = FakeStore {
            fail: Some("keyring"),
            ..Default::default()
        };
        let err = form.save(&mut store).unwrap_err();
        assert!(err.starts_with("Couldn't save the Groq API key: "), "{err}");
        assert!(form.is_dirty());
    }

    /// The retry compares against what the keyring holds: a key that landed
    /// before a later step failed is not written again.
    #[test]
    fn a_key_saved_before_a_later_failure_is_not_written_again() {
        let mut form = form();
        form.keys.set(GROQ, "gsk".into());
        form.autostart_enabled = true;
        let mut store = FakeStore {
            fail: Some("autostart"),
            ..Default::default()
        };
        let err = form.save(&mut store).unwrap_err();
        assert!(
            err.starts_with("Couldn't change Start with Windows: "),
            "{err}"
        );
        assert!(form.is_dirty(), "autostart is still unsaved");

        store.fail = None;
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(store.keys, vec![(GROQ, KeyWrite::Set("gsk".into()))]);
        assert_eq!(store.autostart, Some(true));
        assert!(!form.is_dirty());
    }

    #[test]
    fn the_vocabulary_is_one_trimmed_term_per_non_blank_line() {
        let mut form = form();
        form.vocab_buffer = "  Janssen \n\n\tkubectl\n   \nReson8".into();
        form.vocabulary_edited();
        assert_eq!(form.cfg.vocabulary, ["Janssen", "kubectl", "Reson8"]);
        assert!(form.is_dirty());
    }

    #[test]
    fn the_vocabulary_opens_one_term_per_line() {
        let cfg = Config {
            vocabulary: vec!["Janssen".into(), "kubectl".into()],
            ..Config::default()
        };
        assert_eq!(Form::new(cfg, [], false).vocab_buffer, "Janssen\nkubectl");
    }

    /// A blank line typed on the way to the next term is not an edit.
    #[test]
    fn a_blank_line_in_the_vocabulary_is_not_an_unsaved_change() {
        let cfg = Config {
            vocabulary: vec!["Janssen".into()],
            ..Config::default()
        };
        let mut form = Form::new(cfg, [], false);
        form.vocab_buffer.push('\n');
        form.vocabulary_edited();
        assert!(!form.is_dirty());
    }

    fn with_vocabulary(terms: impl IntoIterator<Item = String>) -> Form {
        let cfg = Config {
            vocabulary: terms.into_iter().collect(),
            ..Config::default()
        };
        Form::new(cfg, [], false)
    }

    #[test]
    fn a_hundred_and_one_terms_are_captioned() {
        let form = with_vocabulary((0..101).map(|i| format!("term{i}")));
        assert_eq!(
            form.vocabulary_count(),
            VocabularyCount {
                used: 100,
                total: 101
            }
        );
        assert_eq!(
            form.vocabulary_count().caption().as_deref(),
            Some("101 terms — only the first 100 are used.")
        );
    }

    #[test]
    fn a_hundred_terms_are_not_captioned() {
        let form = with_vocabulary((0..100).map(|i| format!("term{i}")));
        assert_eq!(form.vocabulary_count().caption(), None);
    }

    #[test]
    fn repeats_do_not_count_toward_the_caption() {
        let terms = (0..100).map(|i| format!("term{i}"));
        let form = with_vocabulary(terms.clone().chain(terms.take(20)));
        assert_eq!(form.vocabulary_count().total, 100);
        assert_eq!(form.vocabulary_count().caption(), None);
    }

    #[test]
    fn the_default_hotkeys_are_clear() {
        let cfg = Config {
            push_to_command: true,
            ..Config::default()
        };
        assert!(HotkeyErrors::of(&cfg).is_clear());
    }

    #[test]
    fn a_bad_dictation_hotkey_is_flagged_on_its_own_field() {
        let cfg = Config {
            hotkey: "Ctrl+Bakslash".into(),
            ..Config::default()
        };
        let errs = HotkeyErrors::of(&cfg);
        assert!(errs.dictate.as_deref().unwrap().contains("Bakslash"));
        assert_eq!(errs.command, None);
        assert!(!errs.is_clear());
    }

    #[test]
    fn a_bad_command_hotkey_is_flagged_while_push_to_command_is_on() {
        let cfg = Config {
            push_to_command: true,
            command_hotkey: "Ctrl+Shift".into(),
            ..Config::default()
        };
        let errs = HotkeyErrors::of(&cfg);
        assert_eq!(errs.dictate, None);
        assert!(errs.command.is_some());
        assert!(!errs.is_clear());
    }

    /// The main process never parses the command chord with push-to-command
    /// off, and the field isn't on screen — so it can't block Save either.
    #[test]
    fn a_bad_command_hotkey_is_ignored_while_push_to_command_is_off() {
        let cfg = Config {
            push_to_command: false,
            command_hotkey: "nonsense".into(),
            ..Config::default()
        };
        assert!(HotkeyErrors::of(&cfg).is_clear());
    }

    #[test]
    fn every_provider_with_a_keyring_slot_holds_a_key() {
        let mut keys = Keys::default();
        for &p in ALL_PROVIDERS {
            keys.set(p, format!("{p:?}-key"));
        }
        for &p in ALL_PROVIDERS {
            let want = match secrets::slot_name(p) {
                Some(_) => format!("{p:?}-key"),
                None => String::new(),
            };
            assert_eq!(keys.get(p), want, "{p:?}");
        }
    }

    #[test]
    fn local_parakeet_takes_no_key() {
        let keys = Keys::default();
        assert!(!keys.slots().any(|s| s == Provider::LocalParakeet.into()));
        // Every other Provider, and Cerebras.
        assert_eq!(keys.slots().count(), ALL_PROVIDERS.len());
    }

    /// Cerebras has a key here, but it is a Chat backend, not a Provider: it
    /// is never offered as one, and the Groq Chat backend has no key of its
    /// own beside the Groq Provider's.
    #[test]
    fn cerebras_holds_a_key_but_stays_out_of_the_provider_list() {
        assert!(Keys::default().slots().any(|s| s == KeySlot::Cerebras));
        assert!(ALL_PROVIDERS
            .iter()
            .all(|p| p.label() != ChatBackend::Cerebras.label()));
        let groq_slots = Keys::default()
            .slots()
            .filter(|&s| s == ChatBackend::Groq.into())
            .count();
        assert_eq!(groq_slots, 1);
    }

    #[test]
    fn the_cerebras_key_round_trips_through_save_and_the_dirty_check() {
        let mut form = form();
        form.keys.set(KeySlot::Cerebras, "csk".into());
        assert!(form.is_dirty());
        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(
            store.keys,
            vec![(KeySlot::Cerebras, KeyWrite::Set("csk".into()))]
        );
        assert!(!form.is_dirty());
    }

    #[test]
    fn a_failed_cerebras_key_write_names_it() {
        let mut form = form();
        form.keys.set(KeySlot::Cerebras, "csk".into());
        let mut store = FakeStore {
            fail: Some("keyring"),
            ..Default::default()
        };
        let err = form.save(&mut store).unwrap_err();
        assert!(
            err.starts_with("Couldn't save the Cerebras API key: "),
            "{err}"
        );
    }

    #[test]
    fn the_default_backend_follows_which_keys_are_stored() {
        let pick = |keys: &[(KeySlot, &str, KeySource)]| with_keys(keys).chat();
        let qwen = Chat {
            backend: ChatBackend::Cerebras,
            model: llm::QWEN_3_8_27B.into(),
        };
        let gpt_oss = Chat {
            backend: ChatBackend::Groq,
            model: llm::GROQ_GPT_OSS_120B.into(),
        };
        assert_eq!(pick(&[]), qwen);
        assert_eq!(pick(&[(GROQ, "gsk", KeySource::Keyring)]), gpt_oss);
        assert_eq!(
            pick(&[
                (GROQ, "gsk", KeySource::Keyring),
                (KeySlot::Cerebras, "csk", KeySource::Environment),
            ]),
            qwen
        );
        assert_eq!(
            pick(&[(KeySlot::Cerebras, "csk", KeySource::Keyring)]),
            qwen
        );
    }

    /// Typed into this window and not yet saved, a key still moves the pick:
    /// the pane shows what push-to-command would ask once saved.
    #[test]
    fn a_key_typed_in_this_window_moves_the_default_backend() {
        let mut form = with_keys(&[(GROQ, "gsk", KeySource::Keyring)]);
        assert_eq!(form.chat().backend, ChatBackend::Groq);
        form.keys.set(KeySlot::Cerebras, "csk".into());
        assert_eq!(form.chat().backend, ChatBackend::Cerebras);
        assert_eq!(form.cfg.chat_backend, None, "still Draft's pick");
    }

    #[test]
    fn a_chosen_backend_and_model_round_trip_through_save_and_the_dirty_check() {
        let mut form = form();
        form.choose_backend(ChatBackend::Groq);
        assert_eq!(form.cfg.chat_backend, Some(ChatBackend::Groq));
        assert_eq!(form.cfg.chat_model.as_deref(), Some(llm::GROQ_GPT_OSS_120B));
        assert!(form.can_save());

        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        let saved = store.config.clone().unwrap();
        assert_eq!(saved.chat_backend, Some(ChatBackend::Groq));
        assert!(!form.is_dirty());

        let reopened = Form::new(saved, [], false);
        assert_eq!(reopened.chat(), form.chat());
        assert_eq!(
            reopened.model_pick(),
            ModelPick::Listed(llm::GROQ_GPT_OSS_120B)
        );
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn a_listed_model_is_chosen_on_the_backend_shown() {
        let mut form = form();
        assert_eq!(form.model_pick(), ModelPick::Listed(llm::QWEN_3_8_27B));
        form.choose_model(ModelPick::Listed(llm::CEREBRAS_GPT_OSS_120B));
        assert_eq!(form.cfg.chat_backend, Some(ChatBackend::Cerebras));
        assert_eq!(
            form.cfg.chat_model.as_deref(),
            Some(llm::CEREBRAS_GPT_OSS_120B)
        );
        assert_eq!(
            form.model_pick(),
            ModelPick::Listed(llm::CEREBRAS_GPT_OSS_120B)
        );
        assert!(form.is_dirty());
    }

    /// Choosing a backend asks its default: a model id means nothing on a
    /// backend that doesn't serve it.
    #[test]
    fn choosing_another_backend_asks_its_default_model() {
        let mut form = form();
        form.choose_model(ModelPick::Listed(llm::CEREBRAS_GPT_OSS_120B));
        form.choose_backend(ChatBackend::Groq);
        assert_eq!(form.chat().model, llm::GROQ_GPT_OSS_120B);
        form.choose_backend(ChatBackend::Cerebras);
        assert_eq!(form.chat().model, llm::QWEN_3_8_27B);
    }

    /// A choice the user made is written down even when it is Draft's pick,
    /// so it can't move to another backend when a key is added or removed.
    #[test]
    fn choosing_what_draft_already_picks_is_still_a_choice() {
        let mut form = form();
        form.choose_model(ModelPick::Listed(llm::CEREBRAS_GPT_OSS_120B));
        form.choose_model(ModelPick::Listed(llm::QWEN_3_8_27B));
        assert_eq!(form.cfg.chat_backend, Some(ChatBackend::Cerebras));
        assert!(form.is_dirty());
        form.keys.set(GROQ, "gsk".into());
        assert_eq!(form.chat().backend, ChatBackend::Cerebras);
    }

    #[test]
    fn a_choice_already_saved_stays_a_choice_when_picked_again() {
        let cfg = Config {
            chat_backend: Some(ChatBackend::Cerebras),
            chat_model: Some(llm::CEREBRAS_GPT_OSS_120B.into()),
            ..Config::default()
        };
        let mut form = Form::new(cfg, [], false);
        form.choose_model(ModelPick::Listed(llm::QWEN_3_8_27B));
        assert_eq!(form.cfg.chat_backend, Some(ChatBackend::Cerebras));
        form.choose_model(ModelPick::Listed(llm::CEREBRAS_GPT_OSS_120B));
        assert!(!form.is_dirty());
    }

    #[test]
    fn other_saves_the_id_as_typed_and_blocks_save_while_it_is_empty() {
        let mut form = form();
        form.choose_model(ModelPick::Other);
        assert_eq!(form.model_pick(), ModelPick::Other);
        assert_eq!(form.model_error(), Some("Enter a model id."));
        assert_eq!(form.save_blocker(), Some("Enter a model id before saving."));
        assert!(form.is_dirty());
        assert!(!form.can_save());

        *form.other_model_mut().unwrap() = "  vendor/next-model ".into();
        form.other_model_edited();
        assert_eq!(form.model_error(), None);
        assert!(form.can_save());

        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        let saved = store.config.unwrap();
        assert_eq!(saved.chat_backend, Some(ChatBackend::Cerebras));
        assert_eq!(saved.chat_model.as_deref(), Some("vendor/next-model"));
        assert!(!form.is_dirty());

        let mut reopened = Form::new(saved, [], false);
        assert_eq!(reopened.model_pick(), ModelPick::Other);
        assert_eq!(
            reopened.other_model_mut().map(|s| s.as_str()),
            Some("vendor/next-model")
        );
        assert!(!reopened.is_dirty());
    }

    #[test]
    fn choosing_a_listed_model_after_other_drops_the_typed_id() {
        let mut form = form();
        form.choose_model(ModelPick::Other);
        form.choose_model(ModelPick::Listed(llm::QWEN_3_8_27B));
        assert_eq!(form.model_pick(), ModelPick::Listed(llm::QWEN_3_8_27B));
        assert_eq!(form.model_error(), None);
        assert!(form.other_model_mut().is_none());
        assert!(form.can_save());
    }

    #[test]
    fn the_missing_key_is_the_chosen_backends() {
        assert_eq!(form().missing_chat_key(), Some(KeySlot::Cerebras));
        let mut form = with_keys(&[(GROQ, "gsk", KeySource::Keyring)]);
        assert_eq!(form.missing_chat_key(), None);
        form.choose_backend(ChatBackend::Cerebras);
        assert_eq!(form.missing_chat_key(), Some(KeySlot::Cerebras));
        form.keys.set(KeySlot::Cerebras, "csk".into());
        assert_eq!(form.missing_chat_key(), None);

        let cfg = Config {
            chat_backend: Some(ChatBackend::Groq),
            ..Config::default()
        };
        let form = with_config_and_keys(cfg, &[(KeySlot::Cerebras, "csk", KeySource::Keyring)]);
        assert_eq!(form.missing_chat_key(), Some(GROQ));
    }
}
