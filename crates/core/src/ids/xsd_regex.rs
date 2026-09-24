//! XSD 1.0 regular expressions (XML Schema Part 2, Appendix F) →
//! Rust `regex` (design §2.6).
//!
//! The translator is a small recursive-descent parser over the XSD
//! grammar, so everything outside it is rejected instead of being passed
//! through to a richer dialect:
//!
//! | XSD behaviour | Translation |
//! |---|---|
//! | whole value must match | `^(?:…)$` (Rust `$` = end of text) |
//! | `^` / `$` are literal | escaped (`\^`, `\$`) |
//! | `.` excludes `\n`, `\r` | `[^\n\r]` |
//! | `\s` = `[#x20\t\n\r]` only | `[\x20\t\n\r]` (Rust `\s` is wider) |
//! | `\d`, `\w`, `\D`, `\W` | kept as Rust's Unicode classes (see note) |
//! | `\i \c \I \C` | XML 1.0 (5th ed.) NameStartChar / NameChar classes |
//! | `\p{Lu}` categories | kept (Rust supports general categories) |
//! | `\p{IsBasicLatin}` blocks | table below; other blocks → `Unsupported` |
//! | class subtraction `[a-z-[aeiou]]` | Rust class difference `[[a-z]--[aeiou]]` |
//! | `\/`, `\@` … (escaped ASCII punctuation outside the XSD list) | literal, as xmlschema does |
//! | lazy/possessive quantifiers, `(?…)`, backrefs, `\b` … | `InvalidIds` |
//!
//! Note on `\w`: strict XSD `\w` is `[#x0-#x10FFFF]-[\p{P}\p{Z}\p{C}]`
//! (so it excludes `_` and includes symbols like `+`). IfcTester passes
//! `\w` through `xmlschema.translate_pattern` unchanged to Python `re`,
//! whose `\w` ≈ Rust's (`_` included, `+` excluded). We follow IfcTester
//! here (ambiguity register candidate).

use regex::Regex;

use super::IdsError;

/// Compile an XSD pattern into an anchored Rust regex.
///
/// Errors: `InvalidIds` for syntax that is not XSD regex; `Unsupported`
/// for valid XSD constructs this engine does not translate (currently
/// only Unicode block escapes outside [`BLOCKS`]).
pub fn compile_xsd_pattern(p: &str) -> Result<Regex, IdsError> {
    let translated = translate_xsd_pattern(p)?;
    Regex::new(&translated).map_err(|e| {
        IdsError::invalid(format!(
            "xs:pattern '{p}' translated to '{translated}' but the regex engine rejected it: {e}"
        ))
    })
}

/// The translation step alone (exposed for tests and diagnostics).
pub fn translate_xsd_pattern(p: &str) -> Result<String, IdsError> {
    let chars: Vec<char> = p.chars().collect();
    let mut t = Translator { p, s: &chars, i: 0 };
    let body = t.reg_exp()?;
    if t.i != chars.len() {
        // Only an unmatched ')' can stop reg_exp early.
        return Err(t.err("unmatched ')'"));
    }
    Ok(format!("^(?:{body})$"))
}

/// Unicode blocks accepted in `\p{IsXxx}` (XSD 1.0 block names). Other
/// block names raise `Unsupported("xsd-regex:block:<name>")`.
pub const BLOCKS: &[(&str, char, char)] = &[
    ("BasicLatin", '\u{0000}', '\u{007F}'),
    ("Latin-1Supplement", '\u{0080}', '\u{00FF}'),
    ("LatinExtended-A", '\u{0100}', '\u{017F}'),
    ("LatinExtended-B", '\u{0180}', '\u{024F}'),
    ("IPAExtensions", '\u{0250}', '\u{02AF}'),
    ("SpacingModifierLetters", '\u{02B0}', '\u{02FF}'),
    ("CombiningDiacriticalMarks", '\u{0300}', '\u{036F}'),
    ("Greek", '\u{0370}', '\u{03FF}'),
    ("Cyrillic", '\u{0400}', '\u{04FF}'),
    ("LatinExtendedAdditional", '\u{1E00}', '\u{1EFF}'),
    ("GreekExtended", '\u{1F00}', '\u{1FFF}'),
    ("GeneralPunctuation", '\u{2000}', '\u{206F}'),
    ("SuperscriptsandSubscripts", '\u{2070}', '\u{209F}'),
    ("CurrencySymbols", '\u{20A0}', '\u{20CF}'),
    ("LetterlikeSymbols", '\u{2100}', '\u{214F}'),
    ("NumberForms", '\u{2150}', '\u{218F}'),
    ("Arrows", '\u{2190}', '\u{21FF}'),
    ("MathematicalOperators", '\u{2200}', '\u{22FF}'),
    ("BoxDrawing", '\u{2500}', '\u{257F}'),
    ("GeometricShapes", '\u{25A0}', '\u{25FF}'),
];

