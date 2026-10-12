//! Headless self-test for key mapping, event ordering, layout
//! invariants, prediction, and the modifier engine.
//!
//! Ported from `legacy/src/selftest.c`. Cases that exercised C
//! artifacts (NULL pointers, the dead sticky-slot API superseded by
//! the modifier engine) have no Rust equivalent and are omitted;
//! every live behavior case is present.

use crate::events::{
    complete_word, sticky_presses, tap_events, OskEvent, StickyMods, TapError, TAP_MAX_EVENTS,
};
use crate::key::{key, Key};
use crate::layout::LAYOUT;
use crate::mods::{
    mod_index, next_state, ModKeyState, ModifierEngine, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT,
};
use crate::predict::{Predictor, MAX_SUGG};
use crate::typekey::{type_string, CharMap, KeySink};

/// Result of one self-test run.
#[derive(Clone, Debug)]
pub struct Report {
    pub passed: usize,
    pub failures: Vec<String>,
}

impl Report {
    pub fn total(&self) -> usize {
        self.passed + self.failures.len()
    }

    pub fn ok(&self) -> bool {
        self.failures.is_empty()
    }

    /// Print failures to stderr and a summary to stdout, C-style.
    pub fn print(&self) {
        for f in &self.failures {
            eprintln!("FAIL {f}");
        }
        println!("selftest: {} of {} cases pass", self.passed, self.total());
    }
}

struct T {
    passed: usize,
    failures: Vec<String>,
}

impl T {
    fn check(&mut self, name: &str, cond: bool) {
        if cond {
            self.passed += 1;
        } else {
            self.failures.push(name.to_string());
        }
    }
}

/// Run every case. The char-mapping cases need a working xkbcommon;
/// without one they are all reported as failures.
pub fn run() -> Report {
    let mut t = T {
        passed: 0,
        failures: Vec::new(),
    };
    map_cases(&mut t);
    seq_cases(&mut t);
    input_cases(&mut t);
    layout_cases(&mut t);
    predict_cases(&mut t);
    mod_cases(&mut t);
    Report {
        passed: t.passed,
        failures: t.failures,
    }
}

// ---- fixtures ----

const K_A: Key = Key {
    label: Some("A"),
    code: key::KEY_A,
    letter: true,
    width: 10,
    repeat: true,
    toggle: false,
    moment: false,
    shift_label: None,
};
const K_C: Key = Key {
    label: Some("C"),
    code: key::KEY_C,
    letter: true,
    width: 10,
    repeat: true,
    toggle: false,
    moment: false,
    shift_label: None,
};
const K_1: Key = Key {
    label: Some("1"),
    code: key::KEY_1,
    letter: false,
    width: 10,
    repeat: true,
    toggle: false,
    moment: false,
    shift_label: Some("!"),
};
const K_LCTRL: Key = Key {
    label: Some("Ctrl"),
    code: key::KEY_LEFTCTRL,
    letter: false,
    width: 15,
    repeat: false,
    toggle: false,
    moment: true,
    shift_label: None,
};
const K_RCTRL: Key = Key {
    label: Some("Ctrl"),
    code: key::KEY_RIGHTCTRL,
    letter: false,
    width: 15,
    repeat: false,
    toggle: false,
    moment: true,
    shift_label: None,
};
const K_CAPS: Key = Key {
    label: Some("Caps"),
    code: key::KEY_CAPSLOCK,
    letter: false,
    width: 17,
    repeat: false,
    toggle: true,
    moment: false,
    shift_label: None,
};
const K_LSHIFT: Key = Key {
    label: Some("Shift"),
    code: key::KEY_LEFTSHIFT,
    letter: false,
    width: 27,
    repeat: false,
    toggle: true,
    moment: false,
    shift_label: None,
};
const K_RSHIFT: Key = Key {
    label: Some("Shift"),
    code: key::KEY_RIGHTSHIFT,
    letter: false,
    width: 27,
    repeat: false,
    toggle: true,
    moment: false,
    shift_label: None,
};

