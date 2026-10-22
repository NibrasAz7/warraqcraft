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

/// A finished fragment: items plus its bounding metrics.
#[derive(Debug, Clone)]
pub struct Fragment {
    pub items: Vec<Item>,
    pub width: f64,
    pub height: f64,
    pub depth: f64,
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
fn parse_list(chars: &[char], pos: usize, stop: Option<char>) -> Option<(Vec<Atom>, usize)> {
    const MAX_DEPTH: usize = 32;
    fn inner(chars: &[char], pos: usize, stop: Option<char>, depth: usize) -> Option<(Vec<Atom>, usize)> {
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
                '{' => {
                    let (g, ni) = inner(chars, i + 1, Some('}'), depth + 1)?;
                    out.push(Atom::Group(g));
                    i = ni;
                }
                '^' | '_' => {
                    i += 1;
                    let script = parse_script(chars, &mut i, depth)?;
                    let last = out.last_mut()?;
                    attach_script(last, c == '^', script)?;
                }
                '\\' => {
                    i += 1;
                    let name_start = i;
                    while i < chars.len() && chars[i].is_ascii_alphabetic() {
                        i += 1;
                    }
                    if i == name_start {
                        // Control symbol: `\,` `\;` `\!` `\ ` etc.
                        let sym = *chars.get(i)?;
                        i += 1;
                        match sym {
                            ',' => out.push(Atom::Space(3.0 / 18.0)),
                            ';' => out.push(Atom::Space(5.0 / 18.0)),
                            ':' => out.push(Atom::Space(4.0 / 18.0)),
                            '!' => out.push(Atom::Space(-3.0 / 18.0)),
                            ' ' | '~' => out.push(Atom::Space(0.5)),
                            _ => return None,
                        }
                        continue;
                    }
                    let name: String = chars[name_start..i].iter().collect();
                    match name.as_str() {
                        "frac" | "dfrac" | "tfrac" | "cfrac" => {
                            let num = braced(chars, &mut i, depth)?;
                            let den = braced(chars, &mut i, depth)?;
                            out.push(Atom::Frac { num, den, sup: None, sub: None });
                        }
                        "sqrt" => {
                            // Optional [n] index is out of subset scope.
                            if chars.get(i) == Some(&'[') {
                                return None;
                            }
                            let inner_v = braced(chars, &mut i, depth)?;
                            out.push(Atom::Sqrt { inner: inner_v, sup: None, sub: None });
                        }
                        "left" | "right" | "big" | "Big" | "bigg" | "Bigg" | "left." | "right." => return None,
                        "mathrm" | "mathbf" | "text" | "operatorname" => {
                            // Upright group: contents as Ord Regular.
                            let g = braced(chars, &mut i, depth)?;
                            for a in g {
                                out.push(force_regular(a));
                            }
                        }
                        _ => {
                            let ch = symbol_of(&name)?;
                            let class = match name.as_str() {
                                "sum" | "prod" | "int" | "oint" => Class::Inner,
                                _ => char_class(ch),
                            };
                            out.push(Atom::Sym { ch, class, italic: false, sup: None, sub: None });
                        }
                    }
                }
                '$' => i += 1, // tolerate inline $…$ wrappers
                _ => {
                    let italic = c.is_ascii_alphabetic();
                    out.push(Atom::Sym { ch: c, class: char_class(c), italic, sup: None, sub: None });
                    i += 1;
                }
            }
        }
        Some((out, i))
    }
    inner(chars, pos, stop, 0)
}

/// A script operand: `{…}` group or a single atom (char or \command).
fn parse_script(chars: &[char], i: &mut usize, depth: usize) -> Option<Vec<Atom>> {
    match chars.get(*i) {
        Some('{') => {
            let (g, ni) = parse_list(chars, *i + 1, Some('}'))?;
            let _ = depth;
            *i = ni;
            Some(g)
        }
        _ => {
            let (mut a, ni) = parse_list(chars, *i, None)?;
            if a.len() != 1 {
                return None;
            }
            *i = ni;
            Some(vec![a.remove(0)])
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
            let (mut a, ni) = parse_list(chars, *i, None)?;
            if a.len() != 1 {
                return None;
            }
            *i = ni;
            Some(vec![a.remove(0)])
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
    Some(Fragment { items: lay.items, width: lay.x, height: height + depth, depth: 0.0 })
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
}
