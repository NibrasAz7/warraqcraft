//! The `typeset` layer (L1) — boxes/glue and math layout built from
//! published algorithms only (The TeXbook's appendix-G math rules are
//! public; the implementation here is original).
//!
//! Milestone scope: a bounded math-fragment subset used by
//! `math.preview`'s native path — letters, digits, the common symbol
//! names, `^`/`_` scripts, `\frac`, `\sqrt`, and thin/medium/thick
//! atom spacing. Anything outside the subset returns `None` and the
//! caller falls back to the external engine — honest, never guessed.
//!
//! The crate measures nothing itself: every width goes through the
//! caller's `measure(char, size, style)` so it stays pure and
//! font-agnostic — desktop or wasm, no I/O, no panics.

#![forbid(unsafe_code)]

/// Font style tag — the caller maps these onto real fonts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// Italic — lone identifiers.
    Italic,
    /// Upright — digits, operators, named symbols.
    Regular,
}

/// A laid-out item in pt, baseline coordinates (origin bottom-left of
/// the fragment's own box).
#[derive(Debug, Clone)]
pub enum Item {
    /// Text run: chars share one style and size; `x`,`y` is the
    /// baseline origin of the first glyph.
    Text { x: f64, y: f64, size: f64, style: Style, text: String },
    /// Filled rule — fraction bars, radical overlines.
    Rule { x: f64, y: f64, w: f64, h: f64 },
}

/// A finished fragment: items plus its bounding metrics. `baseline` is
/// the item-space y of the text baseline — callers embedding a fragment
/// inside a line offset items by `-baseline`.
#[derive(Debug, Clone)]
pub struct Fragment {
    pub items: Vec<Item>,
    pub width: f64,
    pub height: f64,
    pub depth: f64,
    pub baseline: f64,
}

/// Atom classes (appendix G) — enough of the table for spacing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Ord,
    Bin,
    Rel,
    Inner, // punctuation/parens — thin-space behavior
}

/// One parsed atom: nucleus + optional scripts.
#[derive(Debug, Clone)]
enum Atom {
    /// A leaf glyph.
    Sym { ch: char, class: Class, italic: bool, sup: Option<Box<Atom>>, sub: Option<Box<Atom>> },
    /// `\frac{num}{den}`.
    Frac { num: Vec<Atom>, den: Vec<Atom>, sup: Option<Box<Atom>>, sub: Option<Box<Atom>> },
    /// `\sqrt{inner}`.
    Sqrt { inner: Vec<Atom>, sup: Option<Box<Atom>>, sub: Option<Box<Atom>> },
    /// A `{…}` group typesets as its contents (Ord).
    Group(Vec<Atom>),
    /// Explicit spacing: `\,` `\;` `\ ` `\!`.
    Space(f64),
}

/// Greek + symbol command names → their Unicode scalar. Bounded table
/// of the common names; anything absent makes the fragment fall back.
fn symbol_of(cmd: &str) -> Option<char> {
    Some(match cmd {
        "alpha" => 'α',
        "beta" => 'β',
        "gamma" => 'γ',
        "delta" => 'δ',
        "epsilon" => 'ϵ',
        "varepsilon" => 'ε',
        "zeta" => 'ζ',
        "eta" => 'η',
        "theta" => 'θ',
        "vartheta" => 'ϑ',
        "iota" => 'ι',
        "kappa" => 'κ',
        "lambda" => 'λ',
        "mu" => 'μ',
        "nu" => 'ν',
        "xi" => 'ξ',
        "pi" => 'π',
        "varpi" => 'ϖ',
        "rho" => 'ρ',
        "varrho" => 'ϱ',
        "sigma" => 'σ',
        "varsigma" => 'ς',
        "tau" => 'τ',
        "upsilon" => 'υ',
        "phi" => 'ϕ',
        "varphi" => 'φ',
        "chi" => 'χ',
        "psi" => 'ψ',
        "omega" => 'ω',
        "Gamma" => 'Γ',
        "Delta" => 'Δ',
        "Theta" => 'Θ',
        "Lambda" => 'Λ',
        "Xi" => 'Ξ',
        "Pi" => 'Π',
        "Sigma" => 'Σ',
        "Upsilon" => 'Υ',
        "Phi" => 'Φ',
        "Psi" => 'Ψ',
        "Omega" => 'Ω',
        "times" => '×',
        "div" => '÷',
        "pm" => '±',
        "mp" => '∓',
        "cdot" => '·',
        "ast" => '∗',
        "star" => '⋆',
        "circ" => '∘',
        "leq" | "le" => '≤',
        "geq" | "ge" => '≥',
        "neq" | "ne" => '≠',
        "approx" => '≈',
        "sim" => '∼',
        "simeq" => '≃',
        "equiv" => '≡',
        "propto" => '∝',
        "infty" => '∞',
        "partial" => '∂',
        "nabla" => '∇',
        "forall" => '∀',
        "exists" => '∃',
        "in" => '∈',
        "notin" => '∉',
        "subset" => '⊂',
        "supset" => '⊃',
        "subseteq" => '⊆',
        "supseteq" => '⊇',
        "cup" => '∪',
        "cap" => '∩',
        "vee" | "lor" => '∨',
        "wedge" | "land" => '∧',
        "oplus" => '⊕',
        "otimes" => '⊗',
        "perp" => '⊥',
        "parallel" => '∥',
        "to" | "rightarrow" => '→',
        "leftarrow" | "gets" => '←',
        "Rightarrow" => '⇒',
        "Leftarrow" => '⇐',
        "leftrightarrow" => '↔',
        "mapsto" => '↦',
        "uparrow" => '↑',
        "downarrow" => '↓',
        "sum" => '∑',
        "prod" => '∏',
        "int" => '∫',
        "oint" => '∮',
        "ldots" | "dots" => '…',
        "cdots" => '⋯',
        "aleph" => 'ℵ',
        "hbar" => 'ℏ',
        "ell" => 'ℓ',
        "Re" => 'ℜ',
        "Im" => 'ℑ',
        "wp" => '℘',
        "prime" => '′',
        "emptyset" => '∅',
        "neg" | "lnot" => '¬',
        "angle" => '∠',
        "degree" => '°',
        "imath" => 'ı',
        "jmath" => 'ȷ',
        _ => return None,
    })
}

