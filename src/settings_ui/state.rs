// What the Settings window edits and how it saves, without the window: the
// config being edited, the API keys, the autostart flag, the snapshot they are
// compared against for "unsaved changes", and the save that writes them out.
// The panes bind to a `Form`'s fields and ask it whether Save is on; nothing
// here knows about egui, so it is asserted directly.

use crate::config::{Config, Provider};
use crate::secrets::{self, KeySource, KeyWrite};
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

/// One API key per Provider that takes one — those `secrets::slot_name` gives
/// a keyring slot — so a new Provider in `ALL_PROVIDERS` gets its key here
/// without another edit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ProviderKeys(Vec<(Provider, String)>);

impl Default for ProviderKeys {
    fn default() -> Self {
        ProviderKeys(
            ALL_PROVIDERS
                .iter()
                .filter(|&&p| secrets::slot_name(p).is_some())
                .map(|&p| (p, String::new()))
                .collect(),
        )
    }
}

impl ProviderKeys {
    /// The Providers that take a key, in picker order.
    pub fn providers(&self) -> impl Iterator<Item = Provider> + '_ {
        self.0.iter().map(|(p, _)| *p)
    }

    /// `p`'s key; empty for none, and for a Provider that takes no key.
    pub fn get(&self, p: Provider) -> &str {
        self.0
            .iter()
            .find(|(q, _)| *q == p)
            .map_or("", |(_, key)| key)
    }

    /// Ignored for a Provider that takes no key.
    pub fn set(&mut self, p: Provider, key: String) {
        if let Some((_, slot)) = self.0.iter_mut().find(|(q, _)| *q == p) {
            *slot = key;
        }
    }
}

/// On-open (and post-save) snapshot used to detect unsaved changes.
#[derive(Clone, PartialEq, Eq)]
struct Snapshot {
    cfg: Config,
    keys: ProviderKeys,
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

/// Where a save lands: the config file, the keyring, the autostart entry.
pub(super) trait Store {
    fn save_config(&mut self, cfg: &Config) -> anyhow::Result<()>;
    fn write_key(&mut self, p: Provider, write: &KeyWrite) -> anyhow::Result<()>;
    fn set_autostart(&mut self, enabled: bool) -> anyhow::Result<()>;
}

/// The machine's own: `config.toml`, Windows Credential Manager, HKCU Run.
pub(super) struct SystemStore;

impl Store for SystemStore {
    fn save_config(&mut self, cfg: &Config) -> anyhow::Result<()> {
        cfg.save()
    }

    fn write_key(&mut self, p: Provider, write: &KeyWrite) -> anyhow::Result<()> {
        secrets::apply_write(p, write)
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
    pub keys: ProviderKeys,
    pub autostart_enabled: bool,
    /// Where each key came from, as of open or the last save. Save consults it
    /// through `secrets::key_write` so an environment key is never persisted
    /// and a key that failed to load is never deleted.
    key_sources: Vec<(Provider, KeySource)>,
    baseline: Snapshot,
}

impl Form {
    /// Reads the keys and the autostart entry off the machine.
    pub fn load(cfg: Config) -> Self {
        let loaded: Vec<_> = ProviderKeys::default()
            .providers()
            .map(|p| {
                let (key, source) = secrets::load_key_with_source(p);
                (p, key, source)
            })
            .collect();
        Form::new(cfg, loaded, crate::autostart::is_enabled())
    }

    /// `loaded` is each Provider's key as loaded, and where it came from.
    pub fn new(
        cfg: Config,
        loaded: impl IntoIterator<Item = (Provider, Option<String>, KeySource)>,
        autostart_enabled: bool,
    ) -> Self {
        let mut keys = ProviderKeys::default();
        // A Provider `loaded` doesn't mention has no key to speak of.
        let mut key_sources: Vec<_> = keys.providers().map(|p| (p, KeySource::Absent)).collect();
        for (p, key, source) in loaded {
            keys.set(p, key.unwrap_or_default());
            if let Some((_, slot)) = key_sources.iter_mut().find(|(q, _)| *q == p) {
                *slot = source;
            }
        }
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

    /// Save is offered only when there's something to save and nothing in it
    /// the main process would refuse.
    pub fn can_save(&self) -> bool {
        self.is_dirty() && self.hotkey_errors().is_clear()
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
        for (p, source) in self.key_sources.iter_mut() {
            let Some(write) =
                secrets::key_write(self.baseline.keys.get(*p), self.keys.get(*p), *source)
            else {
                continue;
            };
            store
                .write_key(*p, &write)
                .map_err(|e| format!("Couldn't save the {} API key: {e}", p.label()))?;
            // Advance this key's baseline now, not with the rest: if a later
            // step fails, the next save must compare against what the keyring
            // holds, or a Remove made in between would read as "no edit".
            *source = write.leaves();
            self.baseline.keys.set(*p, self.keys.get(*p).to_string());
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
        keys: Vec<(Provider, KeyWrite)>,
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

        fn write_key(&mut self, p: Provider, write: &KeyWrite) -> anyhow::Result<()> {
            self.check("keyring")?;
            self.keys.push((p, write.clone()));
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

    fn with_keys(keys: &[(Provider, &str, KeySource)]) -> Form {
        let loaded = keys
            .iter()
            .map(|&(p, k, source)| (p, (!k.is_empty()).then(|| k.to_string()), source));
        Form::new(Config::default(), loaded, false)
    }

    #[test]
    fn a_form_as_loaded_has_nothing_to_save() {
        let form = with_keys(&[(Provider::Groq, "gsk", KeySource::Keyring)]);
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
            (Provider::Groq, "gsk-old", KeySource::Keyring),
            (Provider::Openai, "sk", KeySource::Keyring),
        ]);
        form.keys.set(Provider::Groq, "gsk-new".into());
        let mut store = FakeStore::default();
        assert_eq!(form.save(&mut store), Ok(()));
        assert_eq!(
            store.keys,
            vec![(Provider::Groq, KeyWrite::Set("gsk-new".into()))]
        );
    }

    #[test]
    fn clearing_a_key_from_the_environment_writes_nothing() {
        let mut form = with_keys(&[(Provider::Groq, "gsk-env", KeySource::Environment)]);
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
                (Provider::Mistral, KeyWrite::Set("m".into())),
                (Provider::Mistral, KeyWrite::Delete),
            ]
        );
    }

    #[test]
    fn a_failed_config_save_writes_nothing_else_and_stays_unsaved() {
        let mut form = form();
        form.autostart_enabled = true;
        form.keys.set(Provider::Groq, "gsk".into());
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
        form.keys.set(Provider::Groq, "gsk".into());
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
        form.keys.set(Provider::Groq, "gsk".into());
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
        assert_eq!(
            store.keys,
            vec![(Provider::Groq, KeyWrite::Set("gsk".into()))]
        );
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
        let mut keys = ProviderKeys::default();
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
        let keys = ProviderKeys::default();
        assert!(!keys.providers().any(|p| p == Provider::LocalParakeet));
        assert_eq!(keys.providers().count(), ALL_PROVIDERS.len() - 1);
    }
}
