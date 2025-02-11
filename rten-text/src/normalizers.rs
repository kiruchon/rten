//! Tools for performing string normalization prior to tokenization.

use std::error::Error;
use std::fmt;

use fancy_regex::Regex;
use unicode_categories::UnicodeCategories;
use unicode_normalization::char::{compose, decompose_canonical, decompose_compatible};

struct CharNormalizer {
    normalized: Vec<char>,

    /// Temporary buffer that holds the output of a normalization step until
    /// it is copied back to `normalized`.
    tmp: Vec<char>,
}

impl CharNormalizer {
    fn new() -> CharNormalizer {
        CharNormalizer {
            normalized: Vec::new(),
            tmp: Vec::new(),
        }
    }

    /// Set the input character to normalize.
    fn set_char(&mut self, ch: char) {
        self.tmp.push(ch);
        self.update_normalized_from_tmp();
    }

    /// Lowercase the normalized characters.
    fn lower_case(&mut self) {
        for ch in &self.normalized {
            for lower_ch in ch.to_lowercase() {
                self.tmp.push(lower_ch);
            }
        }
        self.update_normalized_from_tmp();
    }

    /// Decompose the input into NFD form and then remove any characters in
    /// the Unicode non-spacing mark ("Mn") category.
    fn strip_accents(&mut self) {
        for ch in &self.normalized {
            decompose_canonical(*ch, |decomposed| {
                if !decomposed.is_mark_nonspacing() {
                    self.tmp.push(decomposed);
                }
            });
        }
        self.update_normalized_from_tmp();
    }

    /// Return the normalized characters.
    fn normalized(&self) -> &[char] {
        &self.normalized
    }

    fn update_normalized_from_tmp(&mut self) {
        self.normalized.clear();
        self.normalized.extend(self.tmp.iter());
        self.tmp.clear();
    }
}

/// Errors occuring while normalizing text during the first phase of
/// tokenization.
#[derive(Clone, Debug)]
pub enum NormalizeError {
    RegexError(Box<fancy_regex::Error>),
}

impl fmt::Display for NormalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RegexError(err) => write!(f, "regex failed {}", err),
        }
    }
}

impl Error for NormalizeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RegexError(err) => Some(err),
        }
    }
}

impl From<fancy_regex::Error> for NormalizeError {
    fn from(val: fancy_regex::Error) -> Self {
        Self::RegexError(Box::new(val))
    }
}

/// A normalizer applies normalization such as Unicode normalization and
/// lower-casing to strings.
///
/// In addition to the normalized text, Normalizer methods also return mappings
/// from positions in the normalized string back to the original string. This
/// is useful for post-processing in NLP tasks to map machine learning model
/// outputs back to the location in the original text.
pub trait Normalizer: std::fmt::Debug {
    /// Apply normalization to a string.
    ///
    /// Returns a tuple of `(normalized_string, offset_map)` where `offset_map`
    /// is a mapping from byte offsets in the normalized string to corresponding
    /// offsets in the original string.
    fn normalize(&self, text: &str) -> Result<(String, Vec<usize>), NormalizeError>;
}

/// A [`Normalizer`] that implements normalization used by BERT and BERT-derived
/// models.
#[derive(Clone, Debug)]
pub struct Bert {
    lowercase: bool,
    strip_accents: bool,
}

/// Configuration for a [`Bert`] normalizer.
#[derive(Clone, Debug, Default)]
pub struct BertOptions {
    /// If true, convert all text to lowercase using [`char::to_lowercase`].
    pub lowercase: bool,

    /// Whether to strip accents when tokenizing. An "accent" is defined as
    /// any unicode character in the Nonspacing Mark ("Mn") category.
    pub strip_accents: bool,
}

impl Bert {
    pub fn new(opts: BertOptions) -> Bert {
        Bert {
            lowercase: opts.lowercase,
            strip_accents: opts.strip_accents,
        }
    }