/// Class of a raw character for spacing purposes.
fn char_class(c: char) -> Class {
    match c {
        '+' | '-' | '∗' | '⋆' | '∘' | '⊕' | '⊗' | '∪' | '∩' | '∨' | '∧' | '¬' | '·' | '×' | '÷' | '±' | '∓' => Class::Bin,
        '=' | '<' | '>' | '≤' | '≥' | '≠' | '≈' | '∼' | '≃' | '≡' | '∝' | '∈' | '∉' | '⊂' | '⊃' | '⊆' | '⊇' | '→' | '←' | '⇒' | '⇐' | '↔' | '↦'
        | '↑' | '↓' | '⊥' | '∥' => Class::Rel,
        ',' | ';' | ':' | '(' | ')' | '[' | ']' | '|' => Class::Inner,
        _ => Class::Ord,
    }
}

/// Space (in fractions of `size`) between adjacent atoms — thin 1/6 em
/// around Bin, thick 5/18 em around Rel, thin after Inner commas.
/// A compact reading of appendix G's spacing table for our classes.
fn sep(left: Class, right: Class, size: f64) -> f64 {
    let em = size;
    match (left, right) {
        (Class::Ord, Class::Bin) | (Class::Bin, Class::Ord) | (Class::Bin, Class::Bin) => em * (4.0 / 18.0),
        (Class::Bin, Class::Rel) | (Class::Rel, Class::Ord) | (Class::Ord, Class::Rel) | (Class::Rel, Class::Rel) => em * (5.0 / 18.0),
        (Class::Inner, _) | (Class::Rel, Class::Inner) | (Class::Bin, Class::Inner) => em * (1.0 / 6.0),
        _ => 0.0,
    }
}

/// Parse `src` into atoms. `None` on anything outside the subset —
/// callers fall back to the external engine, never guess.
fn parse(src: &str) -> Option<Vec<Atom>> {
    let chars: Vec<char> = src.chars().collect();
    let (atoms, pos) = parse_list(&chars, 0, None)?;
    if pos != chars.len() {
        return None;
    }
    Some(atoms)
}

/// Parse atoms until `stop` (a `}` byte char) or end of input; bounded
/// recursion keeps hostile nesting from exploding the stack.
fn plist(chars: &[char], pos: usize, stop: Option<char>, depth: usize) -> Option<(Vec<Atom>, usize)> {
    const MAX_DEPTH: usize = 32;
    if depth > MAX_DEPTH {
        return None;
    }
    let mut out = Vec::new();
    let mut i = pos;
    while i < chars.len() {
        let c = chars[i];
        if Some(c) == stop {
            return Some((out, i + 1));
        }
        match c {
            ' ' | '\t' | '\n' | '~' => i += 1,
            '}' => return Some((out, i)), // unmatched close — caller decides
            '^' | '_' => {
                i += 1;
                let script = parse_script(chars, &mut i, depth)?;
                let last = out.last_mut()?;
                attach_script(last, c == '^', script)?;
            }
            '$' => i += 1, // tolerate inline $…$ wrappers
            _ => {
                let (a, ni) = one_atom(chars, i, depth)?;
                out.push(a);
                i = ni;
            }
        }
    }
    Some((out, i))
}

fn parse_list(chars: &[char], pos: usize, stop: Option<char>) -> Option<(Vec<Atom>, usize)> {
    plist(chars, pos, stop, 0)
}