/// (char, expected KEY_*, expected shift) rows.
const MAP_CASES: [(char, u16, bool); 83] = [
    ('a', key::KEY_A, false),
    ('b', key::KEY_B, false),
    ('c', key::KEY_C, false),
    ('d', key::KEY_D, false),
    ('e', key::KEY_E, false),
    ('f', key::KEY_F, false),
    ('g', key::KEY_G, false),
    ('h', key::KEY_H, false),
    ('i', key::KEY_I, false),
    ('j', key::KEY_J, false),
    ('k', key::KEY_K, false),
    ('l', key::KEY_L, false),
    ('m', key::KEY_M, false),
    ('n', key::KEY_N, false),
    ('o', key::KEY_O, false),
    ('p', key::KEY_P, false),
    ('q', key::KEY_Q, false),
    ('r', key::KEY_R, false),
    ('s', key::KEY_S, false),
    ('t', key::KEY_T, false),
    ('u', key::KEY_U, false),
    ('v', key::KEY_V, false),
    ('w', key::KEY_W, false),
    ('x', key::KEY_X, false),
    ('y', key::KEY_Y, false),
    ('z', key::KEY_Z, false),
    ('A', key::KEY_A, true),
    ('B', key::KEY_B, true),
    ('C', key::KEY_C, true),
    ('D', key::KEY_D, true),
    ('E', key::KEY_E, true),
    ('F', key::KEY_F, true),
    ('G', key::KEY_G, true),
    ('H', key::KEY_H, true),
    ('I', key::KEY_I, true),
    ('J', key::KEY_J, true),
    ('K', key::KEY_K, true),
    ('L', key::KEY_L, true),
    ('M', key::KEY_M, true),
    ('N', key::KEY_N, true),
    ('O', key::KEY_O, true),
    ('P', key::KEY_P, true),
    ('Q', key::KEY_Q, true),
    ('R', key::KEY_R, true),
    ('S', key::KEY_S, true),
    ('T', key::KEY_T, true),
    ('U', key::KEY_U, true),
    ('V', key::KEY_V, true),
    ('W', key::KEY_W, true),
    ('X', key::KEY_X, true),
    ('Y', key::KEY_Y, true),
    ('Z', key::KEY_Z, true),
    ('0', key::KEY_0, false),
    ('1', key::KEY_1, false),
    ('2', key::KEY_2, false),
    ('3', key::KEY_3, false),
    ('4', key::KEY_4, false),
    ('5', key::KEY_5, false),
    ('6', key::KEY_6, false),
    ('7', key::KEY_7, false),
    ('8', key::KEY_8, false),
    ('9', key::KEY_9, false),
    ('-', key::KEY_MINUS, false),
    ('=', key::KEY_EQUAL, false),
    ('[', key::KEY_LEFTBRACE, false),
    (']', key::KEY_RIGHTBRACE, false),
    ('\\', key::KEY_BACKSLASH, false),
    (';', key::KEY_SEMICOLON, false),
    ('\'', key::KEY_APOSTROPHE, false),
    (',', key::KEY_COMMA, false),
    ('.', key::KEY_DOT, false),
    ('/', key::KEY_SLASH, false),
    ('~', key::KEY_GRAVE, true),
    ('_', key::KEY_MINUS, true),
    ('+', key::KEY_EQUAL, true),
    ('{', key::KEY_LEFTBRACE, true),
    ('}', key::KEY_RIGHTBRACE, true),
    ('|', key::KEY_BACKSLASH, true),
    (':', key::KEY_SEMICOLON, true),
    ('"', key::KEY_APOSTROPHE, true),
    ('<', key::KEY_COMMA, true),
    ('>', key::KEY_DOT, true),
    ('?', key::KEY_SLASH, true),
];

fn map_cases(t: &mut T) {
    let Some(map) = CharMap::new() else {
        for (c, _, _) in MAP_CASES {
            t.check(&format!("map {c} (no keymap)"), false);
        }
        return;
    };
    for (c, want_code, want_shift) in MAP_CASES {
        let got = map.char_to_key(c);
        t.check(
            &format!("map {c} -> ({want_code}, {want_shift})"),
            got == Some((want_code, want_shift)),
        );
    }
}

struct Rec {
    ev: Vec<(u16, bool)>,
}

impl KeySink for Rec {
    fn press(&mut self, code: u16) {
        self.ev.push((code, true));
    }
    fn release(&mut self, code: u16) {
        self.ev.push((code, false));
    }
}

