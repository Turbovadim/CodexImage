//! Pure text helpers shared by the editor: grapheme and word boundaries,
//! line ranges, UTF-16 offset conversion, and paste normalization.

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BoundaryClass {
    Whitespace,
    Word,
    Punctuation,
}

fn boundary_class(grapheme: &str) -> BoundaryClass {
    if grapheme.chars().all(char::is_whitespace) {
        BoundaryClass::Whitespace
    } else if grapheme
        .chars()
        .any(|character| character.is_alphanumeric() || character == '_')
    {
        BoundaryClass::Word
    } else {
        BoundaryClass::Punctuation
    }
}

pub fn previous_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .rev()
        .find_map(|(index, _)| (index < offset).then_some(index))
        .unwrap_or(0)
}

pub fn next_grapheme_boundary(text: &str, offset: usize) -> usize {
    text.grapheme_indices(true)
        .find_map(|(index, _)| (index > offset).then_some(index))
        .unwrap_or(text.len())
}

pub fn previous_word_boundary(text: &str, offset: usize) -> usize {
    let graphemes: Vec<_> = text
        .grapheme_indices(true)
        .take_while(|(index, _)| *index < offset)
        .collect();
    let mut index = graphemes.len();
    while index > 0 && boundary_class(graphemes[index - 1].1) == BoundaryClass::Whitespace {
        index -= 1;
    }
    if index == 0 {
        return 0;
    }
    let class = boundary_class(graphemes[index - 1].1);
    while index > 0 && boundary_class(graphemes[index - 1].1) == class {
        index -= 1;
    }
    graphemes.get(index).map_or(0, |(offset, _)| *offset)
}

pub fn next_word_boundary(text: &str, offset: usize) -> usize {
    let graphemes: Vec<_> = text
        .grapheme_indices(true)
        .filter(|(index, _)| *index >= offset)
        .collect();
    let mut index = 0;
    while index < graphemes.len() && boundary_class(graphemes[index].1) == BoundaryClass::Whitespace
    {
        index += 1;
    }
    if index == graphemes.len() {
        return text.len();
    }
    let class = boundary_class(graphemes[index].1);
    while index < graphemes.len() && boundary_class(graphemes[index].1) == class {
        index += 1;
    }
    graphemes
        .get(index)
        .map_or(text.len(), |(offset, _)| *offset)
}

pub fn word_range_at(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let offset = offset.min(text.len());
    for (start, segment) in text.split_word_bound_indices() {
        let end = start + segment.len();
        if (start..end).contains(&offset) || (offset == text.len() && end == text.len()) {
            return start..end;
        }
    }
    offset..offset
}

pub fn line_range_at(text: &str, offset: usize) -> Range<usize> {
    let offset = offset.min(text.len());
    let start = text[..offset]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |position| offset + position + 1);
    start..end
}

pub fn logical_line_edge(text: &str, offset: usize, end: bool) -> usize {
    let offset = offset.min(text.len());
    if end {
        text[offset..]
            .find('\n')
            .map_or(text.len(), |position| offset + position)
    } else {
        text[..offset]
            .rfind('\n')
            .map_or(0, |position| position + 1)
    }
}

pub fn normalize_inserted_text(text: &str, multiline: bool) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if multiline {
        normalized
    } else {
        normalized.replace('\n', " ")
    }
}

pub fn offset_from_utf16_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf16 >= offset {
            break;
        }
        utf16 += character.len_utf16();
        utf8 += character.len_utf8();
    }
    utf8
}

pub fn offset_to_utf16_in(text: &str, offset: usize) -> usize {
    let mut utf8 = 0;
    let mut utf16 = 0;
    for character in text.chars() {
        if utf8 >= offset {
            break;
        }
        utf8 += character.len_utf8();
        utf16 += character.len_utf16();
    }
    utf16
}

#[cfg(test)]
mod tests {
    use super::{
        line_range_at, logical_line_edge, next_grapheme_boundary, next_word_boundary,
        normalize_inserted_text, offset_from_utf16_in, offset_to_utf16_in,
        previous_grapheme_boundary, previous_word_boundary, word_range_at,
    };

    #[test]
    fn navigation_respects_graphemes_words_and_lines() {
        let text = "Hello, tall 👩🏽‍🎨 world\nsecond line";
        let emoji = text.find('👩').expect("emoji");
        assert_eq!(
            next_grapheme_boundary(text, emoji),
            text.find(" world").unwrap()
        );
        assert_eq!(
            previous_grapheme_boundary(text, text.find(" world").unwrap()),
            emoji
        );
        assert_eq!(
            previous_word_boundary(text, text.find("world").unwrap()),
            emoji
        );
        assert_eq!(
            next_word_boundary(text, text.find("world").unwrap()),
            text.find('\n').unwrap()
        );
        assert_eq!(word_range_at(text, 1), 0..5);
        assert_eq!(line_range_at(text, 2), 0..text.find('\n').unwrap() + 1);
        assert_eq!(
            logical_line_edge(text, text.len(), false),
            text.find('\n').unwrap() + 1
        );
    }

    #[test]
    fn single_line_paste_is_sanitized_without_breaking_multiline_text() {
        assert_eq!(
            normalize_inserted_text("one\r\ntwo\rthree", false),
            "one two three"
        );
        assert_eq!(
            normalize_inserted_text("one\r\ntwo\rthree", true),
            "one\ntwo\nthree"
        );
    }

    #[test]
    fn utf16_conversion_handles_non_bmp_input() {
        let text = "a👩🏽‍🎨b";
        for offset in text
            .char_indices()
            .map(|(offset, _)| offset)
            .chain([text.len()])
        {
            let utf16 = offset_to_utf16_in(text, offset);
            assert_eq!(offset_from_utf16_in(text, utf16), offset);
        }
    }
}