/// Exactly one atom starting at `i` — a group, a `\command` with its
/// arguments, a control symbol, or a single character. Script and
/// argument operands use this so a bare `^2` or `\frac12` never
/// swallows the rest of the stream.
fn one_atom(chars: &[char], i: usize, depth: usize) -> Option<(Atom, usize)> {
    match *chars.get(i)? {
        '{' => {
            let (g, ni) = plist(chars, i + 1, Some('}'), depth + 1)?;
            Some((Atom::Group(g), ni))
        }
        '\\' => {
            let mut i = i + 1;
            let ns = i;
            while i < chars.len() && chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i == ns {
                // Control symbol: `\,` `\;` `\!` `\ ` etc.
                let sym = *chars.get(i)?;
                i += 1;
                let sp = match sym {
                    ',' => 3.0 / 18.0,
                    ';' => 5.0 / 18.0,
                    ':' => 4.0 / 18.0,
                    '!' => -3.0 / 18.0,
                    ' ' | '~' => 0.5,
                    _ => return None,
                };
                return Some((Atom::Space(sp), i));
            }
            let name: String = chars[ns..i].iter().collect();
            match name.as_str() {
                "frac" | "dfrac" | "tfrac" | "cfrac" => {
                    let num = braced(chars, &mut i, depth)?;
                    let den = braced(chars, &mut i, depth)?;
                    Some((Atom::Frac { num, den, sup: None, sub: None }, i))
                }
                "sqrt" => {
                    // Optional [n] index is out of subset scope.
                    if chars.get(i) == Some(&'[') {
                        return None;
                    }
                    let inner_v = braced(chars, &mut i, depth)?;
                    Some((Atom::Sqrt { inner: inner_v, sup: None, sub: None }, i))
                }
                "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "left." | "right." => None,
                "mathrm" | "mathbf" | "text" | "operatorname" => {
                    // Upright group: contents as Ord Regular.
                    let g = braced(chars, &mut i, depth)?;
                    Some((Atom::Group(g.into_iter().map(force_regular).collect()), i))
                }
                _ => {
                    let ch = symbol_of(&name)?;
                    let class = match name.as_str() {
                        "sum" | "prod" | "int" | "oint" => Class::Inner,
                        _ => char_class(ch),
                    };
                    Some((Atom::Sym { ch, class, italic: false, sup: None, sub: None }, i))
                }
            }
        }
        c => {
            let italic = c.is_ascii_alphabetic();
            Some((Atom::Sym { ch: c, class: char_class(c), italic, sup: None, sub: None }, i + 1))
        }
    }
}

/// A script operand: `{…}` group or a single atom (char or \command).
fn parse_script(chars: &[char], i: &mut usize, depth: usize) -> Option<Vec<Atom>> {
    while chars.get(*i).is_some_and(|c| matches!(c, ' ' | '\t' | '\n' | '~')) {
        *i += 1;
    }
    match chars.get(*i) {
        Some('{') => {
            let (g, ni) = parse_list(chars, *i + 1, Some('}'))?;
            *i = ni;
            Some(g)
        }
        _ => {
            let (a, ni) = one_atom(chars, *i, depth + 1)?;
            *i = ni;
            Some(vec![a])
        }
    }
}

/// Attach a `^`/`_` script to an atom — scripts on bare Groups are
/// allowed by TeX; attach to a synthetic wrapper instead is fine too,
/// but simplest is to refuse groups and let the caller fall back.
fn attach_script(atom: &mut Atom, is_sup: bool, script: Vec<Atom>) -> Option<()> {
    let boxed = if script.len() == 1 { script.into_iter().next().map(Box::new) } else { Some(Box::new(Atom::Group(script))) };
    let slot = match atom {
        Atom::Sym { sup, sub, .. } | Atom::Frac { sup, sub, .. } | Atom::Sqrt { sup, sub, .. } => {
            if is_sup {
                sup
            } else {
                sub
            }
        }
        Atom::Group(_) | Atom::Space(_) => return None,
    };
    if slot.is_some() {
        return None; // double script — outside subset
    }
    *slot = boxed;
    Some(())
}

/// `{…}`-delimited argument, or a single bare atom when there's no
/// brace (TeX allows `\frac12`).
fn braced(chars: &[char], i: &mut usize, depth: usize) -> Option<Vec<Atom>> {
    match chars.get(*i) {
        Some('{') => {
            let (g, ni) = parse_list(chars, *i + 1, Some('}'))?;
            if chars.get(ni - 1) != Some(&'}') {
                return None;
            }
            let _ = depth;
            *i = ni;
            Some(g)
        }
        _ => {
            let (a, ni) = one_atom(chars, *i, depth + 1)?;
            *i = ni;
            Some(vec![a])
        }
    }
}

/// `force_regular` — `\mathrm{…}` children render upright.
fn force_regular(a: Atom) -> Atom {
    match a {
        Atom::Sym { ch, class, sup, sub, .. } => Atom::Sym { ch, class, italic: false, sup, sub },
        other => other,
    }
}

/// Layout context: measures glyphs and accumulates items.
struct Lay<'a> {
    measure: &'a dyn Fn(char, f64, Style) -> f64,
    items: Vec<Item>,
    x: f64,
    /// Track extremes so the fragment reports real metrics.
    top: f64,
    bottom: f64,
}

impl<'a> Lay<'a> {
    fn text(&mut self, ch: char, y: f64, size: f64, style: Style) -> f64 {
        let w = (self.measure)(ch, size, style);
        // Mergeable runs keep the item list small — same style/size/y
        // and adjacent x. Simplest correct first: push per glyph.
        self.items.push(Item::Text { x: self.x, y, size, style, text: ch.to_string() });
        self.top = self.top.max(y + size * 0.8);
        self.bottom = self.bottom.min(y);
        self.x += w;
        w
    }
}

