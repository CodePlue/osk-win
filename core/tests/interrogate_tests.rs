//! Regression tests covering defect areas found during interrogation.

use oskwin_core::predict::Predictor;
use oskwin_core::typekey::{type_text, CharMap, KeySink};

struct TestSink(Vec<(u16, bool)>);
impl KeySink for TestSink {
    fn press(&mut self, code: u16) {
        self.0.push((code, true));
    }
    fn release(&mut self, code: u16) {
        self.0.push((code, false));
    }
}

#[test]
fn predictor_empty_commit_clears_context() {
    let mut p = Predictor::ephemeral();
    p.commit("first");
    p.commit("second");
    // Empty commit must clear both last_word and prev_word
    p.commit("");

    // With context cleared, typing 'a' must match clean state
    p.char('a');
    let s_after = p.suggestions(&[]);

    let mut clean = Predictor::ephemeral();
    clean.char('a');
    let s_clean = clean.suggestions(&[]);

    assert_eq!(s_after, s_clean, "commit('') must not leave residual context");
}

#[test]
fn predictor_backspace_shrinks_partial() {
    let mut p = Predictor::ephemeral();
    p.char('c');
    p.char('a');
    p.char('t');
    assert_eq!(p.partial(), "cat");

    p.backspace();
    assert_eq!(p.partial(), "ca");

    p.backspace();
    assert_eq!(p.partial(), "c");

    p.backspace();
    assert_eq!(p.partial(), "");

    // Extra backspaces on empty partial do not crash or corrupt
    p.backspace();
    assert_eq!(p.partial(), "");
}

#[test]
fn type_text_crlf_and_tabs() {
    let map = match CharMap::new() {
        Some(m) => m,
        None => return, // xkbcommon not available in headless env
    };

    let mut sink = TestSink(Vec::new());
    let res = type_text(&mut sink, &map, "hello\r\nworld\ttest", || {});
    assert!(res.is_ok(), "type_text must accept CRLF and tab characters: {:?}", res);

    // Verify key codes: enter (28), tab (15)
    use oskwin_core::key::key;
    let has_enter = sink.0.iter().any(|(code, _)| *code == key::KEY_ENTER);
    let has_tab = sink.0.iter().any(|(code, _)| *code == key::KEY_TAB);
    assert!(has_enter, "CRLF must inject KEY_ENTER");
    assert!(has_tab, "Tab must inject KEY_TAB");
}

#[test]
fn multiline_typing_emits_enter_between_lines() {
    let map = match CharMap::new() {
        Some(m) => m,
        None => return,
    };
    let mut sink = TestSink(Vec::new());
    let res = type_text(&mut sink, &map, "a\nb", || {});
    assert!(res.is_ok());

    use oskwin_core::key::key;
    let expected = vec![
        (key::KEY_A, true),
        (key::KEY_A, false),
        (key::KEY_ENTER, true),
        (key::KEY_ENTER, false),
        (key::KEY_B, true),
        (key::KEY_B, false),
    ];
    assert_eq!(sink.0, expected);
}

#[test]
fn numpad_rows_sum_to_20_5u() {
    use oskwin_core::layout::LAYOUT_NUMPAD;
    for (i, row) in LAYOUT_NUMPAD.rows.iter().enumerate() {
        let sum: u16 = row.keys.iter().map(|k| k.width).sum();
        assert_eq!(sum, 410, "65%+Num row {} must sum to 410 (20.5u)", i);
    }
}

#[test]
fn gboard_user_learning_and_context_boost() {
    let mut p = Predictor::ephemeral();

    // Commit a word pair twice: "project" -> "system"
    p.commit("project");
    p.commit("system");
    p.commit("project");
    p.commit("system");

    // Clear context
    p.commit("");

    // Now type "project": "system" must be suggested due to learned user bigram!
    p.commit("project");
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.contains(&"system"),
        "Learned bigram 'system' must be suggested after 'project': {:?}",
        suggs
    );
}

#[test]
fn test_out_of_vocabulary_learning_and_suggestion() {
    let mut p = Predictor::ephemeral();
    for c in "hyprland".chars() {
        p.char(c);
    }
    assert_eq!(p.partial(), "hyprland");
    p.commit("hyprland");
    p.commit("");

    for c in "hyp".chars() {
        p.char(c);
    }
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.contains(&"hyprland"),
        "learned out-of-vocabulary word must be suggested for its prefix: {:?}",
        suggs
    );

}

#[test]
fn test_contraction_typing_and_completion() {
    let mut p = Predictor::ephemeral();
    for c in "don".chars() {
        p.char(c);
    }
    p.char('\'');
    p.char('t');
    assert_eq!(p.partial(), "don't");

    let suggs = p.suggestions(&[]);
    assert!(
        !suggs.is_empty() && suggs.iter().all(|w| w.starts_with("don't")),
        "prefix 'don''t' must complete contractions only: {:?}",
        suggs
    );
    assert!(
        suggs.contains(&"don't"),
        "lexicon contraction must be offered: {:?}",
        suggs
    );

    p.commit("don't");
    p.commit("");
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.contains(&"don't"),
        "committed contraction must be learned and offered: {:?}",
        suggs
    );
}