/// XSD general-category names allowed in `\p{..}` (XSD 1.0 F.1.1).
const CATEGORIES: &[&str] = &[
    "L", "Lu", "Ll", "Lt", "Lm", "Lo", "M", "Mn", "Mc", "Me", "N", "Nd", "Nl", "No", "P", "Pc",
    "Pd", "Ps", "Pe", "Pi", "Pf", "Po", "Z", "Zs", "Zl", "Zp", "S", "Sm", "Sc", "Sk", "So", "C",
    "Cc", "Cf", "Co", "Cn",
];

/// XML 1.0 5th ed. NameStartChar, as Rust class-body ranges.
const NAME_START: &str = r":A-Z_a-z\u{C0}-\u{D6}\u{D8}-\u{F6}\u{F8}-\u{2FF}\u{370}-\u{37D}\u{37F}-\u{1FFF}\u{200C}-\u{200D}\u{2070}-\u{218F}\u{2C00}-\u{2FEF}\u{3001}-\u{D7FF}\u{F900}-\u{FDCF}\u{FDF0}-\u{FFFD}\u{10000}-\u{EFFFF}";
/// NameChar additions on top of NameStartChar.
const NAME_EXTRA: &str = r"\-.0-9\u{B7}\u{300}-\u{36F}\u{203F}-\u{2040}";

struct Translator<'a> {
    p: &'a str,
    s: &'a [char],
    i: usize,
}

/// A translated character-class item: either a single char (usable as a
/// range endpoint) or a set fragment valid inside a Rust class.
enum ClassItem {
    Char(char),
    Set(String),
}

impl<'a> Translator<'a> {
    fn err(&self, msg: &str) -> IdsError {
        IdsError::invalid(format!(
            "xs:pattern '{}' is not a valid XSD regular expression: {msg} (at char {})",
            self.p, self.i
        ))
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn peek_at(&self, off: usize) -> Option<char> {
        self.s.get(self.i + off).copied()
    }

    // regExp ::= branch ( '|' branch )*
    fn reg_exp(&mut self) -> Result<String, IdsError> {
        let mut out = self.branch()?;
        while self.peek() == Some('|') {
            self.i += 1;
            out.push('|');
            out.push_str(&self.branch()?);
        }
        Ok(out)
    }

    // branch ::= piece*
    fn branch(&mut self) -> Result<String, IdsError> {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            out.push_str(&self.piece()?);
        }
        Ok(out)
    }

    // piece ::= atom quantifier?
    fn piece(&mut self) -> Result<String, IdsError> {
        let atom = self.atom()?;
        let mut out = atom;
        if let Some(q) = self.quantifier()? {
            out.push_str(&q);
            // XSD allows exactly one quantifier per atom: `a*?` (lazy),
            // `a*+` (possessive) and `a{2}{3}` are not XSD.
            if matches!(self.peek(), Some('?' | '*' | '+' | '{')) {
                return Err(self.err(
                    "a quantifier cannot follow a quantifier (lazy/possessive quantifiers are not XSD regex)",
                ));
            }
        }
        Ok(out)
    }