/// Typeset one atom at baseline `y`, size `size`. Advances `lay.x`.
fn layout_atom(a: &Atom, lay: &mut Lay, y: f64, size: f64, prev: Option<Class>) -> Option<Class> {
    match a {
        Atom::Space(em) => {
            lay.x += em * size;
            Some(Class::Ord) // spacing separates atoms
        }
        a => {
            let cls = match a {
                Atom::Sym { class, .. } => *class,
                _ => Class::Ord,
            };
            if let Some(p) = prev {
                lay.x += sep(p, cls, size);
            }
            match a {
                Atom::Sym { ch, italic, sup, sub, .. } => {
                    let style = if *italic { Style::Italic } else { Style::Regular };
                    lay.text(*ch, y, size, style);
                    scripts(sup, sub, lay, y, size, ch_w(lay, *ch, size, style));
                }
                Atom::Group(g) => {
                    let mut p = prev;
                    for inner in g {
                        p = layout_atom(inner, lay, y, size, p);
                    }
                }
                Atom::Frac { num, den, sup, sub } => {
                    // Num above, den below a centered rule — appendix G
                    // rules 15a-c distilled: stack on the math axis.
                    let gap = size * 0.15;
                    let axis = size * 0.25;
                    let num_w = width_of(num, lay, size * 0.9);
                    let den_w = width_of(den, lay, size * 0.9);
                    let w = num_w.max(den_w).max(size * 0.6);
                    let x0 = lay.x;
                    lay.items.push(Item::Rule { x: x0, y: y + axis - 0.4, w, h: 0.8 });
                    // Numerator centered, baseline raised above the rule.
                    let num_y = y + axis + gap + size * 0.5;
                    lay.x = x0 + (w - num_w) / 2.0;
                    let mut p = None;
                    for n in num {
                        p = layout_atom(n, lay, num_y, size * 0.9, p);
                    }
                    let den_y = y + axis - gap - size * 0.62;
                    lay.x = x0 + (w - den_w) / 2.0;
                    let mut p = None;
                    for d in den {
                        p = layout_atom(d, lay, den_y, size * 0.9, p);
                    }
                    lay.top = lay.top.max(num_y + size * 0.72);
                    lay.bottom = lay.bottom.min(den_y);
                    lay.x = x0 + w;
                    scripts(sup, sub, lay, y, size, w);
                }
                Atom::Sqrt { inner, sup, sub } => {
                    let inner_w = width_of(inner, lay, size);
                    let rad = '\u{221A}';
                    let rad_w = ch_w(lay, rad, size, Style::Regular);
                    lay.text(rad, y, size, Style::Regular);
                    let body_y = y;
                    lay.items.push(Item::Rule { x: lay.x, y: body_y + size * 0.95, w: inner_w + size * 0.15, h: 0.6 });
                    for n in inner {
                        layout_atom(n, lay, body_y, size, None);
                    }
                    lay.top = lay.top.max(body_y + size * 1.05);
                    scripts(sup, sub, lay, y, size, rad_w + inner_w);
                }
                Atom::Space(_) => {}
            }
            Some(cls)
        }
    }
}

/// Width of an atom list without emitting items — typeset into a
/// scratch Lay.
fn width_of(atoms: &[Atom], lay: &Lay, size: f64) -> f64 {
    let mut scratch = Lay { measure: lay.measure, items: Vec::new(), x: 0.0, top: f64::MIN, bottom: f64::MAX };
    let mut p = None;
    for a in atoms {
        p = layout_atom(a, &mut scratch, 0.0, size, p);
    }
    scratch.x
}

/// `^`/`_` scripts: 70% size, raised/dropped — appendix G's script
/// placement distilled (x-height approximated by size/2). Both scripts
/// start at the same column; the pen advances past the widest.
fn scripts(sup: &Option<Box<Atom>>, sub: &Option<Box<Atom>>, lay: &mut Lay, y: f64, size: f64, _base_w: f64) {
    let s_size = size * 0.7;
    let x = lay.x;
    let mut end = x;
    if let Some(s) = sup {
        lay.x = x;
        let mut p = None;
        for a in std::slice::from_ref(s.as_ref()) {
            p = layout_atom(a, lay, y + size * 0.55, s_size, p);
        }
        lay.top = lay.top.max(y + size * 0.55 + s_size * 0.8);
        end = end.max(lay.x);
    }
    if let Some(s) = sub {
        lay.x = x;
        let mut p = None;
        for a in std::slice::from_ref(s.as_ref()) {
            p = layout_atom(a, lay, y - size * 0.3, s_size, p);
        }
        lay.bottom = lay.bottom.min(y - size * 0.3);
        end = end.max(lay.x);
    }
    lay.x = end;
}

/// Measure one char through the caller.
fn ch_w(lay: &Lay, ch: char, size: f64, style: Style) -> f64 {
    (lay.measure)(ch, size, style)
}

/// Typeset a math fragment to positioned items. `size` is the base
/// font size in pt; `measure(c, size, style)` → advance width in pt.
/// Returns `None` when the source uses constructs outside the subset —
/// callers then take the external-engine path.
pub fn math(src: &str, size: f64, measure: &dyn Fn(char, f64, Style) -> f64) -> Option<Fragment> {
    if src.chars().count() > 4096 {
        return None;
    }
    let atoms = parse(src)?;
    let mut lay = Lay { measure, items: Vec::new(), x: 0.0, top: 0.0, bottom: 0.0 };
    let mut prev = None;
    for a in &atoms {
        prev = layout_atom(a, &mut lay, 0.0, size, prev);
    }
    if lay.items.is_empty() {
        return None;
    }
    // Shift so the fragment's bounding box sits at positive y.
    let depth = -lay.bottom.min(0.0);
    let height = lay.top.max(size * 0.7);
    for it in &mut lay.items {
        match it {
            Item::Text { y, .. } => *y += depth,
            Item::Rule { y, .. } => *y += depth,
        }
    }
    Some(Fragment { items: lay.items, width: lay.x, height: height + depth, depth: 0.0, baseline: depth })
}

// ---------- text mode: paragraphs and line breaking ----------