    /// Return true if this normalizer doesn't alter its input.
    fn is_noop(&self) -> bool {
        !self.lowercase && !self.strip_accents
    }
}

impl Normalizer for Bert {
    fn normalize(&self, text: &str) -> Result<(String, Vec<usize>), NormalizeError> {
        if self.is_noop() {
            let offsets = (0..text.len()).collect();
            return Ok((text.to_string(), offsets));
        }

        let mut normalized = String::with_capacity(text.len());
        let mut offsets = Vec::with_capacity(text.len());
        let mut char_normalizer = CharNormalizer::new();

        for (offset, ch) in text.char_indices() {
            char_normalizer.set_char(ch);

            if self.strip_accents {
                char_normalizer.strip_accents();
            }

            if self.lowercase {
                char_normalizer.lower_case();
            }

            for ch in char_normalizer.normalized() {
                normalized.push(*ch);
                for _ in 0..ch.len_utf8() {
                    offsets.push(offset);
                }
            }
        }

        Ok((normalized, offsets))
    }
}

/// Replaces occurrences of a pattern with a given string.
#[derive(Clone, Debug)]
pub struct Replace {
    regex: Regex,
    content: String,
}

impl Replace {
    /// Replaces occurrences of `pattern` with `content`.
    ///
    /// `pattern` is a regex pattern. See the
    /// [fancy-regex](https://docs.rs/fancy-regex/) docs for supported syntax.
    pub fn new(pattern: &str, content: String) -> Result<Replace, NormalizeError> {
        Ok(Replace {
            regex: Regex::new(pattern)?,
            content,
        })
    }
}

impl Normalizer for Replace {
    fn normalize(&self, text: &str) -> Result<(String, Vec<usize>), NormalizeError> {
        let mut normalized = String::with_capacity(text.len());
        let mut offsets = Vec::with_capacity(text.len());

        let mut last_match_end = 0;
        for match_ in self.regex.find_iter(text) {
            let match_ = match_?;

            let before_match = &text[last_match_end..match_.range().start];
            normalized.push_str(before_match);
            offsets.extend(last_match_end..match_.range().start);

            normalized.push_str(&self.content);
            offsets.extend(std::iter::repeat(match_.range().start).take(self.content.len()));

            last_match_end = match_.range().end;
        }

        normalized.push_str(&text[last_match_end..]);
        offsets.extend(last_match_end..text.len());

        Ok((normalized, offsets))
    }
}

/// Run a series of normalizers in sequence.
#[derive(Debug)]
pub struct Sequence {
    normalizers: Vec<Box<dyn Normalizer>>,
}

impl Sequence {
    pub fn from_vec(normalizers: Vec<Box<dyn Normalizer>>) -> Self {
        Sequence { normalizers }
    }
}

impl Normalizer for Sequence {
    fn normalize(&self, text: &str) -> Result<(String, Vec<usize>), NormalizeError> {
        let mut normalized = text.to_string();
        let mut offsets: Vec<usize> = (0..text.len()).collect();

        for normalizer in &self.normalizers {
            let (next_normalized, mut next_offsets) = normalizer.normalize(&normalized)?;
            for offset in next_offsets.iter_mut() {
                *offset = offsets[*offset];
            }
            normalized = next_normalized;
            offsets = next_offsets;
        }

        Ok((normalized, offsets))
    }
}

/// Temporary buffer used while normalizing text.
struct UnicodeBuf {
    // Work-in-progress normalized text.
    normalized: String,

    // Offset from char position in `normalized` to byte position in
    // original text.
    char_offsets: Vec<usize>,
}

impl UnicodeBuf {
    fn with_capacity(len: usize) -> Self {
        UnicodeBuf {
            normalized: String::with_capacity(len),
            char_offsets: Vec::with_capacity(len),
        }
    }