    fn quantifier(&mut self) -> Result<Option<String>, IdsError> {
        match self.peek() {
            Some(c @ ('?' | '*' | '+')) => {
                self.i += 1;
                Ok(Some(c.to_string()))
            }
            Some('{') => {
                self.i += 1;
                let min = self.digits().ok_or_else(|| self.err("'{' must start a {n}, {n,} or {n,m} quantifier"))?;
                let mut q = format!("{{{min}");
                if self.peek() == Some(',') {
                    self.i += 1;
                    q.push(',');
                    if let Some(max) = self.digits() {
                        let (lo, hi) = (min.parse::<u64>(), max.parse::<u64>());
                        match (lo, hi) {
                            (Ok(lo), Ok(hi)) if lo <= hi => {}
                            (Ok(_), Ok(_)) => return Err(self.err("quantifier {n,m} requires n <= m")),
                            _ => return Err(self.err("quantifier bound too large")),
                        }
                        q.push_str(&max);
                    }
                }
                if self.peek() != Some('}') {
                    return Err(self.err("unterminated quantifier"));
                }
                self.i += 1;
                q.push('}');
                Ok(Some(q))
            }
            _ => Ok(None),
        }
    }

    fn digits(&mut self) -> Option<String> {
        let start = self.i;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.i == start {
            None
        } else {
            Some(self.s[start..self.i].iter().collect())
        }
    }

    // atom ::= Char | charClass | '(' regExp ')'
    fn atom(&mut self) -> Result<String, IdsError> {
        let Some(c) = self.peek() else {
            return Err(self.err("unexpected end of pattern"));
        };
        match c {
            '(' => {
                self.i += 1;
                if self.peek() == Some('?') {
                    return Err(self.err(
                        "'(?' groups (non-capturing, lookaround, flags) are not XSD regex",
                    ));
                }
                let inner = self.reg_exp()?;
                if self.peek() != Some(')') {
                    return Err(self.err("unclosed '('"));
                }
                self.i += 1;
                Ok(format!("(?:{inner})"))
            }
            '[' => {
                self.i += 1;
                self.char_class_expr_body()
            }
            '.' => {
                self.i += 1;
                Ok(r"[^\n\r]".to_string())
            }
            '\\' => {
                self.i += 1;
                match self.escape()? {
                    ClassItem::Char(ch) => Ok(regex::escape(&ch.to_string())),
                    ClassItem::Set(frag) => Ok(format!("[{frag}]")),
                }
            }
            '?' | '*' | '+' | '{' => Err(self.err("quantifier without a preceding atom")),
            ']' | '}' => Err(self.err(&format!("unescaped '{c}'"))),
            // '^' and '$' are ordinary characters in XSD; regex::escape
            // makes them literal.
            other => {
                self.i += 1;
                Ok(regex::escape(&other.to_string()))
            }
        }
    }

    /// Called after a backslash. Returns a single char (SingleCharEsc)
    /// or a class fragment (multi-char / category escapes).
    fn escape(&mut self) -> Result<ClassItem, IdsError> {
        let Some(c) = self.peek() else {
            return Err(self.err("pattern ends with a lone backslash"));
        };
        self.i += 1;
        Ok(match c {
            'n' => ClassItem::Char('\n'),
            'r' => ClassItem::Char('\r'),
            't' => ClassItem::Char('\t'),
            '\\' | '|' | '.' | '?' | '*' | '+' | '(' | ')' | '{' | '}' | '-' | '[' | ']' | '^' => {
                ClassItem::Char(c)
            }
            's' => ClassItem::Set(r"\x20\t\n\r".to_string()),
            'S' => ClassItem::Set(r"[^\x20\t\n\r]".to_string()),
            'd' => ClassItem::Set(r"\d".to_string()),
            'D' => ClassItem::Set(r"\D".to_string()),
            'w' => ClassItem::Set(r"\w".to_string()),
            'W' => ClassItem::Set(r"\W".to_string()),
            'i' => ClassItem::Set(NAME_START.to_string()),
            'I' => ClassItem::Set(format!("[^{NAME_START}]")),
            'c' => ClassItem::Set(format!("{NAME_START}{NAME_EXTRA}")),
            'C' => ClassItem::Set(format!("[^{NAME_START}{NAME_EXTRA}]")),
            'p' | 'P' => {
                let set = self.property()?;
                if c == 'p' {
                    ClassItem::Set(set)
                } else {
                    ClassItem::Set(format!("[^{set}]"))
                }
            }
            // Not in the XSD SingleCharEsc list, but xmlschema (IfcTester's
            // translator) accepts `\` + any ASCII punctuation or space as the
            // literal char, and the suite relies on it:
            // property/fail-properties_can_be_associated_to_relevant_object_types.ids
            // uses `\/`. Letters and digits stay errors (`\b`, `\1`, …).
            other if other.is_ascii_punctuation() || other == ' ' => ClassItem::Char(other),
            other => {
                return Err(self.err(&format!(
                    "'\\{other}' is not an XSD escape (backreferences, \\b, \\A, \\x, \\u … are not XSD regex)"
                )))
            }
        })
    }