#[test]
fn test_user_history_persistence_roundtrip() {
    let dir = std::env::temp_dir().join(format!("osk-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("user_history.tsv");

    // Persistence now goes through the text API; the file write is the
    // host's part, hand-rolled here just for the round trip.
    let mut p = Predictor::ephemeral();
    p.commit("hyprland");
    p.commit("wayland");
    p.commit("hyprland");

    let data = p.history_tsv();
    std::fs::write(&path, &data).unwrap();
    assert!(
        data.lines().any(|l| l == "u\thyprland\t\t2"),
        "unigram TSV line with count 2: {data:?}"
    );
    assert!(
        data.lines().any(|l| l == "b\thyprland\twayland\t1"),
        "bigram TSV line: {data:?}"
    );

    let mut q = Predictor::ephemeral();
    q.load_history_tsv(&std::fs::read_to_string(&path).unwrap());
    for c in "hyp".chars() {
        q.char(c);
    }
    assert!(
        q.suggestions(&[]).contains(&"hyprland"),
        "loaded history must drive prefix completion"
    );

    // Learned bigrams survive the roundtrip too.
    q.commit("wayland");
    assert!(
        q.suggestions(&[]).contains(&"hyprland"),
        "loaded bigram wayland->hyprland must be suggested"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_suggestion_diversity() {
    let mut p = Predictor::ephemeral();
    for c in "pro".chars() {
        p.char(c);
    }
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.len() >= 2,
        "prefix 'pro' must have multiple completions: {:?}",
        suggs
    );
    for i in 0..suggs.len() {
        for j in (i + 1)..suggs.len() {
            let shared = suggs[i]
                .chars()
                .zip(suggs[j].chars())
                .take_while(|(a, b)| a == b)
                .count();
            assert!(
                shared < 4,
                "pills '{}' and '{}' share a {}-char stem: {:?}",
                suggs[i],
                suggs[j],
                shared,
                suggs
            );
        }
    }
}

#[test]
fn test_sentence_start_blend() {
    let mut p = Predictor::ephemeral();
    p.char('h');
    p.char('i');
    p.char('.');
    assert_eq!(p.partial(), "");
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.contains(&"i") && suggs.contains(&"the"),
        "sentence starters must be offered after '.': {:?}",
        suggs
    );

    // A learned word also appears at sentence start.
    let mut q = Predictor::ephemeral();
    q.commit("hyprland");
    q.char('.');
    let suggs = q.suggestions(&[]);
    assert!(
        suggs.contains(&"hyprland"),
        "learned word must be offered at sentence start: {:?}",
        suggs
    );
}

#[test]
fn test_neural_engine_graceful_fallback() {
    use oskwin_core::neural::NeuralEngine;
    let dir = std::env::temp_dir().join(format!("osk-neural-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // No model file: construction must return None, not panic or block.
    let engine = NeuralEngine::new_at(&dir);
    assert!(engine.is_none(), "missing model must yield None");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_embedded_neural_engine_always_ready() {
    use oskwin_core::controller::{Controller, ControllerConfig};

    // The embedded weights make construction succeed with no download.
    let engine = oskwin_core::neural::NeuralEngine::new();
    assert!(
        engine.is_some(),
        "embedded model must make new() succeed without a download"
    );

    // The controller owns the engine as a value; there is no process
    // global to install into. A fresh engine starts enabled.
    let mut ctl = Controller::with_neural(
        ControllerConfig::default(),
        Predictor::ephemeral(),
        engine,
    );
    assert!(
        ctl.is_neural_enabled(),
        "freshly constructed engine is enabled by default"
    );

    // Toggling off disables it, toggling back on re-enables it. The
    // state lives in this controller only, so other tests cannot be
    // reached by it.
    assert!(
        !ctl.toggle_neural() && !ctl.is_neural_enabled(),
        "toggle turns the engine off"
    );
    assert!(ctl.toggle_neural() && ctl.is_neural_enabled(), "toggle turns the engine back on");

    // The engine-less construction path (offline render, tests) never
    // reports an engine, and toggling there stays off.
    let mut offline = Controller::with_predictor(
        ControllerConfig::default(),
        Predictor::ephemeral(),
    );
    assert!(!offline.is_neural_enabled(), "no engine, not enabled");
    assert!(
        !offline.toggle_neural(),
        "toggling without an engine returns false"
    );
}

#[test]
fn test_learned_words_management() {
    let dir = std::env::temp_dir().join(format!("osk-learned-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("user_history.tsv");

    let mut p = Predictor::ephemeral();

    // Invalid input is rejected without touching the history.
    assert!(!p.learn_word("", 1));
    assert!(!p.learn_word("!!!", 1));

    // Explicit learning plus recording through commits and typing.
    assert!(p.learn_word("osk", 2));
    p.commit("the");
    p.commit("hyprland");
    for c in "wayland".chars() {
        p.char(c);
    }
    p.space();

    let learned = p.learned_words();
    assert_eq!(
        learned.iter().map(|l| l.word).collect::<Vec<_>>(),
        vec!["osk", "hyprland", "the", "wayland"],
        "sorted by count desc, then word asc"
    );
    let osk = learned.iter().find(|l| l.word == "osk").unwrap();
    assert_eq!((osk.count, osk.links), (2, 0));
    let hypr = learned.iter().find(|l| l.word == "hyprland").unwrap();
    assert_eq!(
        hypr.links, 2,
        "hyprland participates in the->hyprland and hyprland->wayland"
    );
    assert!(p.is_learned("hyprland"));
    assert!(!p.is_learned("quasar"));

    // Persistence goes through the text API; the file write stands in
    // for the host's save here.
    std::fs::write(&path, p.history_tsv()).unwrap();
    let data = std::fs::read_to_string(&path).unwrap();
    assert!(data.lines().any(|l| l == "u\thyprland\t\t1"));
    assert!(data.lines().any(|l| l == "b\tthe\thyprland\t1"));
    assert!(data.lines().any(|l| l == "b\thyprland\twayland\t1"));

    // Forgetting removes the unigram and both bigrams from the TSV.
    assert!(p.forget_word("hyprland"));
    assert!(!p.forget_word("hyprland"));
    assert!(!p.is_learned("hyprland"));
    std::fs::write(&path, p.history_tsv()).unwrap();
    let data = std::fs::read_to_string(&path).unwrap();
    assert!(
        !data.lines().any(|l| l.contains("hyprland")),
        "no hyprland unigram or bigram may survive: {data:?}"
    );
    let learned = p.learned_words();
    let wl = learned.iter().find(|l| l.word == "wayland").unwrap();
    assert_eq!(wl.links, 0, "wayland's only bigram died with hyprland");

    p.clear_learned();
    assert!(p.learned_words().is_empty());
    std::fs::write(&path, p.history_tsv()).unwrap();
    assert!(
        std::fs::read_to_string(&path).unwrap().is_empty(),
        "clear_learned leaves an empty TSV"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn misspelled_partial_offers_corrections() {
    for (typed, want) in [
        ("teh", "the"),
        ("recieve", "receive"),
        ("wierd", "weird"),
        ("definately", "definitely"),
        ("keybaord", "keyboard"),
    ] {
        let mut p = Predictor::ephemeral();
        for c in typed.chars() {
            p.char(c);
        }
        let suggs = p.suggestions(&[]);
        assert_eq!(suggs.first(), Some(&want), "{typed}: {suggs:?}");
    }
}

#[test]
fn correct_prefix_keeps_completions_first() {
    let mut p = Predictor::ephemeral();
    for c in "wor".chars() {
        p.char(c);
    }
    let suggs = p.suggestions(&[]);
    assert!(
        suggs.iter().take(3).all(|w| w.starts_with("wor")),
        "{suggs:?}"
    );
}

#[test]
fn replace_word_erases_and_retypes_corrections() {
    use oskwin_core::events::replace_word;
    use oskwin_core::mods::Casing;
    let mut buf = [0 as char; 16];
    let text = |buf: &[char], n: usize| buf[..n].iter().collect::<String>();

    // Completion: no backspaces, same as complete_word.
    let (erase, n) = replace_word("there", "the", "the", Casing::default(), &mut buf).unwrap();
    assert_eq!((erase, text(&buf, n).as_str()), (0, "re "));

    // "teh" -> "the": keep the shared "t", erase "eh", type "he ".
    let (erase, n) = replace_word("the", "teh", "Teh", Casing::default(), &mut buf).unwrap();
    assert_eq!((erase, text(&buf, n).as_str()), (2, "he "));

    // A capitalized first letter is retyped capitalized.
    let (erase, n) = replace_word("quick", "qick", "Qick", Casing::default(), &mut buf).unwrap();
    assert_eq!((erase, text(&buf, n).as_str()), (3, "uick "));
    let (erase, n) = replace_word("the", "hte", "Hte", Casing::default(), &mut buf).unwrap();
    assert_eq!((erase, text(&buf, n).as_str()), (3, "The "));

    // All caps stays all caps.
    let (erase, n) = replace_word("the", "teh", "TEH", Casing::default(), &mut buf).unwrap();
    assert_eq!((erase, text(&buf, n).as_str()), (2, "HE "));

    assert!(replace_word("because", "becuase", "becuase", Casing::default(), &mut [' '; 3]).is_none());
}