/// Vertical material the caller stacks: a `Line` advances by the
/// leading, a `Gap` by its own amount.
#[derive(Debug)]
pub enum Block {
    Line(Fragment),
    Gap(f64),
}

/// One horizontal unit inside a paragraph.
#[derive(Debug)]
enum Unit {
    /// A run of styled chars that never breaks internally.
    Word(Vec<(char, Style)>),
    /// Inline math — an opaque box of fixed width.
    Math(Fragment),
    /// Interword glue; `breakable` marks legal break points.
    Glue { breakable: bool },
    /// Forced break (`\\`, `\newline`).
    Break,
}

/// A parse event: inline content, a paragraph boundary, or a heading.
enum Ev {
    Unit(Unit),
    Par,
    Heading(Vec<Unit>, f64),
}

const MAX_DOC: usize = 512 * 1024;
const MAX_UNITS: usize = 8192;

/// Push a space — collapses onto a preceding glue/break so runs of
/// whitespace stay one unit.
fn push_glue(units: &mut Vec<Unit>, breakable: bool) {
    match units.last() {
        Some(Unit::Glue { .. } | Unit::Break) | None => {}
        _ => units.push(Unit::Glue { breakable }),
    }
}

/// Append a char to the trailing Word, or start one.
fn push_char(units: &mut Vec<Unit>, c: char, style: Style) {
    match units.last_mut() {
        Some(Unit::Word(chars)) => chars.push((c, style)),
        _ => units.push(Unit::Word(vec![(c, style)])),
    }
}

fn push_word(units: &mut Vec<Unit>, s: &str) {
    for c in s.chars() {
        push_char(units, c, Style::Regular);
    }
}

/// All Word chars in the list switch face (`\emph` → italic).
fn restyle(units: &mut [Unit], style: Style) {
    for u in units.iter_mut() {
        if let Unit::Word(chars) = u {
            for c in chars.iter_mut() {
                c.1 = style;
            }
        }
    }
}

/// Parse one braced argument `{…}` into units — the opening brace
/// must be at `chars[*i]`. Shared by `\emph`, `\section`, bare groups.
fn braced_units(chars: &[char], i: &mut usize, size: f64, measure: &dyn Fn(char, f64, Style) -> f64, depth: usize) -> Option<Vec<Unit>> {
    if chars.get(*i) != Some(&'{') {
        return None;
    }
    let (evs, ni) = stream(chars, *i + 1, Some('}'), size, measure, depth + 1, false)?;
    *i = ni;
    let mut out = Vec::new();
    for e in evs {
        match e {
            Ev::Unit(u) => out.push(u),
            _ => return None, // no paragraph events inside braces
        }
    }
    Some(out)
}

/// Parse a unit/event stream until `stop` or end of input. `top` is
/// true only at document level, where blank lines and `\par` end a
/// paragraph and `\section` family becomes a heading. `None` on any
/// construct outside the subset — callers fall back.
fn stream(
    chars: &[char],
    pos: usize,
    stop: Option<char>,
    size: f64,
    measure: &dyn Fn(char, f64, Style) -> f64,
    depth: usize,
    top: bool,
) -> Option<(Vec<Ev>, usize)> {
    if depth > 16 {
        return None;
    }
    let mut evs: Vec<Ev> = Vec::new();
    let mut units: Vec<Unit> = Vec::new();
    let mut i = pos;
    while i < chars.len() {
        if units.len() > MAX_UNITS {
            return None;
        }
        let c = chars[i];
        if Some(c) == stop {
            break;
        }
        match c {
            '%' => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            ' ' | '\t' | '\r' => {
                push_glue(&mut units, true);
                i += 1;
            }
            '\n' => {
                // A blank line — newline, then only whitespace/comments
                // up to a second newline — ends the paragraph at top.
                let mut j = i + 1;
                let blank = loop {
                    while j < chars.len() && matches!(chars[j], ' ' | '\t' | '\r') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == '%' {
                        while j < chars.len() && chars[j] != '\n' {
                            j += 1;
                        }
                        continue;
                    }
                    break j < chars.len() && chars[j] == '\n';
                };
                if top && blank && units.iter().any(|u| !matches!(u, Unit::Glue { .. } | Unit::Break)) {
                    evs.extend(units.drain(..).map(Ev::Unit));
                    evs.push(Ev::Par);
                    i = j + 1;
                } else {
                    push_glue(&mut units, true);
                    i = if blank { j } else { i + 1 };
                }
            }
            '~' => {
                push_glue(&mut units, false);
                i += 1;
            }
            '{' => {
                let inner = braced_units(chars, &mut i, size, measure, depth)?;
                units.extend(inner);
            }
            '}' => {
                stop?; // stray close at top level
                break;
            }
            '$' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != '$' && j - i <= 4096 {
                    j += 1;
                }
                if chars.get(j) != Some(&'$') {
                    return None;
                }
                let inner: String = chars[i + 1..j].iter().collect();
                units.push(Unit::Math(math(&inner, size, measure)?));
                i = j + 1;
            }
            '\\' => {
                i += 1;
                let ns = i;
                while i < chars.len() && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                if i == ns {
                    let sym = *chars.get(i)?;
                    i += 1;
                    match sym {
                        '\\' => units.push(Unit::Break),
                        ' ' | '~' => push_glue(&mut units, false),
                        '&' | '%' | '$' | '#' | '_' | '{' | '}' => push_char(&mut units, sym, Style::Regular),
                        ',' | ';' | ':' | '!' => push_glue(&mut units, false),
                        _ => return None,
                    }
                    continue;
                }
                let name: String = chars[ns..i].iter().collect();
                match name.as_str() {
                    "par" if top => {
                        if units.iter().any(|u| !matches!(u, Unit::Glue { .. } | Unit::Break)) {
                            evs.extend(units.drain(..).map(Ev::Unit));
                            evs.push(Ev::Par);
                        }
                    }
                    "section" | "subsection" | "subsubsection" if top => {
                        let f = if name == "section" {
                            1.4
                        } else if name == "subsection" {
                            1.2
                        } else {
                            1.1
                        };
                        if units.iter().any(|u| !matches!(u, Unit::Glue { .. } | Unit::Break)) {
                            evs.extend(units.drain(..).map(Ev::Unit));
                            evs.push(Ev::Par);
                        }
                        let title = braced_units(chars, &mut i, size, measure, depth)?;
                        evs.push(Ev::Heading(title, f));
                    }
                    "emph" | "textit" | "textsl" => {
                        let mut inner = braced_units(chars, &mut i, size, measure, depth)?;
                        restyle(&mut inner, Style::Italic);
                        units.append(&mut inner);
                    }
                    "textbf" | "textrm" | "text" => {
                        let inner = braced_units(chars, &mut i, size, measure, depth)?;
                        units.extend(inner);
                    }
                    "ldots" | "dots" => push_char(&mut units, '…', Style::Regular),
                    "LaTeX" => push_word(&mut units, "LaTeX"),
                    "TeX" => push_word(&mut units, "TeX"),
                    "newline" => units.push(Unit::Break),
                    _ => return None,
                }
            }
            _ => {
                push_char(&mut units, c, Style::Regular);
                i += 1;
            }
        }
    }
    match stop {
        Some(sc) if chars.get(i) == Some(&sc) => i += 1,
        Some(_) => return None, // ran out of input with an open brace
        None => {}
    }
    evs.extend(units.drain(..).map(Ev::Unit));
    Some((evs, i))
}