    /// `\p{...}` body (after `p`/`P`). Returns a class fragment.
    fn property(&mut self) -> Result<String, IdsError> {
        if self.peek() != Some('{') {
            return Err(self.err("\\p / \\P must be followed by {name}"));
        }
        self.i += 1;
        let start = self.i;
        while matches!(self.peek(), Some(c) if c != '}') {
            self.i += 1;
        }
        if self.peek() != Some('}') {
            return Err(self.err("unterminated \\p{...}"));
        }
        let name: String = self.s[start..self.i].iter().collect();
        self.i += 1;
        if let Some(block) = name.strip_prefix("Is") {
            return match BLOCKS.iter().find(|(n, _, _)| *n == block) {
                Some((_, lo, hi)) => Ok(format!(
                    "\\u{{{:X}}}-\\u{{{:X}}}",
                    *lo as u32, *hi as u32
                )),
                None => Err(IdsError::unsupported(format!("xsd-regex:block:{name}"))),
            };
        }
        if CATEGORIES.contains(&name.as_str()) {
            Ok(format!("\\p{{{name}}}"))
        } else {
            Err(self.err(&format!("'{name}' is not an XSD character category or Is-block")))
        }
    }

    /// Parse `charGroup ']'` after the opening '['. Returns a complete
    /// Rust class (`[...]`).
    fn char_class_expr_body(&mut self) -> Result<String, IdsError> {
        let negated = if self.peek() == Some('^') {
            self.i += 1;
            true
        } else {
            false
        };
        let mut body = String::new();
        let mut n_items = 0usize;
        let mut subtraction: Option<String> = None;
        loop {
            let Some(c) = self.peek() else {
                return Err(self.err("unterminated character class"));
            };
            if c == ']' {
                break;
            }
            if c == '-' {
                if self.peek_at(1) == Some('[') {
                    if n_items == 0 {
                        return Err(self.err("class subtraction needs a non-empty base group"));
                    }
                    self.i += 2;
                    subtraction = Some(self.char_class_expr_body()?);
                    if self.peek() != Some(']') {
                        return Err(self.err("class subtraction must be the last part of a character class"));
                    }
                    break;
                }
                // '-' is literal only at the start or end of a group.
                if n_items == 0 || self.peek_at(1) == Some(']') {
                    self.i += 1;
                    body.push_str(r"\-");
                    n_items += 1;
                    continue;
                }
                return Err(self.err("'-' must be escaped unless it is first or last in a character class"));
            }
            if c == '[' {
                return Err(self.err("unescaped '[' inside a character class"));
            }
            let first = self.class_char_or_esc()?;
            match first {
                ClassItem::Set(frag) => body.push_str(&frag),
                ClassItem::Char(lo) => {
                    // Range? '-' followed by something other than ']' or '['.
                    if self.peek() == Some('-')
                        && !matches!(self.peek_at(1), Some(']') | Some('[') | None)
                    {
                        self.i += 1;
                        let hi = match self.class_char_or_esc()? {
                            ClassItem::Char(h) => h,
                            ClassItem::Set(_) => {
                                return Err(self.err("a character range cannot end in a multi-character escape"))
                            }
                        };
                        if hi < lo {
                            return Err(self.err(&format!("character range '{lo}-{hi}' is reversed")));
                        }
                        body.push_str(&class_lit(lo));
                        body.push('-');
                        body.push_str(&class_lit(hi));
                    } else {
                        body.push_str(&class_lit(lo));
                    }
                }
            }
            n_items += 1;
        }
        if n_items == 0 {
            return Err(self.err("empty character class"));
        }
        // consume the closing ']'
        self.i += 1;
        let base = if negated {
            format!("[^{body}]")
        } else {
            format!("[{body}]")
        };
        Ok(match subtraction {
            Some(sub) => format!("[{base}--{sub}]"),
            None => base,
        })
    }