fn seq_cases(t: &mut T) {
    let Some(map) = CharMap::new() else {
        for s in ["a", "A", "a1!", " "] {
            t.check(&format!("seq {s} (no keymap)"), false);
        }
        return;
    };
    let cases: [(&str, &[(u16, bool)]); 4] = [
        ("a", &[(key::KEY_A, true), (key::KEY_A, false)]),
        (
            "A",
            &[
                (key::KEY_LEFTSHIFT, true),
                (key::KEY_A, true),
                (key::KEY_A, false),
                (key::KEY_LEFTSHIFT, false),
            ],
        ),
        (
            "a1!",
            &[
                (key::KEY_A, true),
                (key::KEY_A, false),
                (key::KEY_1, true),
                (key::KEY_1, false),
                (key::KEY_LEFTSHIFT, true),
                (key::KEY_1, true),
                (key::KEY_1, false),
                (key::KEY_LEFTSHIFT, false),
            ],
        ),
        (" ", &[(key::KEY_SPACE, true), (key::KEY_SPACE, false)]),
    ];
    for (s, exp) in cases {
        let mut rec = Rec { ev: Vec::new() };
        let r = type_string(&mut rec, &map, s, || {});
        t.check(&format!("seq {s}"), r.is_ok() && rec.ev.as_slice() == exp);
    }
}