    /// Add a character and its associated byte offset in the original text to
    /// the work-in-progress buffer.
    fn push(&mut self, ch: char, offset: usize) {
        self.normalized.push(ch);
        self.char_offsets.push(offset);
    }

    /// Compose `ch` with the last char in the buffer if possible, otherwise
    /// add it the same as `push`.
    fn push_compose(&mut self, ch: char, offset: usize) {
        if let (Some(prev_ch), Some(prev_offset)) = (self.normalized.pop(), self.char_offsets.pop())
        {
            if let Some(composed_ch) = compose(prev_ch, ch) {
                self.push(composed_ch, prev_offset);
            } else {
                self.push(prev_ch, prev_offset);
                self.push(ch, offset);
            }
        } else {
            self.push(ch, offset);
        }
    }

    fn into_string_with_byte_offsets(self) -> (String, Vec<usize>) {
        // Convert offsets from char positions in normalized text to byte
        // positions in normalized text.
        let UnicodeBuf {
            normalized,
            char_offsets,
        } = self;
        let mut byte_offsets = Vec::with_capacity(char_offsets.len());
        for (ch, offset) in normalized.chars().zip(char_offsets) {
            for _ in 0..ch.len_utf8() {
                byte_offsets.push(offset);
            }
        }
        (normalized, byte_offsets)
    }
}

/// Normalize text into one of the standard Unicode normalization forms.
#[derive(Clone, Debug)]
pub enum Unicode {
    /// Canonical composition
    Nfc,
    /// Canonical decomposition
    Nfd,
    /// Compatibility decomposition, followed by canonical composition
    Nfkc,
    /// Compatibility decomposition
    Nfkd,
}

impl Normalizer for Unicode {
    fn normalize(&self, text: &str) -> Result<(String, Vec<usize>), NormalizeError> {
        let mut tmp = UnicodeBuf::with_capacity(text.len());

        for (offset, ch) in text.char_indices() {
            match self {
                Self::Nfc => {
                    tmp.push_compose(ch, offset);
                }
                Self::Nfd => {
                    decompose_canonical(ch, |decomposed| {
                        tmp.push(decomposed, offset);
                    });
                }
                Self::Nfkc => {
                    decompose_compatible(ch, |ch| {
                        tmp.push_compose(ch, offset);
                    });
                }
                Self::Nfkd => {
                    decompose_compatible(ch, |decomposed| {
                        tmp.push(decomposed, offset);
                    });
                }
            }
        }

        Ok(tmp.into_string_with_byte_offsets())
    }
}

#[cfg(test)]
mod tests {
    use rten_testing::TestCases;

    use super::{Bert, BertOptions, Normalizer, Replace, Sequence, Unicode};

    #[test]
    fn test_bert_noop() {
        let normalizer = Bert::new(BertOptions::default());
        let inputs = [
            "Hello world!", // Mixed case
            "Motörhead",    // Accented
            "lowercase",
        ];
        for input in inputs {
            let (normalized, offsets) = normalizer.normalize(input).unwrap();
            assert_eq!(normalized, input);
            assert_eq!(offsets, (0..input.len()).collect::<Vec<_>>());
        }
    }

    #[test]
    fn test_bert_lowercase() {
        let normalizer = Bert::new(BertOptions {
            lowercase: true,
            ..Default::default()
        });

        #[derive(Debug)]
        struct Case<'a> {
            input: &'a str,
            expected: &'a str,
            expected_offsets: Vec<usize>,
        }

        let cases = [
            // Simple text where chars map 1:1 to lower-case version
            Case {
                input: "Hello World!",
                expected: "hello world!",
                expected_offsets: (0.."hello world!".len()).collect(),
            },
            // Text with chars which expand when lower-cased
            Case {
                input: "İİAB",
                expected: "i\u{307}i\u{307}ab",

                // The "İ" char requires two bytes in the input and expands into
                // two characters which require one and three bytes
                // respectively. Hence the offsets contain two groups of three
                // equal offsets, with values separated by two.
                expected_offsets: vec![0, 0, 0, 2, 2, 2, 4, 5],
            },
        ];