    /// One class element: an escape or a literal char (not '[', ']', '-'
    /// handled by the caller).
    fn class_char_or_esc(&mut self) -> Result<ClassItem, IdsError> {
        let Some(c) = self.peek() else {
            return Err(self.err("unterminated character class"));
        };
        if c == '\\' {
            self.i += 1;
            return self.escape();
        }
        if c == '[' || c == ']' {
            return Err(self.err(&format!("unescaped '{c}' inside a character class")));
        }
        self.i += 1;
        Ok(ClassItem::Char(c))
    }
}

/// A single char as a Rust class literal. `regex::escape` covers every
/// class/set-operation metachar (`[ ] \ ^ - & ~`).
fn class_lit(c: char) -> String {
    regex::escape(&c.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(p: &str) -> Regex {
        compile_xsd_pattern(p).unwrap_or_else(|e| panic!("{p}: {e}"))
    }

    fn is_invalid(p: &str) -> bool {
        matches!(compile_xsd_pattern(p), Err(IdsError::InvalidIds { .. }))
    }

    #[test]
    fn ids_regex_implicit_anchoring() {
        let r = ok("DT[0-9]{2}");
        assert!(r.is_match("DT01"));
        assert!(!r.is_match("xDT01"));
        assert!(!r.is_match("DT012"));
        assert!(!r.is_match("DT01\n"));
        let alt = ok("a|b");
        assert!(alt.is_match("a") && alt.is_match("b") && !alt.is_match("ab"));
    }

    #[test]
    fn ids_regex_caret_dollar_literal() {
        let r = ok("a^b$");
        assert!(r.is_match("a^b$"));
        assert!(!r.is_match("ab"));
        assert!(ok("^").is_match("^"));
        assert!(ok("[$^]+").is_match("$^$"));
        assert!(ok("[a^]").is_match("^"));
    }

    #[test]
    fn ids_regex_dot_excludes_newlines() {
        let r = ok("a.c");
        assert!(r.is_match("abc") && r.is_match("aøc"));
        assert!(!r.is_match("a\nc") && !r.is_match("a\rc"));
        assert!(ok(".*").is_match(""));
    }

    #[test]
    fn ids_regex_class_subtraction() {
        let r = ok("[a-z-[aeiou]]+");
        assert!(r.is_match("bcd"));
        assert!(!r.is_match("bad"));
        // negation binds to the base group: (not a-z) minus 'A'
        let n = ok("[^a-z-[A]]");
        assert!(n.is_match("B") && n.is_match("1"));
        assert!(!n.is_match("A") && !n.is_match("b"));
        let nested = ok("[a-z-[b-y-[c]]]");
        assert!(nested.is_match("a") && nested.is_match("c") && nested.is_match("z"));
        assert!(!nested.is_match("b"));
        let w = ok(r"[\w-[_]]");
        assert!(w.is_match("a") && !w.is_match("_"));
    }

    #[test]
    fn ids_regex_name_char_escapes() {
        let i = ok(r"\i\c*");
        assert!(i.is_match("_foo-bar.1") && i.is_match(":x") && i.is_match("Ærø"));
        assert!(!i.is_match("1abc") && !i.is_match("-a"));
        assert!(ok(r"\I").is_match("1") && !ok(r"\I").is_match("a"));
        assert!(ok(r"\C").is_match(" ") && !ok(r"\C").is_match("-"));
    }

    #[test]
    fn ids_regex_block_escapes() {
        assert!(ok(r"\p{IsBasicLatin}+").is_match("abc~"));
        assert!(!ok(r"\p{IsBasicLatin}").is_match("æ"));
        assert!(ok(r"\p{IsLatin-1Supplement}").is_match("æ"));
        assert!(ok(r"\P{IsBasicLatin}").is_match("ø"));
        assert!(ok(r"[\p{IsBasicLatin}\p{IsLatin-1Supplement}]+").is_match("Blåbær"));
        match compile_xsd_pattern(r"\p{IsThai}") {
            Err(IdsError::Unsupported { feature, .. }) => assert_eq!(feature, "xsd-regex:block:IsThai"),
            other => panic!("{other:?}"),
        }
        assert!(is_invalid(r"\p{Xx}"));
    }

    #[test]
    fn ids_regex_categories_and_unicode_classes() {
        assert!(ok(r"\p{Lu}\p{Ll}+").is_match("Øst"));
        assert!(ok(r"\d+").is_match("١٢٣")); // Arabic-Indic digits are Nd
        assert!(ok(r"\w+").is_match("Uniclass2015"));
        assert!(ok(r"\s").is_match(" ") && !ok(r"\s").is_match("\u{A0}"));
        assert!(ok(r"\S").is_match("\u{A0}"));
    }

    #[test]
    fn ids_regex_rejects_non_xsd() {
        for p in [
            "a*?", "a+?", "a??", "a{2}?", "a*+", "a{2}{3}", "(?:a)", "(?=a)", "(?!a)", "(?i)a",
            r"(a)\1", r"\b", r"\A", r"\x41", "a{,3}", "a{3,2}", "*a", "a|*", "(a",
            "a)", "[a", "[]", "[a-]b-]", "[z-a]", "a]", "a}", r"a\", "[a[b]]", r"[\d-z]",
        ] {
            assert!(is_invalid(p), "should reject {p:?}: {:?}", translate_xsd_pattern(p));
        }
        // `\u0041` (built at runtime so no layer unescapes it).
        let u_esc = format!("{}u0041", '\\');
        assert!(is_invalid(&u_esc), "{u_esc}");
    }

    #[test]
    fn ids_regex_literal_dash_and_escapes() {
        assert!(ok("[-a]").is_match("-"));
        assert!(ok("[a-]").is_match("-"));
        assert!(ok(r"[a\-z]").is_match("-") && !ok(r"[a\-z]").is_match("b"));
        assert!(ok(r"\.\*\?\+\(\)\{\}\|\[\]\^\\").is_match(r".*?+(){}|[]^\"));
        assert!(ok(r"a\nb").is_match("a\nb"));
        assert!(ok("[&~]+").is_match("&&~~"));
        assert!(ok("a{2,}").is_match("aaa") && !ok("a{2,}").is_match("a"));
        assert!(ok("a{0}b").is_match("b"));
        assert!(ok(r"[0-9]{2}\/[0-9]{2}").is_match("12/34"));
        assert!(ok(r"\@\#\ ").is_match("@# "));
        assert!(ok("").is_match("") && !ok("").is_match("x"));
    }

    #[test]
    fn ids_regex_suite_patterns() {
        // Patterns used by the buildingSMART IDS conformance cases.
        assert!(ok("[A-Z]{2}[0-9]{2}").is_match("AB12"));
        assert!(ok("1.*").is_match("11"));
        assert!(ok("IFC.*TYPE").is_match("IFCWALLTYPE"));
        assert!(ok("NumberOfRiser(s)?").is_match("NumberOfRisers"));
        assert!(ok(r"[^@]+@[^\.]+\..+").is_match("a@b.c"));
    }
}
