//! Conversation-specific destination and emphasis rules. See LICENSE-DSH.
use icu_properties::{
    props::{GeneralCategory, GeneralCategoryGroup as Group, Script},
    script::ScriptWithExtensions,
    CodePointMapData,
};

fn in_group(c: char, group: Group) -> bool {
    group.contains(CodePointMapData::<GeneralCategory>::new().get(c))
}

fn cjk(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    let scripts = ScriptWithExtensions::new().get_script_extensions_val(c);
    [
        Script::Han,
        Script::Hiragana,
        Script::Katakana,
        Script::Hangul,
        Script::Bopomofo,
    ]
    .iter()
    .any(|script| scripts.contains(script))
}

pub(crate) fn cjk_punctuation(cjk_side: char, punctuation_side: char) -> bool {
    cjk(cjk_side) && in_group(punctuation_side, Group::Punctuation.union(Group::Symbol))
}

pub(crate) fn windows_path(raw: &str) -> bool {
    if !raw.contains('\\') {
        return false;
    }
    let has_scheme = raw.split_once(':').is_some_and(|(prefix, _)| {
        let bytes = prefix.as_bytes();
        bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1..]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(b))
    });
    !has_scheme
        && (!raw.contains('/')
            || raw
                .split('\\')
                .skip(1)
                .filter_map(|part| part.chars().next())
                .any(|c| in_group(c, Group::Letter.union(Group::Number))))
}
