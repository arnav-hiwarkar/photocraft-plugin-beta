//! Unicode validation for history labels.
const FORMAT: &[(char, char)] = &[
    ('\u{AD}', '\u{AD}'),
    ('\u{600}', '\u{605}'),
    ('\u{61C}', '\u{61C}'),
    ('\u{6DD}', '\u{6DD}'),
    ('\u{70F}', '\u{70F}'),
    ('\u{890}', '\u{891}'),
    ('\u{8E2}', '\u{8E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Default-ignorable characters outside Cf (fillers, variation selectors, unassigned ignorables), and
/// the blank Braille pattern U+2800: allowed in a name, but they draw nothing on their own.
const BLANK: &[(char, char)] = &[
    ('\u{34F}', '\u{34F}'),
    ('\u{115F}', '\u{1160}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180D}'),
    ('\u{180F}', '\u{180F}'),
    ('\u{2065}', '\u{2065}'),
    ('\u{2800}', '\u{2800}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FFA0}', '\u{FFA0}'),
    ('\u{FFF0}', '\u{FFF8}'),
    ('\u{E0000}', '\u{E0000}'),
    ('\u{E0002}', '\u{E001F}'),
    ('\u{E0080}', '\u{E0FFF}'),
];

fn in_ranges(c: char, ranges: &[(char, char)]) -> bool {
    ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// A format character a menu label must not carry: it hides in the label or reorders it. The zero-width
/// non-joiner and joiner stay allowed, since Persian, Indic scripts and emoji sequences need them and they
/// cannot reorder text.
fn hidden_format(c: char) -> bool {
    !matches!(c, '\u{200C}' | '\u{200D}') && in_ranges(c, FORMAT)
}

/// Whether `name` (already trimmed) can be shown as a label: 1-64 characters, at least one of them
/// visible, and no control characters, line or paragraph separators, or hidden format characters.
pub fn printable_name(name: &str) -> bool {
    name.chars().nth(64).is_none()
        && name.chars().any(visible)
        && !name.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') || hidden_format(c))
}

/// `name` without what pads its ends invisibly: white space, the zero-width (non-)joiners and the
/// characters that draw nothing on their own (fillers, blank Braille, unassigned ignorables, stray
/// selectors), so a name that looks like "Tone" is "Tone". What belongs to a character the name
/// keeps stays: a variation selector after it (the emoji in "Love ❤️"), and a Hangul filler that
/// is part of a syllable block (U+1160 after a leading consonant, U+115F before a vowel).
pub fn trim_name(name: &str) -> &str {
    let pad = |c: char| c.is_whitespace() || matches!(c, '\u{200C}' | '\u{200D}') || in_ranges(c, BLANK);
    let mut start = name.len();
    let mut chars = name.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        // The choseong filler opens a syllable before a vowel, and before the jungseong filler U+1160
        // when a final consonant follows (U+115F U+1160 + a final consonant is how a lone final
        // consonant is written; U+115F U+1160 alone draws nothing).
        let opens_syllable = c == '\u{115F}'
            && chars.peek().is_some_and(|&(j, n)| {
                vowel_jamo(n) || (n == '\u{1160}' && name.get(j + n.len_utf8()..).and_then(|r| r.chars().next()).is_some_and(trailing_jamo))
            });
        if !pad(c) || opens_syllable {
            start = i;
            break;
        }
    }
    let s = name.get(start..).unwrap_or_default();
    let mut end = s.len();
    for (i, c) in s.char_indices().rev() {
        if !pad(c) {
            break;
        }
        end = i;
    }
    // Take back the mark that belongs to the last character kept: one per character, so a run of
    // selectors after it is still padding.
    let mut prev = s.get(..end).and_then(|h| h.chars().next_back());
    for c in s.get(end..).unwrap_or_default().chars() {
        let belongs = match prev {
            Some(p) => attaches(c) || (c == '\u{1160}' && leading_jamo(p)),
            None => false,
        };
        if !belongs {
            break;
        }
        end += c.len_utf8();
        prev = if attaches(c) { None } else { Some(c) };
    }
    s.get(..end).unwrap_or_default()
}

/// The marks among [`BLANK`] that modify the character before them: the combining grapheme joiner,
/// the Khmer inherent vowels, the Mongolian free variation selectors and the variation selectors.
fn attaches(c: char) -> bool {
    matches!(
        c,
        '\u{34F}' | '\u{17B4}' | '\u{17B5}' | '\u{180B}'..='\u{180D}' | '\u{180F}' | '\u{FE00}'..='\u{FE0F}' | '\u{E0100}'..='\u{E01EF}'
    )
}

/// A conjoining Hangul leading consonant (choseong), not the filler U+115F.
fn leading_jamo(c: char) -> bool {
    matches!(c, '\u{1100}'..='\u{115E}' | '\u{A960}'..='\u{A97C}')
}

/// A conjoining Hangul final consonant (jongseong).
fn trailing_jamo(c: char) -> bool {
    matches!(c, '\u{11A8}'..='\u{11FF}' | '\u{D7CB}'..='\u{D7FB}')
}

/// A conjoining Hangul vowel (jungseong) other than the vowel filler U+1160.
fn vowel_jamo(c: char) -> bool {
    matches!(c, '\u{1161}'..='\u{11A7}' | '\u{D7B0}'..='\u{D7C6}')
}

/// A character that draws something on its own.
fn visible(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !in_ranges(c, FORMAT) && !in_ranges(c, BLANK)
}