fn input_cases(t: &mut T) {
    // complete_word: cased remainder + trailing space
    let mut buf = [0 as char; 8];
    t.check(
        "cw and/a",
        complete_word("and", "a", false, false, &mut buf) == Some(3)
            && buf[0..3] == ['n', 'd', ' '],
    );
    t.check(
        "cw and/a shift",
        complete_word("and", "a", true, false, &mut buf) == Some(3) && buf[0..3] == ['N', 'd', ' '],
    );
    t.check(
        "cw and/a caps",
        complete_word("and", "a", false, true, &mut buf) == Some(3) && buf[0..3] == ['N', 'D', ' '],
    );
    t.check(
        "cw and/a shift+caps",
        complete_word("and", "a", true, true, &mut buf) == Some(3) && buf[0..3] == ['n', 'D', ' '],
    );
    t.check(
        "cw the/the",
        complete_word("the", "the", false, false, &mut buf) == Some(1) && buf[0] == ' ',
    );
    t.check(
        "cw no prefix",
        complete_word("and", "b", false, false, &mut buf).is_none(),
    );
    t.check(
        "cw small buffer",
        complete_word("and", "a", false, false, &mut [0 as char; 2]).is_none(),
    );
    let mut exact = [0 as char; 3];
    t.check(
        "cw exact buffer",
        complete_word("and", "a", false, false, &mut exact) == Some(3) && exact == ['n', 'd', ' '],
    );

    // tap_events: press/release order, shift wrap, caps rule at the boundary
    let mut ev = [OskEvent {
        code: 0,
        press: false,
    }; TAP_MAX_EVENTS];
    let empty = StickyMods::default();
    t.check(
        "tap a plain",
        tap_events(&K_A, false, false, &empty, &mut ev).is_ok_and(|n| {
            n == 2
                && ev[0]
                    == OskEvent {
                        code: key::KEY_A,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_A,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap a shift",
        tap_events(&K_A, true, false, &empty, &mut ev).is_ok_and(|n| {
            n == 4
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_A,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_A,
                        press: false,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap a caps",
        tap_events(&K_A, false, true, &empty, &mut ev).is_ok_and(|n| {
            n == 4
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_A,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_A,
                        press: false,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap a shift+caps",
        tap_events(&K_A, true, true, &empty, &mut ev).is_ok_and(|n| {
            n == 2
                && ev[0]
                    == OskEvent {
                        code: key::KEY_A,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_A,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap 1 caps no shift",
        tap_events(&K_1, false, true, &empty, &mut ev).is_ok_and(|n| {
            n == 2
                && ev[0]
                    == OskEvent {
                        code: key::KEY_1,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_1,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap 1 shift+caps",
        tap_events(&K_1, true, true, &empty, &mut ev).is_ok_and(|n| {
            n == 4
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_1,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_1,
                        press: false,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: false,
                    }
        }),
    );
    let all = StickyMods {
        ctrl: Some(key::KEY_LEFTCTRL),
        alt: Some(key::KEY_LEFTALT),
        meta: Some(key::KEY_LEFTMETA),
    };
    t.check(
        "tap c 3 mods",
        tap_events(&K_C, false, false, &all, &mut ev).is_ok_and(|n| {
            n == 8
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTCTRL,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_LEFTALT,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_LEFTMETA,
                        press: true,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_C,
                        press: true,
                    }
                && ev[4]
                    == OskEvent {
                        code: key::KEY_C,
                        press: false,
                    }
                && ev[5]
                    == OskEvent {
                        code: key::KEY_LEFTMETA,
                        press: false,
                    }
                && ev[7]
                    == OskEvent {
                        code: key::KEY_LEFTCTRL,
                        press: false,
                    }
        }),
    );
    let rmeta = StickyMods {
        ctrl: None,
        alt: None,
        meta: Some(key::KEY_RIGHTMETA),
    };
    t.check(
        "tap c rmeta+shift",
        tap_events(&K_C, true, false, &rmeta, &mut ev).is_ok_and(|n| {
            n == 6
                && ev[0]
                    == OskEvent {
                        code: key::KEY_RIGHTMETA,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_C,
                        press: true,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_C,
                        press: false,
                    }
                && ev[4]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: false,
                    }
                && ev[5]
                    == OskEvent {
                        code: key::KEY_RIGHTMETA,
                        press: false,
                    }
        }),
    );
    t.check(
        "tap cap too small",
        tap_events(
            &K_A,
            false,
            false,
            &empty,
            &mut [OskEvent {
                code: 0,
                press: false,
            }],
        ) == Err(TapError::TooSmall),
    );
    t.check(
        "tap full chord exact fit",
        tap_events(&K_A, true, false, &all, &mut ev).is_ok_and(|n| n == 10),
    );
    t.check(
        "tap full chord one short",
        tap_events(&K_A, true, false, &all, &mut ev[..9]) == Err(TapError::TooSmall),
    );

    // sticky_presses: the suggestion-click modifier path
    t.check(
        "sticky empty",
        empty.is_empty() && sticky_presses(&empty) == [0, 0, 0],
    );
    t.check(
        "sticky rmeta",
        rmeta.count() == 1 && sticky_presses(&rmeta)[0] == key::KEY_RIGHTMETA,
    );
    t.check(
        "sticky all",
        all.count() == 3
            && sticky_presses(&all) == [key::KEY_LEFTCTRL, key::KEY_LEFTALT, key::KEY_LEFTMETA],
    );
    let lctrl = StickyMods {
        ctrl: Some(key::KEY_LEFTCTRL),
        alt: None,
        meta: None,
    };
    t.check(
        "sticky lctrl only",
        lctrl.count() == 1 && sticky_presses(&lctrl)[0] == key::KEY_LEFTCTRL,
    );
}

fn layout_cases(t: &mut T) {
    let l65 = &LAYOUT;
    t.check("l65 shape", l65.nrows() == 5 && l65.rows[4].len() == 10);

    t.check(
        "row key counts",
        l65.rows.iter().all(|r| (1..100).contains(&r.len())),
    );

    // The 65% profile: every row sums to 320 (16.0u) so the edges line up.
    let sums: Vec<usize> = l65.rows.iter().map(|r| r.width() as usize).collect();
    let all_equal = sums.iter().all(|s| *s == sums[0]);
    t.check("l65 widths", all_equal && sums[0] == 320);

    let want0 = [
        key::KEY_ESC,
        key::KEY_1,
        key::KEY_2,
        key::KEY_3,
        key::KEY_4,
        key::KEY_5,
        key::KEY_6,
        key::KEY_7,
        key::KEY_8,
        key::KEY_9,
        key::KEY_0,
        key::KEY_MINUS,
        key::KEY_EQUAL,
        key::KEY_BACKSPACE,
        key::KEY_HOME,
    ];
    t.check(
        "l65 row0 codes",
        l65.rows[0].len() == 15
            && l65.rows[0].keys[13].width == 40
            && l65.rows[0].keys.iter().zip(want0).all(|(k, w)| k.code == w),
    );

    let want4 = [
        key::KEY_LEFTCTRL,
        key::KEY_LEFTMETA,
        key::KEY_LEFTALT,
        key::KEY_SPACE,
        key::KEY_RIGHTALT,
        key::KEY_RIGHTCTRL,
        0,
        key::KEY_LEFT,
        key::KEY_DOWN,
        key::KEY_RIGHT,
    ];
    t.check(
        "l65 row4 codes",
        l65.rows[4].len() == 10
            && l65.rows[4].keys[3].code == key::KEY_SPACE
            && l65.rows[4].keys[3].width == 125
            && l65.rows[4].keys[6].code == 0
            && l65.rows[4].keys[6].width == 10
            && l65.rows[4].keys.iter().zip(want4).all(|(k, w)| k.code == w),
    );

    // Shifts and the wide keys carry the measured widths.
    t.check(
        "l65 wide widths",
        l65.rows[1].keys[0].width == 30
            && l65.rows[1].keys[13].width == 30
            && l65.rows[2].keys[0].width == 35
            && l65.rows[2].keys[12].width == 45
            && l65.rows[3].keys[0].width == 45
            && l65.rows[3].keys[11].width == 35
            && l65.rows[4].keys[0].width == 25,
    );

    let arrows = [key::KEY_LEFT, key::KEY_UP, key::KEY_DOWN, key::KEY_RIGHT];
    let mut ok = true;
    for &a in &arrows {
        let k = l65
            .rows
            .iter()
            .flat_map(|r| r.keys.iter())
            .find(|k| k.code == a);
        match k {
            Some(k) if k.width == 20 && k.repeat && !k.letter && !k.toggle && !k.moment => {}
            _ => ok = false,
        }
    }
    t.check("l65 arrows", ok);

    let mut ok = true;
    for &a in &arrows {
        let n = l65
            .rows
            .iter()
            .flat_map(|r| r.keys.iter())
            .filter(|k| k.code == a)
            .count();
        if n != 1 {
            ok = false;
        }
    }
    t.check("l65 arrows unique", ok);
}

fn predict_cases(t: &mut T) {
    fn no_dups(row: &[&str]) -> bool {
        (0..row.len()).all(|i| (0..i).all(|j| row[i] != row[j]))
    }

    let mut p = Predictor::ephemeral();
    p.reset();
    let start = p.suggestions();
    t.check(
        "p1 start",
        !start.is_empty()
            && start.len() <= MAX_SUGG
            && no_dups(&start)
            && p.partial().is_empty(),
    );

    p.reset();
    p.char('t');
    let s = p.suggestions();
    t.check(
        "p2 t prefix",
        !s.is_empty() && s.iter().all(|w| w.starts_with('t')) && no_dups(&s),
    );

    p.reset();
    for c in "the".chars() {
        p.char(c);
    }
    p.space();
    let s = p.suggestions();
    t.check(
        "p3 the context",
        p.partial().is_empty() && !s.is_empty() && s.len() <= MAX_SUGG && no_dups(&s),
    );

    p.reset();
    for c in "the".chars() {
        p.char(c);
    }
    p.space();
    p.char('f');
    let s = p.suggestions();
    t.check(
        "p4 the f",
        !s.is_empty()
            && s.len() <= MAX_SUGG
            && s.iter().all(|w| w.starts_with('f'))
            && no_dups(&s),
    );

    p.commit("You!");
    t.check("p5 you context", !p.suggestions().is_empty());

    p.commit("you");
    p.char('a');
    let s = p.suggestions();
    t.check(
        "p6 you a",
        !s.is_empty() && s.iter().all(|w| w.starts_with('a')) && no_dups(&s),
    );

    p.commit("zygote");
    let s = p.suggestions();
    t.check(
        "p7 unknown word learned",
        s.contains(&"zygote"),
    );

    p.commit("the");
    p.space();
    let s = p.suggestions();
    t.check(
        "p8 bare space drops",
        p.partial().is_empty()
            && !s.is_empty()
            && s.len() <= MAX_SUGG
            && no_dups(&s)
            && s.contains(&"zygote"),
    );

    p.commit("the");
    p.char('f');
    p.backspace();
    let s = p.suggestions();
    t.check(
        "p9 backspace",
        p.partial().is_empty() && !s.is_empty() && no_dups(&s),
    );

    p.commit("thank");
    t.check("p10 thank", !p.suggestions().is_empty());

    p.commit("");
    let s = p.suggestions();
    t.check(
        "p11 empty commit",
        !s.is_empty() && s.len() <= MAX_SUGG && no_dups(&s) && s.contains(&"zygote"),
    );

    p.reset();
    p.commit("the");
    let a = p.suggestions();
    p.reset();
    p.commit("THE");
    t.check("p13 case fold", p.suggestions() == a);

    p.commit("the");
    p.break_word();
    let s = p.suggestions();
    t.check(
        "p14 break",
        p.partial().is_empty() && !s.is_empty() && s.len() <= MAX_SUGG && no_dups(&s),
    );

    p.commit("the");
    p.char(' ');
    let s = p.suggestions();
    t.check(
        "p15 space char breaks",
        p.partial().is_empty() && !s.is_empty() && s.len() <= MAX_SUGG && no_dups(&s),
    );

    // Two-word context: trigram takes priority over the bigram.
    p.reset();
    p.commit("i");
    p.commit("want");
    let s = p.suggestions();
    t.check(
        "p16 trigram context",
        !s.is_empty() && s.len() <= MAX_SUGG && no_dups(&s),
    );

    // Suggestions are stable for a given state.
    let again = p.suggestions();
    t.check("p17 stable", s == again);

    // Contractions: an apostrophe extends a letter run; a second one
    // breaks the word. Sentence punctuation offers the starter blend.
    p.reset();
    for c in "don".chars() {
        p.char(c);
    }
    p.char('\'');
    t.check("p18 contraction partial", p.partial() == "don'");
    p.char('\'');
    t.check("p19 double apostrophe breaks", p.partial().is_empty());
    p.char('.');
    t.check(
        "p20 sentence start",
        p.partial().is_empty() && p.suggestions().contains(&"i"),
    );

    // Learned-word management: learn, list, forget, clear.
    p.reset();
    p.commit("zygote");
    p.learn_word("quixotic", 4);
    let learned = p.learned_words();
    t.check(
        "p21 learned words listed",
        learned.iter().any(|l| l.word == "quixotic" && l.count == 4 && l.links == 0)
            && learned.iter().any(|l| l.word == "zygote" && l.links >= 1),
    );
    t.check(
        "p22 is_learned",
        p.is_learned("quixotic") && !p.is_learned("quasar"),
    );
    t.check(
        "p23 forget word",
        p.forget_word("quixotic") && !p.is_learned("quixotic"),
    );
    t.check("p24 forget unknown", !p.forget_word("quixotic"));
    p.clear_learned();
    t.check("p25 clear learned", p.learned_words().is_empty());
}

fn mod_cases(t: &mut T) {
    t.check(
        "m1 shift slots",
        mod_index(key::KEY_LEFTSHIFT) == Some(MOD_SHIFT)
            && mod_index(key::KEY_RIGHTSHIFT) == Some(MOD_SHIFT),
    );
    t.check(
        "m2 ctrl slots",
        mod_index(key::KEY_LEFTCTRL) == Some(MOD_CTRL)
            && mod_index(key::KEY_RIGHTCTRL) == Some(MOD_CTRL),
    );
    t.check(
        "m3 alt slots",
        mod_index(key::KEY_LEFTALT) == Some(MOD_ALT)
            && mod_index(key::KEY_RIGHTALT) == Some(MOD_ALT),
    );
    t.check(
        "m4 meta slots",
        mod_index(key::KEY_LEFTMETA) == Some(MOD_META)
            && mod_index(key::KEY_RIGHTMETA) == Some(MOD_META),
    );
    t.check(
        "m5 non-mods",
        mod_index(key::KEY_CAPSLOCK).is_none()
            && mod_index(key::KEY_A).is_none()
            && mod_index(key::KEY_SPACE).is_none(),
    );
    t.check(
        "m6 is_active",
        !ModKeyState::Off.is_active()
            && ModKeyState::Latched.is_active()
            && ModKeyState::Locked.is_active(),
    );
    t.check(
        "m7 off->latched",
        next_state(ModKeyState::Off, 1000, 0) == ModKeyState::Latched,
    );
    t.check(
        "m8 double click locks",
        next_state(ModKeyState::Latched, 1000 + 300, 1000) == ModKeyState::Locked,
    );
    t.check(
        "m9 slow click off",
        next_state(ModKeyState::Latched, 1000 + 500, 1000) == ModKeyState::Off,
    );
    t.check(
        "m10 locked->off",
        next_state(ModKeyState::Locked, 5000, 1000) == ModKeyState::Off,
    );

    let mut m = ModifierEngine::new();
    m.click(key::KEY_LEFTSHIFT, 0);
    m.click(key::KEY_LEFTCTRL, 0);
    m.click(key::KEY_LEFTCTRL, 100); // double-click: locked
    m.click(key::KEY_LEFTALT, 0);
    m.click(key::KEY_LEFTMETA, 0);
    m.click(key::KEY_LEFTMETA, 100);
    m.consume_latched();
    t.check(
        "m11 consume latched",
        m.state(MOD_SHIFT) == ModKeyState::Off
            && m.state(MOD_CTRL) == ModKeyState::Locked
            && m.state(MOD_ALT) == ModKeyState::Off
            && m.state(MOD_META) == ModKeyState::Locked,
    );

    let mut m = ModifierEngine::new();
    t.check("m12 shift off", !m.shift_active());
    m.click(key::KEY_LEFTSHIFT, 0);
    t.check("m13 shift latched", m.shift_active());
    m.click(key::KEY_LEFTSHIFT, 100);
    t.check("m14 shift locked", m.shift_active());

    // only the physical key that was latched/locked lights
    let mut m = ModifierEngine::new();
    t.check("m15 fresh off", m.key_visual(&K_LSHIFT) == ModKeyState::Off);
    m.click(key::KEY_LEFTSHIFT, 0);
    t.check(
        "m16 latched lights",
        m.key_visual(&K_LSHIFT) == ModKeyState::Latched,
    );
    t.check("m17 twin dark", m.key_visual(&K_RSHIFT) == ModKeyState::Off);
    m.click(key::KEY_LEFTSHIFT, 100);
    t.check(
        "m18 locked lights",
        m.key_visual(&K_LSHIFT) == ModKeyState::Locked,
    );
    m.click(key::KEY_LEFTSHIFT, 5000);
    m.click(key::KEY_LEFTCTRL, 0);
    m.click(key::KEY_LEFTCTRL, 100);
    t.check(
        "m19 ctrl locked",
        m.key_visual(&K_LCTRL) == ModKeyState::Locked,
    );
    t.check("m20 rctrl dark", m.key_visual(&K_RCTRL) == ModKeyState::Off);
    m.click(key::KEY_LEFTCTRL, 5000);
    m.toggle_caps();
    t.check("m21 caps on", m.key_visual(&K_CAPS) == ModKeyState::Locked);
    m.toggle_caps();
    t.check("m22 caps off", m.key_visual(&K_CAPS) == ModKeyState::Off);
    t.check("m23 letter off", m.key_visual(&K_A) == ModKeyState::Off);

    // symbol injection: shift on a number wraps KEY_LEFTSHIFT around KEY_1
    let mut ev = [OskEvent {
        code: 0,
        press: false,
    }; TAP_MAX_EVENTS];
    let empty = StickyMods::default();
    t.check(
        "m24 symbol shift wrap",
        tap_events(&K_1, true, false, &empty, &mut ev).is_ok_and(|n| {
            n == 4
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: true,
                    }
                && ev[1]
                    == OskEvent {
                        code: key::KEY_1,
                        press: true,
                    }
                && ev[2]
                    == OskEvent {
                        code: key::KEY_1,
                        press: false,
                    }
                && ev[3]
                    == OskEvent {
                        code: key::KEY_LEFTSHIFT,
                        press: false,
                    }
        }),
    );

    // chaining: ctrl + alt + meta + shift all active -> all held, in order
    let all = StickyMods {
        ctrl: Some(key::KEY_LEFTCTRL),
        alt: Some(key::KEY_LEFTALT),
        meta: Some(key::KEY_LEFTMETA),
    };
    t.check(
        "m25 full chord",
        tap_events(&K_A, true, false, &all, &mut ev).is_ok_and(|n| {
            n == 10
                && ev[0]
                    == OskEvent {
                        code: key::KEY_LEFTCTRL,
                        press: true,
                    }
                && ev[9]
                    == OskEvent {
                        code: key::KEY_LEFTCTRL,
                        press: false,
                    }
        }),
    );
}