/// Natural width of a unit at `size`.
fn unit_w(u: &Unit, size: f64, space_w: f64, measure: &dyn Fn(char, f64, Style) -> f64) -> f64 {
    match u {
        Unit::Word(cs) => cs.iter().map(|&(c, s)| measure(c, size, s)).sum(),
        Unit::Math(f) => f.width,
        Unit::Glue { .. } => space_w,
        Unit::Break => 0.0,
    }
}

/// Shared line-breaking parameters — kept in one place because the
/// breaker and emitter both need them.
struct ParaCfg<'a> {
    size: f64,
    width: f64,
    space_w: f64,
    measure: &'a dyn Fn(char, f64, Style) -> f64,
}

/// Break `seg` (no `Break` units inside) into lines ≤ `width` using a
/// total-fit pass — the published Knuth–Plass idea: breakpoints at
/// glue, demerits = badness², glue stretches ½ and shrinks ⅓ of its
/// natural width; the last line is ragged (infinite stretch). Greedy
/// fallback keeps pathological input typesettable. Returns
/// (start, content_end, justified) triples indexing into `seg`.
fn break_lines(seg: &[Unit], cfg: &ParaCfg, ragged: bool) -> Vec<(usize, usize, bool)> {
    let (size, width, space_w, measure) = (cfg.size, cfg.width, cfg.space_w, cfg.measure);
    let n = seg.len();
    let mut ends: Vec<usize> = (0..n).filter(|&j| matches!(seg[j], Unit::Glue { breakable: true })).map(|j| j + 1).collect();
    ends.push(n);
    let stretch = space_w * 0.5;
    let shrink = space_w * (1.0 / 3.0);
    let mut cost = vec![f64::INFINITY; n + 1];
    let mut prev = vec![usize::MAX; n + 1];
    cost[0] = 0.0;
    for (ei, &e) in ends.iter().enumerate() {
        let last_line = e == n;
        let mut ce = e;
        while ce > 0 && matches!(seg.get(ce - 1), Some(Unit::Glue { .. } | Unit::Break)) {
            ce -= 1;
        }
        // Starts: 0 and every earlier candidate end, newest first so we
        // can stop once the line is too wide to shrink in.
        let mut starts: Vec<usize> = std::iter::once(0).chain(ends[..ei].iter().copied()).collect();
        starts.reverse();
        for &s in &starts {
            if !cost[s].is_finite() {
                continue;
            }
            let mut w = 0.0;
            let mut glues = 0usize;
            for u in &seg[s..ce] {
                match u {
                    Unit::Glue { .. } => {
                        w += space_w;
                        glues += 1;
                    }
                    _ => w += unit_w(u, size, space_w, measure),
                }
            }
            if w <= f64::EPSILON {
                continue;
            }
            let stretch_cap = glues as f64 * stretch;
            let shrink_cap = glues as f64 * shrink;
            let badness = if last_line || ragged {
                if w <= width {
                    0.0
                } else if shrink_cap <= f64::EPSILON {
                    10_000.0 // overfull unshrinkable — still emittable
                } else {
                    ((w - width) / shrink_cap * 100.0).min(10_000.0)
                }
            } else if w > width + shrink_cap {
                break; // only gets wider for earlier starts
            } else if w > width {
                ((w - width) / shrink_cap.max(f64::EPSILON) * 100.0).min(10_000.0)
            } else if stretch_cap <= f64::EPSILON {
                if width - w <= f64::EPSILON {
                    0.0
                } else {
                    10_000.0
                }
            } else {
                let need = width - w;
                if need > stretch_cap * 3.0 {
                    continue; // looser than 3× stretch — infeasible
                }
                (need / stretch_cap * 100.0).min(10_000.0)
            };
            let dem = cost[s] + 1.0 + badness * badness;
            if dem < cost[e] {
                cost[e] = dem;
                prev[e] = s;
            }
        }
    }
    let mut cuts: Vec<usize> = Vec::new();
    if cost[n].is_finite() {
        let mut e = n;
        while e > 0 {
            cuts.push(e);
            e = prev[e];
        }
        cuts.reverse();
    } else {
        let mut s = 0usize;
        for &e in &ends {
            if e > s {
                cuts.push(e);
                s = e;
            }
        }
    }
    let mut lines = Vec::new();
    let mut s = 0usize;
    for (li, &e) in cuts.iter().enumerate() {
        let mut ce = e;
        while ce > s && matches!(seg.get(ce - 1), Some(Unit::Glue { .. } | Unit::Break)) {
            ce -= 1;
        }
        let justify = !ragged && li + 1 != cuts.len();
        lines.push((s, ce, justify));
        s = e;
    }
    lines
}