        cases.test_each(|case| {
            let Case {
                input,
                expected,
                expected_offsets,
            } = case;

            let (normalized, offsets) = normalizer.normalize(input).unwrap();
            assert_eq!(normalized, *expected);
            assert_eq!(offsets, *expected_offsets);
        })
    }

    #[test]
    fn test_bert_strip_accepts() {
        #[derive(Debug)]
        struct Case<'a> {
            input: &'a str,
            lowercase: bool,
            expected: &'a str,
            expected_offsets: Vec<usize>,
        }

        let cases = [
            // Strip accents only
            Case {
                input: "Motörhead",
                lowercase: false,
                expected: "Motorhead",
                // Note jump in offset where the two UTF-8 char "ö" is replaced
                // with "o".
                expected_offsets: vec![0, 1, 2, 3, 5, 6, 7, 8, 9],
            },
            // Combined lowercase + strip accents
            Case {
                input: "Motörhead",
                lowercase: true,
                expected: "motorhead",
                // Note jump in offset where the two UTF-8 char "ö" is replaced
                // with "o".
                expected_offsets: vec![0, 1, 2, 3, 5, 6, 7, 8, 9],
            },
        ];

        cases.test_each(|case| {
            let Case {
                input,
                lowercase,
                expected,
                expected_offsets,
            } = case;

            let normalizer = Bert::new(BertOptions {
                lowercase: *lowercase,
                strip_accents: true,
                ..Default::default()
            });

            let (normalized, offsets) = normalizer.normalize(input).unwrap();
            assert_eq!(normalized, *expected);
            assert_eq!(offsets, *expected_offsets);
        })
    }

    #[test]
    fn test_replace() {
        #[derive(Debug)]
        struct Case<'a> {
            input: &'a str,
            pattern: &'a str,
            content: &'a str,
            expected: &'a str,
            expected_offsets: Vec<usize>,
        }

        let cases = [
            // No-op replacement
            Case {
                input: "nothing to do here",
                pattern: "does-not-match",
                content: "replacement",
                expected: "nothing to do here",
                expected_offsets: (0.."nothing to do here".len()).collect(),
            },
            // Whitespace simplification
            Case {
                input: "foo  bar  baz",
                pattern: r"\s+",
                content: " ",
                expected: "foo bar baz",
                expected_offsets: [0, 1, 2, 3, 5, 6, 7, 8, 10, 11, 12].into(),
            },
            // Pattern with overlapping matches
            Case {
                input: "foo   bar   baz",
                pattern: r"  ",
                content: " ",
                expected: "foo  bar  baz",
                expected_offsets: [0, 1, 2, 3, 5, 6, 7, 8, 9, 11, 12, 13, 14].into(),
            },
        ];

        cases.test_each(|case| {
            let Case {
                input,
                pattern,
                content,
                expected,
                expected_offsets,
            } = case;

            let normalizer = Replace::new(pattern, content.to_string()).unwrap();
            let (normalized, offsets) = normalizer.normalize(input).unwrap();
            assert_eq!(offsets.len(), normalized.len());
            assert_eq!(normalized, *expected);
            assert_eq!(offsets, *expected_offsets);
        })
    }

    fn lowercase_normalizer() -> Box<dyn Normalizer> {
        Box::new(Bert::new(BertOptions {
            lowercase: true,
            strip_accents: false,
        }))
    }

    fn nfc_normalizer() -> Box<dyn Normalizer> {
        Box::new(Unicode::Nfc)
    }

    fn replace_normalizer(pattern: &str, content: &str) -> Box<dyn Normalizer> {
        Box::new(Replace::new(pattern, content.to_string()).unwrap())
    }

    #[test]
    fn test_sequence() {
        use std::panic::AssertUnwindSafe;

        #[derive(Debug)]
        struct Case<'a> {
            input: &'a str,
            normalizers: AssertUnwindSafe<Vec<Box<dyn Normalizer>>>,
            expected: &'a str,
            expected_offsets: Vec<usize>,
        }

        let cases = [
            // NFC + Lowercase + whitespace simplification.
            //
            // This is the sequence used by CLIP.
            Case {
                input: "FOO  BAR  BAZ",
                normalizers: AssertUnwindSafe(
                    [
                        nfc_normalizer(),
                        lowercase_normalizer(),
                        replace_normalizer(r"\s+", " "),
                    ]
                    .into(),
                ),
                expected: "foo bar baz",
                expected_offsets: [0, 1, 2, 3, 5, 6, 7, 8, 10, 11, 12].into(),
            },
            // Multiple normalizers that modify offsets.
            Case {
                input: "FOO BAR BAZ",
                normalizers: AssertUnwindSafe(
                    [
                        replace_normalizer(" ", "--"),
                        replace_normalizer("--", "_"),
                        lowercase_normalizer(),
                    ]
                    .into(),
                ),
                expected: "foo_bar_baz",
                expected_offsets: (0.."foo bar baz".len()).collect(),
            },
            // Empty sequence
            Case {
                input: "foo bar baz",
                normalizers: AssertUnwindSafe(Vec::new()),
                expected: "foo bar baz",
                expected_offsets: (0.."foo bar baz".len()).collect(),
            },
        ];

        cases.test_each_value(|case| {
            let Case {
                input,
                normalizers,
                expected,
                expected_offsets,
            } = case;

            let seq = Sequence::from_vec(normalizers.0);
            let (normalized, offsets) = seq.normalize(input).unwrap();
            assert_eq!(normalized, expected);
            assert_eq!(offsets, expected_offsets);
        })
    }

    #[test]
    fn test_unicode() {
        #[derive(Debug)]
        struct Case<'a> {
            input: &'a str,
            normalizer: Unicode,
            expected: &'a str,
            expected_offsets: Vec<usize>,
        }

        let noop_case = |normalizer| Case {
            input: "abc",
            normalizer,
            expected: "abc",
            expected_offsets: [0, 1, 2].into(),
        };

        let cases = [
            // No-op compositions and decompositions
            noop_case(Unicode::Nfc),
            noop_case(Unicode::Nfd),
            noop_case(Unicode::Nfkc),
            noop_case(Unicode::Nfkd),
            // Composition
            Case {
                input: "I\u{307}ab",
                normalizer: Unicode::Nfc,
                expected: "İab",
                expected_offsets: [0, 0, 3, 4].into(),
            },
            // Canonical decomposition
            Case {
                input: "İa",
                normalizer: Unicode::Nfd,
                expected: "I\u{307}a",
                expected_offsets: [0, 0, 0, 2].into(),
            },
            // Compatible decomposition, followed by composition
            Case {
                input: "①",
                normalizer: Unicode::Nfkc,
                expected: "1",
                expected_offsets: [0].into(),
            },
            Case {
                input: "Éab",
                normalizer: Unicode::Nfkc,
                expected: "Éab",
                expected_offsets: [0, 0, 2, 3].into(),
            },
            // Compatible decomposition
            Case {
                input: "Éab",
                normalizer: Unicode::Nfkd,
                expected: "E\u{301}ab",
                expected_offsets: [0, 0, 0, 2, 3].into(),
            },
        ];

        cases.test_each(|case| {
            let Case {
                input,
                normalizer,
                expected,
                expected_offsets,
            } = case;

            let (normalized, offsets) = normalizer.normalize(input).unwrap();
            assert_eq!(normalized, *expected);
            assert_eq!(normalized.len(), offsets.len());
            assert_eq!(offsets, *expected_offsets);
        })
    }
}
