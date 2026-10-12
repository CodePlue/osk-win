//! Regression tests covering defect areas found during interrogation.

use oskwin_core::events::complete_word;
use oskwin_core::layout::LAYOUT;
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
fn complete_word_exact_match_buffer_len_1() {
    let mut buf = [0 as char; 1];
    let res = complete_word("the", "the", false, false, &mut buf);
    assert_eq!(res, Some(1), "exact word match with 1 trailing space should fit in len 1 buffer");
    assert_eq!(buf[0], ' ');
}

#[test]
fn complete_word_zero_buffer() {
    let mut buf = [0 as char; 0];
    let res = complete_word("the", "the", false, false, &mut buf);
    assert_eq!(res, None, "zero buffer must return None");
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
    let s_after = p.suggestions();

    let mut clean = Predictor::ephemeral();
    clean.char('a');
    let s_clean = clean.suggestions();

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
fn layout_row_sums_and_inert_gap() {
    // Verify 65% layout geometry contract: every row sums to 320 (16.0u)
    for (i, row) in LAYOUT.rows.iter().enumerate() {
        let sum: u16 = row.keys.iter().map(|k| k.width).sum();
        assert_eq!(sum, 320, "row {} must sum to 320 (16.0u)", i);
    }

    // Verify the inert gap key exists on row 4 with code 0
    let row4 = &LAYOUT.rows[4];
    let has_inert_gap = row4.keys.iter().any(|k| k.code == 0 && k.width == 10);
    assert!(has_inert_gap, "row 4 must contain the 0.5u (width 10) code-0 gap key");
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
fn active_layouts_contracts() {
    use oskwin_core::layout::{ALL_LAYOUTS, LAYOUT_65, LAYOUT_NUMPAD};

    assert_eq!(ALL_LAYOUTS.len(), 2);
    assert_eq!(ALL_LAYOUTS[0].name, "65%");
    assert_eq!(ALL_LAYOUTS[1].name, "65%+Num");

    // 65% profile: 16.0u (320 units)
    for (i, row) in LAYOUT_65.rows.iter().enumerate() {
        let sum: u16 = row.keys.iter().map(|k| k.width).sum();
        assert_eq!(sum, 320, "65% row {} must sum to 320 (16.0u)", i);
    }

    // Combined 65%+Num profile: 20.5u (410 units)
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
    let suggs = p.suggestions();
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
    let suggs = p.suggestions();
    assert!(
        suggs.contains(&"hyprland"),
        "learned out-of-vocabulary word must be suggested for its prefix: {:?}",
        suggs
    );

    // The interner returns one canonical pointer per distinct word.
    use oskwin_core::predict::intern;
    assert!(std::ptr::eq(intern("hyprland"), intern("hyprland")));
    assert!(!std::ptr::eq(intern("hyprland"), intern("wayland")));
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

    let suggs = p.suggestions();
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

    // complete_word handles the committed contraction (empty remainder + space).
    let mut buf = [0 as char; 8];
    let n = complete_word("don't", "don't", false, false, &mut buf).unwrap();
    assert_eq!(n, 1);
    assert_eq!(buf[0], ' ');

    p.commit("don't");
    p.commit("");
    let suggs = p.suggestions();
    assert!(
        suggs.contains(&"don't"),
        "committed contraction must be learned and offered: {:?}",
        suggs
    );

    // A second apostrophe is not inside a letter run: it breaks the word.
    for c in "do".chars() {
        p.char(c);
    }
    p.char('\'');
    p.char('\'');
    assert_eq!(p.partial(), "");
}

#[test]
fn test_user_history_persistence_roundtrip() {
    let dir = std::env::temp_dir().join(format!("osk-history-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("user_history.tsv");

    let mut p = Predictor::ephemeral();
    p.commit("hyprland");
    p.commit("wayland");
    p.commit("hyprland");
    p.save_history_to(&path);

    let data = std::fs::read_to_string(&path).unwrap();
    assert!(
        data.lines().any(|l| l == "u\thyprland\t\t2"),
        "unigram TSV line with count 2: {data:?}"
    );
    assert!(
        data.lines().any(|l| l == "b\thyprland\twayland\t1"),
        "bigram TSV line: {data:?}"
    );

    let mut q = Predictor::ephemeral();
    q.load_history_from(&path);
    for c in "hyp".chars() {
        q.char(c);
    }
    assert!(
        q.suggestions().contains(&"hyprland"),
        "loaded history must drive prefix completion"
    );

    // Learned bigrams survive the roundtrip too.
    q.commit("wayland");
    assert!(
        q.suggestions().contains(&"hyprland"),
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
    let suggs = p.suggestions();
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
    let suggs = p.suggestions();
    assert!(
        suggs.contains(&"i") && suggs.contains(&"the"),
        "sentence starters must be offered after '.': {:?}",
        suggs
    );

    // A learned word also appears at sentence start.
    let mut q = Predictor::ephemeral();
    q.commit("hyprland");
    q.char('.');
    let suggs = q.suggestions();
    assert!(
        suggs.contains(&"hyprland"),
        "learned word must be offered at sentence start: {:?}",
        suggs
    );
}

#[test]
fn test_tab_suggestion_completion() {
    // The UI's Tab handler completes the word with suggestion 0 —
    // the same path as clicking the first pill.
    let mut p = Predictor::ephemeral();
    for c in "sys".chars() {
        p.char(c);
    }
    let suggs = p.suggestions();
    assert!(!suggs.is_empty(), "prefix 'sys' must have suggestions: {suggs:?}");
    let word = suggs[0];
    assert!(
        word.starts_with("sys"),
        "top suggestion must match the prefix: {suggs:?}"
    );

    // Tab -> click_suggestion(0) -> commit(word).
    p.commit(word);
    assert_eq!(p.partial(), "", "committing the suggestion clears the partial");
    let next = p.suggestions();
    assert!(!next.is_empty(), "after acceptance, next-word pills must be offered");

    // Next-word path (no partial): the primary pill is accepted too.
    let mut q = Predictor::ephemeral();
    q.commit("the");
    let suggs = q.suggestions();
    assert!(!suggs.is_empty(), "context must yield next-word suggestions");
    let word = suggs[0];
    q.commit(word);
    assert_eq!(q.partial(), "", "accepting a next-word pill clears the state");
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

static TEST_GLOBAL_ENGINE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[cfg(feature = "neural")]
#[test]
fn test_embedded_neural_engine_always_ready() {
    let _lock = TEST_GLOBAL_ENGINE_LOCK.lock();
    // The embedded weights make construction succeed with no download.
    let engine = oskwin_core::neural::NeuralEngine::new();
    assert!(
        engine.is_some(),
        "embedded model must make new() succeed without a download"
    );

    oskwin_core::neural::install(engine.unwrap());
    assert!(
        oskwin_core::neural::is_installed(),
        "freshly installed engine is active by default"
    );
    assert!(
        oskwin_core::neural::with_engine(|_| true).is_some(),
        "active engine is reachable"
    );

    // Toggling off disables the engine and makes it unreachable.
    let off = oskwin_core::neural::toggle();
    assert!(!off && !oskwin_core::neural::is_installed(), "toggle turns the engine off");
    assert!(
        oskwin_core::neural::with_engine(|_| true).is_none(),
        "disabled engine is unreachable"
    );

    // Toggling back on re-enables it.
    let on = oskwin_core::neural::toggle();
    assert!(on && oskwin_core::neural::is_installed(), "toggle turns the engine back on");

    // Leave it off so other tests' predictors don't hit the neural worker.
    let off = oskwin_core::neural::toggle();
    assert!(!off && !oskwin_core::neural::is_installed(), "engine ends off");
}

#[test]
fn test_neural_effectiveness() {
    let engine = match oskwin_core::neural::NeuralEngine::new() {
        Some(e) => e,
        None => return,
    };

    let test_prompts = [
        ("The quick brown fox jumps over the", "lazy"),
        ("How are", "you"),
        ("Thank you so", "much"),
        ("Let me know when you are", "ready"),
        ("Can you please check the", "status"),
        ("I will be back in a", "few"),
        ("What time is the", "meeting"),
        ("git commit -m", "fix"),
        ("Good morning everyone, hope you are having a great", "day"),
        ("See you", "later"),
    ];

    println!("\n=== NEURAL ENGINE EFFECTIVENESS BENCHMARK ===");
    let mut hits = 0;
    let total = test_prompts.len();

    for (prompt, expected_top) in test_prompts {
        let start = std::time::Instant::now();
        engine.request(prompt);

        let mut res = None;
        for _ in 0..300 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if let Some(words) = engine.poll() {
                res = Some(words);
                break;
            }
        }
        let elapsed = start.elapsed();
        let words = res.unwrap_or_default();
        let hit = words.iter().any(|w| w.eq_ignore_ascii_case(expected_top));
        if hit {
            hits += 1;
        }

        println!(
            "Prompt: {:<52} | Latency: {:>6.1}ms | Top Suggestions: {:<32} | Expected: {} ({})",
            format!("\"{}\"", prompt),
            elapsed.as_secs_f64() * 1000.0,
            format!("{:?}", words),
            expected_top,
            if hit { "HIT" } else { "MISS" }
        );
    }

    let acc = (hits as f64 / total as f64) * 100.0;
    println!("=== OVERALL ACCURACY: {}/{} ({:.1}%) ===\n", hits, total, acc);
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

    p.save_history_to(&path);
    let data = std::fs::read_to_string(&path).unwrap();
    assert!(data.lines().any(|l| l == "u\thyprland\t\t1"));
    assert!(data.lines().any(|l| l == "b\tthe\thyprland\t1"));
    assert!(data.lines().any(|l| l == "b\thyprland\twayland\t1"));

    // Forgetting removes the unigram and both bigrams from the TSV.
    assert!(p.forget_word("hyprland"));
    assert!(!p.forget_word("hyprland"));
    assert!(!p.is_learned("hyprland"));
    p.save_history_to(&path);
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
    p.save_history_to(&path);
    assert!(
        std::fs::read_to_string(&path).unwrap().is_empty(),
        "clear_learned leaves an empty TSV"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_incremental_decoding() {
    let engine = match oskwin_core::neural::NeuralEngine::new() {
        Some(e) => e,
        None => return,
    };

    // First request: prefill
    engine.request("The quick brown fox");
    let mut words1 = None;
    for _ in 0..600 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        if let Some(w) = engine.poll() {
            words1 = Some(w);
            break;
        }
    }
    assert!(words1.is_some(), "prefill request must produce predictions");

    // Second request: incremental extension (adds new tokens)
    let start = std::time::Instant::now();
    engine.request("The quick brown fox jumps over the");
    let mut words2 = None;
    for _ in 0..600 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        if let Some(w) = engine.poll() {
            words2 = Some(w);
            break;
        }
    }
    let elapsed = start.elapsed();
    assert!(words2.is_some(), "incremental extension must produce predictions");
    let words = words2.unwrap();
    assert!(
        words.contains(&"lazy"),
        "incremental decode must predict 'lazy': {words:?}"
    );
    println!("Incremental step latency: {:.1}ms", elapsed.as_secs_f64() * 1000.0);
}

#[test]
fn test_mid_word_typing_neural_filtering() {
    let engine = match oskwin_core::neural::NeuralEngine::new() {
        Some(e) => std::sync::Arc::new(e),
        None => return,
    };

    let mut p = Predictor::ephemeral().with_neural_engine(engine.clone());
    // Simulate committing words: "Good morning everyone, hope you are having a great"
    for word in ["good", "morning", "everyone", "hope", "you", "are", "having", "a", "great"] {
        p.commit(word);
    }

    // Wait for neural engine to answer the commit
    let mut words = None;
    for _ in 0..600 {
        std::thread::sleep(std::time::Duration::from_millis(10));
        if let Some(w) = engine.poll() {
            words = Some(w);
            break;
        }
    }
    println!("Neural words in test: {:?}", words);

    // Now type 'd' mid-word:
    p.char('d');
    let suggs = p.suggestions();
    assert!(
        !suggs.is_empty() && suggs[0] == "day",
        "mid-word 'd' must prioritize neural prediction 'day': {suggs:?}"
    );

    // Backspace and type 'w':
    p.backspace();
    p.char('w');
    let suggs = p.suggestions();
    assert!(
        suggs.contains(&"week") || suggs.contains(&"weekend"),
        "mid-word 'w' must prioritize neural prediction 'week'/'weekend': {suggs:?}"
    );
}