/// Emit one line's items at baseline y=0. Justified lines distribute
/// the leftover across glue; ragged lines take natural widths.
fn emit_line(seg: &[Unit], s: usize, ce: usize, cfg: &ParaCfg, justify: bool) -> Fragment {
    let (size, width, space_w, measure) = (cfg.size, cfg.width, cfg.space_w, cfg.measure);
    let mut w = 0.0;
    let mut glues = 0usize;
    for u in &seg[s..ce] {
        match u {
            Unit::Glue { .. } => {
                w += space_w;
                glues += 1;
            }
            _ => w += unit_w(u, size, space_w, measure),
        }
    }
    let extra = if justify && glues > 0 { ((width - w) / glues as f64).clamp(-space_w * (1.0 / 3.0), space_w * 1.5) } else { 0.0 };
    let mut lay = Lay { measure, items: Vec::new(), x: 0.0, top: 0.0, bottom: 0.0 };
    for u in &seg[s..ce] {
        match u {
            Unit::Word(cs) => {
                let mut run_style: Option<Style> = None;
                let mut run_x = 0.0;
                let mut run = String::new();
                for &(c, st) in cs {
                    if run_style != Some(st) {
                        if !run.is_empty() {
                            lay.items.push(Item::Text {
                                x: run_x,
                                y: 0.0,
                                size,
                                style: run_style.unwrap_or(Style::Regular),
                                text: std::mem::take(&mut run),
                            });
                        }
                        run_style = Some(st);
                        run_x = lay.x;
                    }
                    run.push(c);
                    lay.x += measure(c, size, st);
                }
                if !run.is_empty() {
                    lay.items.push(Item::Text { x: run_x, y: 0.0, size, style: run_style.unwrap_or(Style::Regular), text: run });
                }
                lay.top = lay.top.max(size * 0.75);
                lay.bottom = lay.bottom.min(-size * 0.25);
            }
            Unit::Math(f) => {
                let x0 = lay.x;
                for it in &f.items {
                    match it {
                        Item::Text { x, y, size: sz, style, text } => {
                            lay.items.push(Item::Text { x: x0 + x, y: y - f.baseline, size: *sz, style: *style, text: text.clone() });
                        }
                        Item::Rule { x, y, w: rw, h } => {
                            lay.items.push(Item::Rule { x: x0 + x, y: y - f.baseline, w: *rw, h: *h });
                        }
                    }
                }
                lay.top = lay.top.max(f.height - f.baseline);
                lay.bottom = lay.bottom.min(-f.baseline);
                lay.x = x0 + f.width;
            }
            Unit::Glue { .. } => lay.x += space_w + extra,
            Unit::Break => {}
        }
    }
    Fragment { items: lay.items, width: lay.x, height: lay.top.max(size * 0.7), depth: -lay.bottom.min(0.0), baseline: 0.0 }
}

/// Emit a paragraph's lines (split at forced breaks) plus a trailing
/// gap.
fn emit_para(units: &mut Vec<Unit>, out: &mut Vec<Block>, cfg: &ParaCfg) {
    let mut seg_start = 0usize;
    for k in 0..=units.len() {
        if k == units.len() || matches!(units[k], Unit::Break) {
            let mut a = seg_start;
            let mut b = k;
            while a < b && matches!(units[a], Unit::Glue { .. }) {
                a += 1;
            }
            while b > a && matches!(units[b - 1], Unit::Glue { .. }) {
                b -= 1;
            }
            if a < b {
                for (s, ce, justify) in break_lines(&units[a..b], cfg, false) {
                    out.push(Block::Line(emit_line(&units[a..b], s, ce, cfg, justify)));
                }
            }
            seg_start = k + 1;
        }
    }
    units.clear();
    if matches!(out.last(), Some(Block::Line(_))) {
        out.push(Block::Gap(cfg.size * 0.45));
    }
}

/// Typeset a document body — paragraphs, `\section` headings, styled
/// groups and `$…$` inline math — into stackable lines. The preamble
/// before `\begin{document}` is skipped; anything else outside the
/// subset returns `None` and the caller falls back to the external
/// engine. All widths flow through `measure`.
pub fn doc(src: &str, size: f64, width: f64, measure: &dyn Fn(char, f64, Style) -> f64) -> Option<Vec<Block>> {
    if src.len() > MAX_DOC {
        return None;
    }
    const DOC_OPEN: &str = "\\begin{document}";
    const DOC_CLOSE: &str = "\\end{document}";
    let body = match src.find(DOC_OPEN) {
        Some(i) => {
            let rest = src.get(i + DOC_OPEN.len()..)?;
            match rest.find(DOC_CLOSE) {
                Some(j) => rest.get(..j)?,
                None => rest,
            }
        }
        None => src,
    };
    let chars: Vec<char> = body.chars().collect();
    let (evs, used) = stream(&chars, 0, None, size, measure, 0, true)?;
    if used != chars.len() {
        return None;
    }
    let cfg = ParaCfg { size, width, space_w: measure(' ', size, Style::Regular).max(size * 0.2), measure };
    let mut out = Vec::new();
    let mut para: Vec<Unit> = Vec::new();
    for ev in evs {
        match ev {
            Ev::Unit(u) => para.push(u),
            Ev::Par => emit_para(&mut para, &mut out, &cfg),
            Ev::Heading(units, f) => {
                emit_para(&mut para, &mut out, &cfg);
                let hs = size * f;
                let hcfg = ParaCfg { size: hs, width, space_w: measure(' ', hs, Style::Regular).max(hs * 0.2), measure };
                if !units.is_empty() {
                    out.push(Block::Gap(hs * 0.7));
                    for (s, ce, _) in break_lines(&units, &hcfg, true) {
                        out.push(Block::Line(emit_line(&units, s, ce, &hcfg, false)));
                    }
                    out.push(Block::Gap(hs * 0.35));
                }
            }
        }
    }
    emit_para(&mut para, &mut out, &cfg);
    if out.iter().all(|b| matches!(b, Block::Gap(_))) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constant metric — unit tests don't need a real font.
    fn fixed(_c: char, size: f64, _s: Style) -> f64 {
        size * 0.5
    }

    #[test]
    fn plain_math_lays_out() {
        let f = math("x+y", 10.0, &fixed).unwrap();
        assert!(f.width > 0.0);
        assert!(!f.items.is_empty());
    }

    #[test]
    fn scripts_and_frac() {
        assert!(math("x^2", 10.0, &fixed).is_some());
        assert!(math("x_i", 10.0, &fixed).is_some());
        assert!(math("x^2+y", 10.0, &fixed).is_some());
        assert!(math(r"\frac12 + 3", 10.0, &fixed).is_some());
        assert!(math(r"\frac{a}{b}", 10.0, &fixed).is_some());
        assert!(math(r"e^{i\pi}+1=0", 10.0, &fixed).is_some());
        assert!(math(r"\sqrt{x}", 10.0, &fixed).is_some());
    }

    #[test]
    fn greek_names_resolve() {
        assert!(math(r"\alpha + \Omega", 10.0, &fixed).is_some());
    }

    #[test]
    fn outside_subset_falls_back() {
        assert!(math(r"\begin{matrix}a\end{matrix}", 10.0, &fixed).is_none());
        assert!(math(r"\unknownmacro", 10.0, &fixed).is_none());
        assert!(math(r"\includegraphics{x.png}", 10.0, &fixed).is_none());
    }

    #[test]
    fn hostile_never_panics() {
        for junk in ["", "^", "_", "{", "}", "{{{{{{{{", "\\frac", "\\frac{", "x^{", "\\\\", "\\frac{a}", "^{^{^{", "x^{{"] {
            let _ = math(junk, 10.0, &fixed);
        }
        // Deep nesting is bounded, not recursive-blowup.
        let deep = "{".repeat(200);
        let _ = math(&deep, 10.0, &fixed);
    }

    #[test]
    fn paragraph_wraps_to_width() {
        // 20 words of 5 chars + 4 glue = 20*5*5 + 19*5 = 595pt at 10pt
        // fixed metrics → must break into several ≤120pt lines.
        let src = "one two three four five six seven eight nine ten ".repeat(4);
        let blocks = doc(&src, 10.0, 120.0, &fixed).unwrap();
        let lines: Vec<&Fragment> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Line(f) => Some(f),
                _ => None,
            })
            .collect();
        assert!(lines.len() >= 4, "{} lines", lines.len());
        for l in &lines {
            assert!(!l.items.is_empty());
        }
    }

    #[test]
    fn section_and_inline_math() {
        let src = "\\documentclass{article}\n\\usepackage{amsmath}\n\\begin{document}\n\\section{Intro}\nWe see $x^2 + y$ here.\n\\end{document}\n";
        let blocks = doc(src, 10.0, 400.0, &fixed).unwrap();
        assert!(blocks.iter().any(|b| matches!(b, Block::Line(_))));
    }

    #[test]
    fn doc_outside_subset_falls_back() {
        assert!(doc("\\begin{document}\\begin{itemize}\\item x\\end{itemize}\\end{document}", 10.0, 400.0, &fixed).is_none());
        assert!(doc("text \\cite{key} more", 10.0, 400.0, &fixed).is_none());
        assert!(doc("unclosed { group", 10.0, 400.0, &fixed).is_none());
    }

    #[test]
    fn doc_hostile_never_panics() {
        for junk in ["", "\\section", "\\section{", "$", "$x", "\\emph{", "}}", "\\begin{document}", "a".repeat(20_000).as_str()] {
            let _ = doc(junk, 10.0, 400.0, &fixed);
        }
    }
}
